//! The desktop camera portal (`org.freedesktop.portal.Camera`): the only way this app reaches a
//! camera. The portal asks the person for permission, and only after they agree hands back a
//! PipeWire connection that shows camera nodes and nothing else. Without a "yes" there is no
//! file descriptor, so there is nothing to open; no `/dev/video*` device is ever touched.
//!
//! It runs on the GTK main loop through GIO's D-Bus, which needs no Tokio runtime.
use gtk::{gio, glib, prelude::*};
use std::{os::fd::OwnedFd, time::Duration};

pub const DESTINATION: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const CAMERA: &str = "org.freedesktop.portal.Camera";
pub const REQUEST: &str = "org.freedesktop.portal.Request";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// Answers kept while waiting; a portal sends one per request, so this is only a bound.
const MAX_ANSWERS: usize = 64;

/// Why there is no camera connection. Each case is told to the person differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This desktop has no camera portal (or no session bus): nothing can be asked.
    PortalUnavailable,
    /// The portal is there but reports no camera.
    NoCamera,
    /// The person (or the system's policy) said no.
    Denied,
    /// The person closed the question without answering.
    Cancelled,
    /// Nobody answered the question in time.
    TimedOut,
    /// Anything else, with the portal's own words.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::PortalUnavailable => "This desktop does not offer camera access",
            Error::NoCamera => "No camera was found",
            Error::Denied => "Camera access was denied",
            Error::Cancelled => "Camera access was not given",
            Error::TimedOut => "The camera permission question was not answered",
            Error::Failed(text) => text,
        })
    }
}

/// How long to wait for each step.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// For a method call to be answered.
    pub call: Duration,
    /// For the person to answer the permission question.
    pub prompt: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self { call: Duration::from_secs(20), prompt: Duration::from_secs(300) }
    }
}

/// D-Bus errors that mean "this portal is not here" rather than "something went wrong".
const ABSENT: [&str; 5] = [
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
    "org.freedesktop.DBus.Error.UnknownMethod",
    "org.freedesktop.DBus.Error.UnknownObject",
    "org.freedesktop.DBus.Error.UnknownInterface",
];

/// `remote` is the D-Bus error name, if the failure came from the other side.
pub fn classify(remote: Option<&str>, message: &str) -> Error {
    match remote {
        Some(name) if ABSENT.contains(&name) => Error::PortalUnavailable,
        // The portal's frontend refuses a caller that its own policy excludes.
        Some("org.freedesktop.portal.Error.NotAllowed") => Error::Denied,
        _ => Error::Failed(crate::text_safety::visible(message, 200)),
    }
}
fn from_glib(error: &glib::Error) -> Error {
    let remote = gio::DBusError::remote_error(error);
    let mut stripped = error.clone();
    gio::DBusError::strip_remote_error(&mut stripped);
    classify(remote.as_deref(), stripped.message())
}

/// The path the portal gives a request: the caller's unique name with the leading ":" dropped and
/// "." made "_", then the token (XDG Desktop Portal, "Request" objects).
pub fn request_path(unique_name: &str, token: &str) -> String {
    let sender: String = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
}

/// A token no other request of ours uses. Only letters, digits and "_" are allowed.
pub fn new_token() -> String {
    format!("monogram_{}", uuid::Uuid::new_v4().simple())
}

/// What a request's `Response` code means: 0 success, 1 the person cancelled, anything else the
/// request ended another way (most often a refusal).
pub fn response(code: u32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        1 => Err(Error::Cancelled),
        _ => Err(Error::Denied),
    }
}

fn options(token: Option<&str>) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    if let Some(token) = token {
        dict.insert("handle_token", token);
    }
    // A tuple of one dictionary, `(a{sv})`. Writing `(dict.end(),).to_variant()` would box the
    // dictionary into a variant, `(v)`, which no portal accepts.
    glib::Variant::tuple_from_iter([dict.end()])
}

/// Ask for the camera. On success the file descriptor is a PipeWire connection restricted to
/// camera nodes; closing it ends the access.
pub async fn request_camera(connection: &gio::DBusConnection, timeouts: Timeouts) -> Result<OwnedFd, Error> {
    let call_ms = i32::try_from(timeouts.call.as_millis()).unwrap_or(i32::MAX);
    // A desktop without the portal answers "unknown service"; nothing is asked of the person.
    let present = connection
        .call_future(
            Some(DESTINATION),
            PATH,
            PROPERTIES,
            "Get",
            Some(&(CAMERA, "IsCameraPresent").to_variant()),
            Some(glib::VariantTy::new("(v)").unwrap()),
            gio::DBusCallFlags::NONE,
            call_ms,
        )
        .await
        .map_err(|error| from_glib(&error))?;
    let present = present
        .child_value(0)
        .as_variant()
        .and_then(|value| value.get::<bool>())
        .ok_or_else(|| Error::Failed("The camera portal gave an unexpected answer".into()))?;
    if !present {
        return Err(Error::NoCamera);
    }

    // Listen before asking, for every `Response` the portal sends us: the answer can arrive before
    // the call returns, and an old portal picks the request path itself, so the path is not known
    // until the reply. Each answer is kept with its path and matched afterwards.
    let token = new_token();
    let unique = connection.unique_name().ok_or(Error::PortalUnavailable)?;
    let expected = request_path(&unique, &token);
    let seen: std::rc::Rc<std::cell::RefCell<Vec<(String, u32)>>> = Default::default();
    let wake = std::rc::Rc::new(tokio::sync::Notify::new());
    let _listening = {
        let (seen, wake) = (seen.clone(), wake.clone());
        connection.subscribe_to_signal(
            // Only the portal itself: another program on the bus cannot answer for it.
            Some(DESTINATION),
            Some(REQUEST),
            Some("Response"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let code = signal.parameters.child_value(0).get::<u32>().unwrap_or(2);
                let mut seen = seen.borrow_mut();
                if seen.len() < MAX_ANSWERS {
                    seen.push((signal.object_path.to_owned(), code));
                }
                wake.notify_one();
            },
        )
    };
    let handle = connection
        .call_future(
            Some(DESTINATION),
            PATH,
            CAMERA,
            "AccessCamera",
            Some(&options(Some(&token))),
            Some(glib::VariantTy::new("(o)").unwrap()),
            gio::DBusCallFlags::NONE,
            call_ms,
        )
        .await
        .map_err(|error| from_glib(&error))?;
    let actual = handle.child_value(0).str().map(str::to_owned).unwrap_or_else(|| expected.clone());
    let answered = async {
        loop {
            let found = seen
                .borrow()
                .iter()
                .find(|(path, _)| *path == actual || *path == expected)
                .map(|(_, code)| *code);
            if let Some(code) = found {
                return code;
            }
            wake.notified().await;
        }
    };
    let prompt = glib::timeout_future(timeouts.prompt);
    futures_util::pin_mut!(answered);
    let code = match futures_util::future::select(answered, prompt).await {
        futures_util::future::Either::Left((code, _)) => code,
        futures_util::future::Either::Right(_) => return Err(Error::TimedOut),
    };
    response(code)?;

    let (reply, fds) = connection
        .call_with_unix_fd_list_future(
            Some(DESTINATION),
            PATH,
            CAMERA,
            "OpenPipeWireRemote",
            Some(&options(None)),
            Some(glib::VariantTy::new("(h)").unwrap()),
            gio::DBusCallFlags::NONE,
            call_ms,
            gio::UnixFDList::NONE,
        )
        .await
        .map_err(|error| from_glib(&error))?;
    let index = reply
        .child_value(0)
        .get::<glib::variant::Handle>()
        .map(|handle| handle.0)
        .ok_or_else(|| Error::Failed("The camera portal gave an unexpected answer".into()))?;
    fds.ok_or_else(|| Error::Failed("The camera portal sent no connection".into()))?
        .get(index)
        .map_err(|error| Error::Failed(crate::text_safety::visible(error.message(), 200)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_path_follows_the_portal_rules() {
        assert_eq!(
            request_path(":1.42", "tok"),
            "/org/freedesktop/portal/desktop/request/1_42/tok"
        );
        assert_eq!(
            request_path(":1.2.3", "a_b"),
            "/org/freedesktop/portal/desktop/request/1_2_3/a_b"
        );
        assert!(request_path("1.7", "t").ends_with("/1_7/t"), "no leading colon to drop");
    }

    #[test]
    fn tokens_are_unique_and_only_use_the_allowed_characters() {
        let tokens: std::collections::HashSet<_> = (0..200).map(|_| new_token()).collect();
        assert_eq!(tokens.len(), 200);
        for token in &tokens {
            assert!(token.starts_with("monogram_") && token.len() > 20);
            assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "{token}");
        }
    }

    #[test]
    fn response_codes_map_to_granted_cancelled_or_denied() {
        assert_eq!(response(0), Ok(()));
        assert_eq!(response(1), Err(Error::Cancelled));
        for other in [2, 3, 255, u32::MAX] {
            assert_eq!(response(other), Err(Error::Denied), "{other}");
        }
    }

    #[test]
    fn missing_portals_are_told_apart_from_failures_and_messages_are_cleaned() {
        for name in ABSENT {
            assert_eq!(classify(Some(name), "x"), Error::PortalUnavailable, "{name}");
        }
        assert_eq!(classify(Some("org.freedesktop.portal.Error.NotAllowed"), "x"), Error::Denied);
        assert_eq!(classify(Some("org.freedesktop.DBus.Error.AccessDenied"), "no"), Error::Failed("no".into()));
        assert_eq!(classify(None, "plain"), Error::Failed("plain".into()));
        // The portal's own text is not trusted to be tidy.
        let Error::Failed(text) = classify(None, "a\u{202e}b\n\nc") else { panic!() };
        assert_eq!(text, "ab c");
        let Error::Failed(long) = classify(None, &"x".repeat(1000)) else { panic!() };
        assert_eq!(long.chars().count(), 200);
    }

    #[test]
    fn the_request_options_are_a_plain_dictionary_tuple_not_a_boxed_one() {
        assert_eq!(options(None).type_().as_str(), "(a{sv})");
        let with_token = options(Some("monogram_abc"));
        assert_eq!(with_token.type_().as_str(), "(a{sv})");
        let dict = glib::VariantDict::new(Some(&with_token.child_value(0)));
        assert_eq!(dict.lookup::<String>("handle_token").unwrap().as_deref(), Some("monogram_abc"));
    }

    #[test]
    fn every_error_has_a_plain_sentence() {
        for error in [
            Error::PortalUnavailable,
            Error::NoCamera,
            Error::Denied,
            Error::Cancelled,
            Error::TimedOut,
            Error::Failed("because".into()),
        ] {
            let text = error.to_string();
            assert!(!text.is_empty() && !text.ends_with('.'), "{text}");
        }
        assert_eq!(Error::Failed("because".into()).to_string(), "because");
    }

    // ---- against a fake portal on a private bus -----------------------------------------

    use super::super::fake::{client, Behavior, FakePortal};
    use std::{io::{Read, Write}, os::fd::AsRawFd, time::Instant};

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        glib::MainContext::default().block_on(future)
    }
    const PRIVATE: &str = "requires a private session bus (tools/headless-gtk-test.sh)";
    fn quick() -> Timeouts {
        Timeouts { call: Duration::from_secs(5), prompt: Duration::from_millis(600) }
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_yes_gives_a_working_descriptor_after_asking_in_the_right_order() {
        run(async {
            let fake = FakePortal::start(Behavior::default()).await;
            let fd = request_camera(&client().await, Timeouts::default()).await.unwrap();
            assert_eq!(*fake.calls.borrow(), ["AccessCamera", "OpenPipeWireRemote"], "asked, and only then connected");
            // The descriptor is the connection the portal made, not something else.
            let mut near = std::os::unix::net::UnixStream::from(fd);
            near.write_all(b"hello").unwrap();
            let mut far = fake.peers.borrow()[0].try_clone().unwrap();
            let mut got = [0u8; 5];
            far.read_exact(&mut got).unwrap();
            assert_eq!(&got, b"hello");
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn the_descriptor_is_not_inherited_by_child_processes() {
        run(async {
            let _fake = FakePortal::start(Behavior::default()).await;
            let fd = request_camera(&client().await, Timeouts::default()).await.unwrap();
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "close-on-exec is set");
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_no_never_opens_a_connection() {
        run(async {
            for (code, want) in [(2u32, Error::Denied), (1, Error::Cancelled), (3, Error::Denied), (99, Error::Denied)] {
                let fake = FakePortal::start(Behavior { response: code, ..Default::default() }).await;
                let result = request_camera(&client().await, Timeouts::default()).await;
                assert_eq!(result.err(), Some(want), "response {code}");
                assert_eq!(fake.called("AccessCamera"), 1);
                assert_eq!(fake.called("OpenPipeWireRemote"), 0, "permission refused: no connection requested ({code})");
                assert!(fake.peers.borrow().is_empty(), "and none made");
            }
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn without_a_camera_nobody_is_asked_anything() {
        run(async {
            let fake = FakePortal::start(Behavior { present: false, ..Default::default() }).await;
            let result = request_camera(&client().await, Timeouts::default()).await;
            assert_eq!(result.err(), Some(Error::NoCamera));
            assert_eq!(fake.called("AccessCamera"), 0, "no permission question for a camera that is not there");
            assert_eq!(fake.called("OpenPipeWireRemote"), 0);
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_desktop_without_the_portal_is_reported_as_such_quickly() {
        run(async {
            let started = Instant::now();
            let result = request_camera(&client().await, Timeouts::default()).await;
            assert_eq!(result.err(), Some(Error::PortalUnavailable));
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn portal_errors_are_sorted_into_denied_and_failed() {
        run(async {
            for (name, want) in [
                ("org.freedesktop.portal.Error.NotAllowed", Error::Denied),
                ("org.freedesktop.DBus.Error.AccessDenied", Error::Failed("refused by the fake portal".into())),
                ("org.freedesktop.portal.Error.Failed", Error::Failed("refused by the fake portal".into())),
            ] {
                let fake = FakePortal::start(Behavior { access_error: Some(name), ..Default::default() }).await;
                let result = request_camera(&client().await, Timeouts::default()).await;
                assert_eq!(result.err(), Some(want), "{name}");
                assert_eq!(fake.called("OpenPipeWireRemote"), 0, "{name}");
            }
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn an_answer_that_arrives_before_the_call_returns_is_not_missed() {
        run(async {
            let fake = FakePortal::start(Behavior { respond_first: true, ..Default::default() }).await;
            assert!(request_camera(&client().await, Timeouts::default()).await.is_ok());
            // The answer being "no" in that order is understood as well.
            fake.set(|b| b.response = 2);
            assert_eq!(request_camera(&client().await, Timeouts::default()).await.err(), Some(Error::Denied));
            assert_eq!(fake.called("OpenPipeWireRemote"), 1, "only the first yes opened a connection");
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_portal_that_chooses_its_own_request_path_is_still_understood() {
        run(async {
            let fake = FakePortal::start(Behavior { other_handle: true, ..Default::default() }).await;
            assert!(request_camera(&client().await, Timeouts::default()).await.is_ok());
            fake.set(|b| b.response = 2);
            assert_eq!(request_camera(&client().await, Timeouts::default()).await.err(), Some(Error::Denied));
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn nobody_answering_ends_in_a_time_out_and_no_connection() {
        run(async {
            let fake = FakePortal::start(Behavior { silent: true, ..Default::default() }).await;
            let started = Instant::now();
            let result = request_camera(&client().await, quick()).await;
            assert_eq!(result.err(), Some(Error::TimedOut));
            assert!(started.elapsed() >= Duration::from_millis(500) && started.elapsed() < Duration::from_secs(3));
            assert_eq!(fake.called("OpenPipeWireRemote"), 0);
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_person_who_takes_their_time_is_waited_for() {
        run(async {
            let _fake = FakePortal::start(Behavior { delay_ms: 300, ..Default::default() }).await;
            let started = Instant::now();
            assert!(request_camera(&client().await, quick()).await.is_ok());
            assert!(started.elapsed() >= Duration::from_millis(250), "it did wait: {:?}", started.elapsed());
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn an_answer_from_anyone_but_the_portal_is_ignored() {
        run(async {
            // Another program on the bus claims "granted" on the request path first; the portal says no.
            let fake = FakePortal::start(Behavior { spoof: true, response: 2, ..Default::default() }).await;
            let result = request_camera(&client().await, Timeouts::default()).await;
            assert_eq!(result.err(), Some(Error::Denied), "the spoofed yes was not believed");
            assert_eq!(fake.called("OpenPipeWireRemote"), 0);
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn a_failed_or_empty_connection_step_is_an_error_not_a_crash() {
        run(async {
            let fake = FakePortal::start(Behavior { open_error: Some("org.freedesktop.portal.Error.Failed"), ..Default::default() }).await;
            assert!(matches!(request_camera(&client().await, Timeouts::default()).await, Err(Error::Failed(_))));
            fake.set(|b| {
                b.open_error = None;
                b.no_fd = true;
            });
            assert!(matches!(request_camera(&client().await, Timeouts::default()).await, Err(Error::Failed(_))));
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn two_requests_at_once_and_one_after_another_each_get_their_own_connection() {
        run(async {
            let fake = FakePortal::start(Behavior { delay_ms: 50, ..Default::default() }).await;
            let connection = client().await;
            let (a, b) = futures_util::future::join(
                request_camera(&connection, Timeouts::default()),
                request_camera(&connection, Timeouts::default()),
            )
            .await;
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_ne!(a.as_raw_fd(), b.as_raw_fd());
            let c = request_camera(&connection, Timeouts::default()).await.unwrap();
            assert_eq!(fake.called("AccessCamera"), 3);
            assert_eq!(fake.peers.borrow().len(), 3, "three separate connections");
            // The first is still alive after the others were made.
            let mut first = std::os::unix::net::UnixStream::from(a);
            first.write_all(b"x").unwrap();
            drop((b, c));
        });
    }

    #[test]
    #[ignore = "requires a private session bus (tools/headless-gtk-test.sh)"]
    fn the_fake_portal_refuses_to_run_on_anything_but_a_private_bus() {
        let real = "unix:path=/run/user/1000/bus";
        let previous = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
        // SAFETY: this test runs alone in its process.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", real) };
        let caught = std::panic::catch_unwind(super::super::fake::private_bus_address);
        match previous {
            Some(value) => unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value) },
            None => unsafe { std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") },
        }
        assert!(caught.is_err(), "{PRIVATE}");
    }
}
