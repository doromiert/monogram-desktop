//! A stand-in for `org.freedesktop.portal.Desktop`, for tests. It owns the portal's name on a
//! *private* session bus (see `tools/headless-gtk-test.sh`) and answers the Camera interface the
//! way a real portal would, including its less polite moods.
//!
//! These tests must never reach the real session bus: there they could ask the person for the
//! camera. [`private_bus_address`] refuses to continue unless the bus is a throw-away one.
use super::portal::{self, CAMERA, DESTINATION, PATH, REQUEST};
use gtk::{gio, glib, prelude::*};
use std::{cell::RefCell, os::unix::net::UnixStream, rc::Rc};

const XML: &str = r#"<node><interface name='org.freedesktop.portal.Camera'>
  <method name='AccessCamera'><arg type='a{sv}' name='options' direction='in'/><arg type='o' name='handle' direction='out'/></method>
  <method name='OpenPipeWireRemote'><arg type='a{sv}' name='options' direction='in'/><arg type='h' name='fd' direction='out'/></method>
  <property name='IsCameraPresent' type='b' access='read'/>
  <property name='version' type='u' access='read'/>
</interface></node>"#;

/// How the fake portal behaves. The default is a friendly portal with a camera that says yes.
#[derive(Clone, Debug)]
pub struct Behavior {
    pub present: bool,
    /// The `Response` code: 0 granted, 1 cancelled, 2 denied.
    pub response: u32,
    /// Fail `AccessCamera` with this D-Bus error name instead of answering.
    pub access_error: Option<&'static str>,
    /// Fail `OpenPipeWireRemote` with this D-Bus error name.
    pub open_error: Option<&'static str>,
    /// Send `Response` before the method reply instead of after it.
    pub respond_first: bool,
    /// Like portals before 0.9: return a request path other than the one asked for.
    pub other_handle: bool,
    /// Never send `Response`.
    pub silent: bool,
    /// Wait this long before `Response`.
    pub delay_ms: u32,
    /// Reply to `OpenPipeWireRemote` without a descriptor.
    pub no_fd: bool,
    /// Before the real answer, have an unrelated bus name send "granted" on the request path.
    pub spoof: bool,
}
impl Default for Behavior {
    fn default() -> Self {
        Self {
            present: true,
            response: 0,
            access_error: None,
            open_error: None,
            respond_first: false,
            other_handle: false,
            silent: false,
            delay_ms: 0,
            no_fd: false,
            spoof: false,
        }
    }
}

pub struct FakePortal {
    connection: gio::DBusConnection,
    registration: Option<gio::RegistrationId>,
    owner: Option<gio::OwnerId>,
    pub behavior: Rc<RefCell<Behavior>>,
    /// The methods called, in order (`Get`/`GetAll` of properties are not listed).
    pub calls: Rc<RefCell<Vec<String>>>,
    /// The far end of each descriptor handed out.
    pub peers: Rc<RefCell<Vec<UnixStream>>>,
}

/// The private bus's address, or a panic: this is the guard that keeps the tests away from the
/// real session bus.
pub fn private_bus_address() -> String {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .expect("run these tests through tools/headless-gtk-test.sh: they need a private session bus");
    assert!(
        !address.contains("/run/user/") && !address.is_empty(),
        "refusing to use what looks like the real session bus ({address}): the fake portal must only run on a private one"
    );
    address
}

async fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_future(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
    )
    .await
    .expect("connect to the private bus")
}

/// The app's own connection, to the same private bus.
pub async fn client() -> gio::DBusConnection {
    private_bus_address();
    gio::bus_get_future(gio::BusType::Session).await.expect("session bus")
}

impl FakePortal {
    pub async fn start(behavior: Behavior) -> Self {
        let address = private_bus_address();
        let connection = connect(&address).await;
        let attacker = connect(&address).await;
        let behavior = Rc::new(RefCell::new(behavior));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let peers = Rc::new(RefCell::new(Vec::new()));
        let info = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let interface = info.lookup_interface(CAMERA).unwrap();
        let (b, c, p) = (behavior.clone(), calls.clone(), peers.clone());
        let registration = connection
            .register_object(PATH, &interface)
            .method_call(move |connection, sender, _path, _interface, method, params, invocation| {
                c.borrow_mut().push(method.to_owned());
                let behavior = b.borrow().clone();
                match method {
                    "AccessCamera" => {
                        if let Some(name) = behavior.access_error {
                            invocation.return_dbus_error(name, "refused by the fake portal");
                            return;
                        }
                        let options = glib::VariantDict::new(Some(&params.child_value(0)));
                        let token = options.lookup::<String>("handle_token").ok().flatten().unwrap_or_default();
                        let asked = portal::request_path(sender.unwrap_or(""), &token);
                        let handle = if behavior.other_handle {
                            "/org/freedesktop/portal/desktop/request/elsewhere/chosen_by_portal".to_owned()
                        } else {
                            asked.clone()
                        };
                        let (respond_first, delay_ms) = (behavior.respond_first, behavior.delay_ms);
                        let emit = {
                            let (connection, handle, sender) = (connection.clone(), handle.clone(), sender.map(str::to_owned));
                            let (attacker, asked) = (attacker.clone(), asked.clone());
                            move || {
                                if behavior.spoof {
                                    let _ = attacker.emit_signal(
                                        None,
                                        &asked,
                                        REQUEST,
                                        "Response",
                                        Some(&glib::Variant::tuple_from_iter([0u32.to_variant(), glib::VariantDict::new(None).end()])),
                                    );
                                }
                                if !behavior.silent {
                                    let _ = connection.emit_signal(
                                        sender.as_deref(),
                                        &handle,
                                        REQUEST,
                                        "Response",
                                        Some(&glib::Variant::tuple_from_iter([behavior.response.to_variant(), glib::VariantDict::new(None).end()])),
                                    );
                                }
                            }
                        };
                        let path = glib::variant::ObjectPath::try_from(handle).unwrap();
                        if respond_first {
                            emit();
                            invocation.return_value(Some(&(path,).to_variant()));
                        } else {
                            invocation.return_value(Some(&(path,).to_variant()));
                            let delay = std::time::Duration::from_millis(u64::from(delay_ms));
                            glib::timeout_add_local_once(delay, emit);
                        }
                    }
                    "OpenPipeWireRemote" => {
                        if let Some(name) = behavior.open_error {
                            invocation.return_dbus_error(name, "refused by the fake portal");
                            return;
                        }
                        if behavior.no_fd {
                            invocation.return_value(Some(&(glib::variant::Handle(0),).to_variant()));
                            return;
                        }
                        let (near, far) = UnixStream::pair().unwrap();
                        p.borrow_mut().push(far);
                        let list = gio::UnixFDList::from_array([std::os::fd::OwnedFd::from(near)]);
                        invocation.return_value_with_unix_fd_list(Some(&(glib::variant::Handle(0),).to_variant()), Some(&list));
                    }
                    other => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", other),
                }
            })
            .property({
                let b = behavior.clone();
                move |_, _, _, _, name| match name {
                    "IsCameraPresent" => b.borrow().present.to_variant(),
                    _ => 1u32.to_variant(),
                }
            })
            .build()
            .expect("register the fake portal");
        let (acquired, waiting) = tokio::sync::oneshot::channel();
        let acquired = Rc::new(RefCell::new(Some(acquired)));
        let owner = gio::bus_own_name_on_connection(
            &connection,
            DESTINATION,
            gio::BusNameOwnerFlags::NONE,
            move |_, _| {
                if let Some(done) = acquired.borrow_mut().take() {
                    let _ = done.send(());
                }
            },
            |_, _| panic!("the fake portal could not take the portal's name on the private bus"),
        );
        waiting.await.expect("the name is acquired");
        Self { connection, registration: Some(registration), owner: Some(owner), behavior, calls, peers }
    }
    pub fn set(&self, change: impl FnOnce(&mut Behavior)) {
        change(&mut self.behavior.borrow_mut());
    }
    pub fn called(&self, method: &str) -> usize {
        self.calls.borrow().iter().filter(|call| *call == method).count()
    }
}

impl Drop for FakePortal {
    fn drop(&mut self) {
        if let Some(registration) = self.registration.take() {
            let _ = self.connection.unregister_object(registration);
        }
        if let Some(owner) = self.owner.take() {
            gio::bus_unown_name(owner);
        }
    }
}
