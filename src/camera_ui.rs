//! "Take Photo": ask the desktop for the camera, show what it sees, add the picture to the message.
//!
//! The camera is only ever opened after a person chooses Take Photo… and the desktop portal has
//! been told to ask them (see `camera::portal`). Every other outcome gets its own page that says
//! what happened and what can be done about it. Closing the dialog stops the stream, which ends
//! the portal's access (and the system's camera indicator).
use crate::{
    camera::{
        frame::Frame,
        portal::{self, Error, Timeouts},
        stream::{Event, Source, Stream},
    },
    ui::Ui,
    widgets::bx,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};

/// A photo is kept as a JPEG of this quality.
const JPEG_QUALITY: u8 = 90;

/// What the dialog shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Asking,
    Live,
    Denied,
    NotGiven,
    NoCamera,
    Unavailable,
    NoAnswer,
    Problem,
}

impl Page {
    fn name(self) -> &'static str {
        match self {
            Page::Asking => "asking",
            Page::Live => "live",
            Page::Denied => "denied",
            Page::NotGiven => "not-given",
            Page::NoCamera => "no-camera",
            Page::Unavailable => "unavailable",
            Page::NoAnswer => "no-answer",
            Page::Problem => "problem",
        }
    }
    /// The page for a reason the camera is not available.
    pub fn for_error(error: &Error) -> Page {
        match error {
            Error::Denied => Page::Denied,
            Error::Cancelled => Page::NotGiven,
            Error::NoCamera => Page::NoCamera,
            Error::PortalUnavailable => Page::Unavailable,
            Error::TimedOut => Page::NoAnswer,
            Error::Failed(_) => Page::Problem,
        }
    }
}

/// A name for a photo that sorts by time and cannot collide with another.
pub fn photo_name(now: chrono::DateTime<chrono::Local>, unique: &str) -> String {
    format!("camera-{}-{}.jpg", now.format("%Y%m%d-%H%M%S"), &unique[..unique.len().min(6)])
}

struct Inner {
    stack: gtk::Stack,
    picture: gtk::Picture,
    capture: gtk::Button,
    detail: adw::StatusPage,
    /// "Try Again" on the reasons page; hidden when trying again cannot help.
    retry: gtk::Button,
    stream: RefCell<Option<Stream>>,
    latest: RefCell<Option<Frame>>,
    /// Bumped on every start and on close: an answer for an older one is ignored.
    generation: std::cell::Cell<u64>,
    busy: std::cell::Cell<bool>,
    closed: std::cell::Cell<bool>,
    page: std::cell::Cell<Page>,
}

fn status(icon: &str, title: &str, description: &str, retry: Option<&gtk::Button>) -> adw::StatusPage {
    let page = adw::StatusPage::builder().icon_name(icon).title(title).description(description).vexpand(true).build();
    if let Some(retry) = retry {
        page.set_child(Some(retry));
    }
    page
}

fn retry_button() -> gtk::Button {
    let retry = gtk::Button::with_label("Try Again");
    retry.add_css_class("suggested-action");
    retry.add_css_class("pill");
    retry.set_halign(gtk::Align::Center);
    retry
}

impl Ui {
    /// Offer the camera: ask the desktop, then show the live picture.
    pub fn open_camera(self: &Rc<Self>) {
        if self.camera_dialog.borrow().as_ref().and_then(|dialog| dialog.upgrade()).is_some() {
            return;
        }
        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();
        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).vexpand(true).hexpand(true).build();
        picture.update_property(&[gtk::accessible::Property::Label("Camera preview")]);
        let capture = gtk::Button::with_label("Take Photo");
        capture.add_css_class("suggested-action");
        capture.add_css_class("pill");
        capture.set_halign(gtk::Align::Center);
        capture.set_margin_bottom(12);
        capture.set_sensitive(false);
        capture.set_tooltip_text(Some("Waiting for the first picture"));

        let waiting = bx(true, 12);
        waiting.set_valign(gtk::Align::Center);
        waiting.set_halign(gtk::Align::Center);
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        waiting.append(&spinner);
        waiting.append(&crate::widgets::label("Waiting for Permission", "title-3"));
        waiting.append(&crate::widgets::wrap("Choose Allow when your desktop asks about the camera.", "dim-label"));
        let live = bx(true, 0);
        live.append(&picture);
        live.append(&capture);
        let (retry_denied, retry_reason) = (retry_button(), retry_button());
        let detail = status("dialog-warning-symbolic", "Camera Problem", "", Some(&retry_reason));
        stack.add_named(&waiting, Some(Page::Asking.name()));
        stack.add_named(&live, Some(Page::Live.name()));
        stack.add_named(
            &status(
                "camera-disabled-symbolic",
                "Camera Access Denied",
                "Monogram is not allowed to use the camera. Allow it in your desktop’s privacy settings, then try again.",
                Some(&retry_denied),
            ),
            Some(Page::Denied.name()),
        );
        stack.add_named(&detail, Some("reason"));
        let inner = Rc::new(Inner {
            stack: stack.clone(),
            picture,
            capture: capture.clone(),
            detail,
            retry: retry_reason.clone(),
            stream: RefCell::new(None),
            latest: RefCell::new(None),
            generation: Default::default(),
            busy: Default::default(),
            closed: Default::default(),
            page: std::cell::Cell::new(Page::Asking),
        });
        let (dialog, _header) = self.dialog("Take Photo", &stack, 640, 520);
        dialog.set_default_widget(Some(&capture));
        self.camera_dialog.replace(Some(dialog.downgrade()));

        for button in [&retry_denied, &retry_reason] {
            let (weak_ui, weak_inner) = (Rc::downgrade(self), Rc::downgrade(&inner));
            button.connect_clicked(move |_| {
                if let (Some(ui), Some(inner)) = (weak_ui.upgrade(), weak_inner.upgrade()) {
                    start(&ui, &inner);
                }
            });
        }
        let (weak_ui, weak_inner) = (Rc::downgrade(self), Rc::downgrade(&inner));
        capture.connect_clicked(move |_| {
            if let (Some(ui), Some(inner)) = (weak_ui.upgrade(), weak_inner.upgrade()) {
                take_photo(&ui, &inner);
            }
        });
        // The dialog owns the state; closing it stops the camera and lets go of the portal's
        // connection, which ends the system's camera indicator.
        let owned = inner.clone();
        let weak_ui = Rc::downgrade(self);
        dialog.connect_closed(move |closing| {
            owned.closed.set(true);
            owned.generation.set(owned.generation.get() + 1);
            owned.stream.borrow_mut().take();
            // Forget it, so Take Photo… opens a fresh one; but not a newer dialog's record.
            if let Some(ui) = weak_ui.upgrade() {
                let current = ui.camera_dialog.borrow().as_ref().and_then(|dialog| dialog.upgrade());
                if current.is_none_or(|current| &current == closing) {
                    ui.camera_dialog.replace(None);
                }
            }
        });
        start(self, &inner);
    }
}

/// The page for a reason, with words that say what to do about it.
fn show_reason(inner: &Inner, page: Page, error: Option<&Error>) {
    let (icon, title, text): (&str, &str, String) = match page {
        Page::NotGiven => ("camera-disabled-symbolic", "Camera Access Not Given", "The permission question was closed without an answer. Try again and choose Allow to use the camera.".into()),
        Page::NoCamera => ("camera-web-symbolic", "No Camera Found", "Connect a camera and try again.".into()),
        Page::Unavailable => ("camera-disabled-symbolic", "Camera Not Available", "This desktop does not offer cameras to apps. A desktop portal with camera support is needed.".into()),
        Page::NoAnswer => ("camera-disabled-symbolic", "No Answer", "The permission question was not answered in time. Try again.".into()),
        _ => (
            "dialog-warning-symbolic",
            "Camera Problem",
            error.map(|error| error.to_string()).unwrap_or_else(|| "The camera stopped working.".into()),
        ),
    };
    inner.detail.set_icon_name(Some(icon));
    inner.detail.set_title(title);
    inner.detail.set_description(Some(&text));
    // A desktop with no camera portal will not grow one by asking again.
    inner.retry.set_visible(page != Page::Unavailable);
    inner.stack.set_visible_child_name("reason");
    inner.page.set(page);
}

fn show(inner: &Inner, page: Page) {
    inner.page.set(page);
    inner.stack.set_visible_child_name(page.name());
}

/// Ask, then stream. Calling it again after a failure is the "Try Again" button.
fn start(ui: &Rc<Ui>, inner: &Rc<Inner>) {
    inner.stream.borrow_mut().take();
    inner.latest.borrow_mut().take();
    inner.capture.set_sensitive(false);
    inner.generation.set(inner.generation.get() + 1);
    let generation = inner.generation.get();
    show(inner, Page::Asking);
    let (weak_ui, weak) = (Rc::downgrade(ui), Rc::downgrade(inner));
    glib::spawn_future_local(async move {
        let outcome = async {
            let connection = gio::bus_get_future(gio::BusType::Session).await.map_err(|_| Error::PortalUnavailable)?;
            portal::request_camera(&connection, Timeouts::default()).await
        }
        .await;
        let (Some(_ui), Some(inner)) = (weak_ui.upgrade(), weak.upgrade()) else { return };
        if inner.closed.get() || inner.generation.get() != generation {
            // Closed or restarted while the question was open: the connection is simply dropped.
            return;
        }
        let fd = match outcome {
            Ok(fd) => fd,
            Err(error) => {
                match Page::for_error(&error) {
                    // Refusal has a page of its own, with the way to change it.
                    Page::Denied => show(&inner, Page::Denied),
                    page => show_reason(&inner, page, Some(&error)),
                }
                return;
            }
        };
        let source = match test_source() {
            Some(description) => Source::Description(description),
            None => Source::PipeWire(fd),
        };
        let (stream, mut events) = match Stream::start(source) {
            Ok(started) => started,
            Err(message) => {
                let error = Error::Failed(crate::text_safety::visible(&message, 200));
                show_reason(&inner, Page::Problem, Some(&error));
                return;
            }
        };
        inner.stream.replace(Some(stream));
        show(&inner, Page::Live);
        while let Some(event) = events.recv().await {
            if inner.closed.get() || inner.generation.get() != generation {
                return;
            }
            match event {
                Event::Frame(frame) => {
                    inner.picture.set_paintable(Some(&frame.texture()));
                    inner.latest.replace(Some(frame));
                    if !inner.busy.get() {
                        inner.capture.set_sensitive(true);
                        inner.capture.set_tooltip_text(None);
                    }
                }
                Event::Failed(message) => {
                    inner.stream.borrow_mut().take();
                    let error = Error::Failed(crate::text_safety::visible(&message, 200));
                    show_reason(&inner, Page::Problem, Some(&error));
                    return;
                }
                Event::Ended => {
                    inner.stream.borrow_mut().take();
                    show_reason(&inner, Page::Problem, None);
                    return;
                }
            }
        }
    });
}

/// Keep the current picture: encode it off the main loop, save it privately, attach it.
fn take_photo(ui: &Rc<Ui>, inner: &Rc<Inner>) {
    let Some(frame) = inner.latest.borrow().clone() else { return };
    let Some(dir) = crate::picture_editor_ui::temporary_copies_dir() else {
        ui.notice("The photo could not be saved");
        return;
    };
    inner.busy.set(true);
    inner.capture.set_sensitive(false);
    let name = photo_name(chrono::Local::now(), &uuid::Uuid::new_v4().simple().to_string());
    let path = dir.join(&name);
    let target = path.clone();
    let saved = gio::spawn_blocking(move || -> Result<(), String> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let jpeg = frame.to_jpeg(JPEG_QUALITY).map_err(|error| error.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .map_err(|error| error.to_string())?;
        file.write_all(&jpeg).map_err(|error| error.to_string())
    });
    let (weak_ui, weak) = (Rc::downgrade(ui), Rc::downgrade(inner));
    glib::spawn_future_local(async move {
        let result = saved.await;
        let Some(ui) = weak_ui.upgrade() else {
            crate::picture_editor_ui::discard_edited(Some(path));
            return;
        };
        let ok = matches!(result, Ok(Ok(())));
        if let Some(inner) = weak.upgrade() {
            inner.busy.set(false);
            inner.capture.set_sensitive(inner.latest.borrow().is_some());
        }
        if !ok {
            crate::picture_editor_ui::discard_edited(Some(path));
            ui.notice("The photo could not be saved");
            return;
        }
        // Stop the camera first: the picture is taken, there is no reason to keep it on.
        // Closing clears the record of the open dialog, so the borrow must be over before it.
        let open = ui.camera_dialog.borrow().as_ref().and_then(|dialog| dialog.upgrade());
        if let Some(dialog) = open {
            dialog.close();
        }
        if ui.stage_attachment_files(vec![gio::File::for_path(&path)]) {
            ui.notice("Photo added to your message");
        } else {
            crate::picture_editor_ui::discard_edited(Some(path));
            ui.notice("The photo could not be added to your message");
        }
    });
}

#[cfg(test)]
thread_local! {
    /// Tests give a GStreamer test source here; the portal is still asked first.
    pub(crate) static TEST_SOURCE: RefCell<Option<String>> = const { RefCell::new(None) };
}
#[cfg(test)]
fn test_source() -> Option<String> {
    TEST_SOURCE.with(|source| source.borrow().clone())
}
#[cfg(not(test))]
fn test_source() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_page_and_the_pages_have_distinct_names() {
        let reasons = [
            (Error::Denied, Page::Denied),
            (Error::Cancelled, Page::NotGiven),
            (Error::NoCamera, Page::NoCamera),
            (Error::PortalUnavailable, Page::Unavailable),
            (Error::TimedOut, Page::NoAnswer),
            (Error::Failed("x".into()), Page::Problem),
        ];
        for (error, page) in &reasons {
            assert_eq!(Page::for_error(error), *page, "{error:?}");
        }
        let names: std::collections::HashSet<_> = [
            Page::Asking,
            Page::Live,
            Page::Denied,
            Page::NotGiven,
            Page::NoCamera,
            Page::Unavailable,
            Page::NoAnswer,
            Page::Problem,
        ]
        .map(Page::name)
        .into_iter()
        .collect();
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn photo_names_sort_by_time_and_are_safe_file_names() {
        use chrono::TimeZone;
        let early = chrono::Local.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        let late = chrono::Local.with_ymd_and_hms(2025, 11, 22, 19, 30, 59).unwrap();
        let (a, b) = (photo_name(early, "abcdef123"), photo_name(late, "abcdef123"));
        assert_eq!(a, "camera-20250102-030405-abcdef.jpg");
        assert!(a < b, "later names sort later");
        assert_ne!(photo_name(early, "aaaaaa"), photo_name(early, "bbbbbb"), "two photos in one second differ");
        assert_eq!(photo_name(early, "ab"), "camera-20250102-030405-ab.jpg", "short ids are fine");
        for name in [&a, &b] {
            assert!(name.ends_with(".jpg") && !name.contains(['/', ' ', '\\', ':']), "{name}");
            assert!(crate::picture_editor_ui::editable(name), "the picture editor accepts it");
        }
    }

    // ---- the dialog against a fake portal on a private bus -------------------------------

    use crate::camera::fake::{Behavior, FakePortal};
    use std::time::{Duration, Instant};

    const TEST_PATTERN: &str = "videotestsrc is-live=true ! video/x-raw,width=320,height=240,framerate=30/1";

    fn ui() -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.CameraFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1000, 700);
        ui.window.present();
        ui
    }
    fn portal(behavior: Behavior) -> FakePortal {
        glib::MainContext::default().block_on(FakePortal::start(behavior))
    }
    fn wait(what: &str, seconds: u64, done: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < end {
            context.iteration(false);
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("timed out waiting for: {what}");
    }
    fn settle(ms: u64) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn descendants(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
        found.push(root.clone());
        let mut child = root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            descendants(&widget, found);
        }
    }
    fn dialog(ui: &Rc<Ui>) -> Option<adw::Dialog> {
        ui.camera_dialog.borrow().as_ref().and_then(|dialog| dialog.upgrade())
    }
    fn parts(ui: &Rc<Ui>) -> (gtk::Stack, Vec<gtk::Widget>) {
        let mut all = vec![];
        descendants(dialog(ui).expect("the camera dialog is open").upcast_ref(), &mut all);
        let stack = all.iter().find_map(|w| w.clone().downcast::<gtk::Stack>().ok()).expect("the page stack");
        (stack, all)
    }
    fn page(ui: &Rc<Ui>) -> String {
        parts(ui).0.visible_child_name().map(|name| name.to_string()).unwrap_or_default()
    }
    fn reason_title(ui: &Rc<Ui>) -> String {
        let (stack, _) = parts(ui);
        stack.child_by_name("reason").and_downcast::<adw::StatusPage>().unwrap().title().to_string()
    }
    fn button(ui: &Rc<Ui>, label: &str) -> Vec<gtk::Button> {
        parts(ui).1.into_iter().filter_map(|w| w.downcast::<gtk::Button>().ok()).filter(|b| b.label().as_deref() == Some(label)).collect()
    }
    /// Whether a widget is on the page the stack is showing, and visible itself.
    fn on_shown_page(widget: &gtk::Widget) -> bool {
        let mut child = widget.clone();
        while let Some(parent) = child.parent() {
            if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
                if stack.visible_child().as_ref() != Some(&child) {
                    return false;
                }
            }
            if !child.is_visible() {
                return false;
            }
            child = parent;
        }
        true
    }
    fn visible_button(ui: &Rc<Ui>, label: &str) -> gtk::Button {
        button(ui, label).into_iter().find(|b| on_shown_page(b.upcast_ref())).unwrap_or_else(|| panic!("no visible {label:?} button"))
    }
    fn use_test_source() {
        TEST_SOURCE.with(|source| *source.borrow_mut() = Some(TEST_PATTERN.into()));
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_yes_from_the_desktop_shows_the_camera_and_a_photo_is_added_to_the_message() {
        use std::os::unix::fs::PermissionsExt;
        use std::io::Read;
        use_test_source();
        let fake = portal(Behavior::default());
        let ui = ui();
        assert_eq!(ui.attachments.borrow().len(), 0);
        ui.open_camera();
        assert_eq!(page(&ui), "asking", "nothing is shown until the desktop has answered");
        wait("the live picture", 10, || page(&ui) == "live");
        assert_eq!(*fake.calls.borrow(), ["AccessCamera", "OpenPipeWireRemote"]);
        let take = visible_button(&ui, "Take Photo");
        wait("the first picture to enable the button", 10, || take.is_sensitive());
        take.emit_clicked();
        wait("the photo to be attached", 10, || ui.attachments.borrow().len() == 1);
        assert!(dialog(&ui).is_none() || !dialog(&ui).unwrap().is_visible(), "the dialog closed after the photo");
        let file = ui.attachments.borrow()[0].clone();
        let path = file.path().unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("camera-") && name.ends_with(".jpg"), "{name}");
        let mut bytes = Vec::new();
        std::fs::File::open(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "private");
        assert_eq!(path.parent(), crate::picture_editor_ui::temporary_copies_dir().as_deref(), "in the private temporary-copy folder");
        // It is a temporary copy: removing it from the message deletes it.
        crate::picture_editor_ui::discard_edited(Some(path.clone()));
        assert!(!path.exists());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_no_shows_why_and_never_opens_the_camera_and_try_again_asks_again() {
        use_test_source();
        let fake = portal(Behavior { response: 2, ..Default::default() });
        let ui = ui();
        ui.open_camera();
        wait("the denied page", 10, || page(&ui) == "denied");
        assert_eq!(fake.called("OpenPipeWireRemote"), 0, "permission refused: no connection");
        assert!(fake.peers.borrow().is_empty());
        assert!(visible_button(&ui, "Try Again").is_sensitive());
        assert!(button(&ui, "Take Photo").iter().all(|b| !b.is_sensitive()), "nothing to capture");
        // The person changes their mind in the desktop's settings.
        fake.set(|b| b.response = 0);
        visible_button(&ui, "Try Again").emit_clicked();
        wait("the live picture after trying again", 10, || page(&ui) == "live");
        assert_eq!(fake.called("AccessCamera"), 2, "asked again");
        assert_eq!(fake.called("OpenPipeWireRemote"), 1);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn closing_the_question_without_answering_is_not_the_same_as_saying_no() {
        let fake = portal(Behavior { response: 1, ..Default::default() });
        let ui = ui();
        ui.open_camera();
        wait("the reasons page", 10, || page(&ui) == "reason");
        assert_eq!(reason_title(&ui), "Camera Access Not Given");
        assert_eq!(fake.called("OpenPipeWireRemote"), 0);
        assert!(visible_button(&ui, "Try Again").is_sensitive());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_computer_without_a_camera_says_so_and_asks_for_nothing() {
        let fake = portal(Behavior { present: false, ..Default::default() });
        let ui = ui();
        ui.open_camera();
        wait("the reasons page", 10, || page(&ui) == "reason");
        assert_eq!(reason_title(&ui), "No Camera Found");
        assert_eq!(fake.called("AccessCamera"), 0, "no permission question for a camera that is not there");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_desktop_without_the_portal_says_so_and_offers_no_pointless_retry() {
        let ui = ui();
        ui.open_camera();
        wait("the reasons page", 10, || page(&ui) == "reason");
        assert_eq!(reason_title(&ui), "Camera Not Available");
        let reason = parts(&ui).0.child_by_name("reason").and_downcast::<adw::StatusPage>().unwrap();
        let retry = reason.child().and_downcast::<gtk::Button>().unwrap();
        assert!(!retry.is_visible(), "asking again cannot add a portal");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_portal_error_is_shown_in_plain_words_not_as_a_crash() {
        let fake = portal(Behavior { access_error: Some("org.freedesktop.portal.Error.Failed"), ..Default::default() });
        let ui = ui();
        ui.open_camera();
        wait("the reasons page", 10, || page(&ui) == "reason");
        assert_eq!(reason_title(&ui), "Camera Problem");
        let reason = parts(&ui).0.child_by_name("reason").and_downcast::<adw::StatusPage>().unwrap();
        assert_eq!(reason.description().as_deref(), Some("refused by the fake portal"));
        assert_eq!(fake.called("OpenPipeWireRemote"), 0);
        // Access refused by policy is a "denied", not a problem.
        let fake2 = &fake;
        fake2.set(|b| b.access_error = Some("org.freedesktop.portal.Error.NotAllowed"));
        visible_button(&ui, "Try Again").emit_clicked();
        wait("the denied page", 10, || page(&ui) == "denied");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn closing_the_dialog_while_the_question_is_open_leaves_nothing_running() {
        use_test_source();
        let fake = portal(Behavior { delay_ms: 600, ..Default::default() });
        let ui = ui();
        ui.open_camera();
        assert_eq!(page(&ui), "asking");
        dialog(&ui).unwrap().force_close();
        // The answer arrives after the person gave up: it must be ignored, and not leak a stream.
        wait("the portal's late yes to be answered", 15, || fake.called("OpenPipeWireRemote") == 1);
        settle(300);
        assert!(ui.attachments.borrow().is_empty());
        assert!(dialog(&ui).is_none(), "no dialog came back");
        let peer = fake.peers.borrow()[0].try_clone().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut byte = [0u8; 1];
        use std::io::Read;
        assert_eq!((&peer).read(&mut byte).unwrap_or(0), 0, "the connection was closed: end of stream");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn closing_the_dialog_stops_the_camera_and_releases_the_connection() {
        use_test_source();
        let fake = portal(Behavior::default());
        let ui = ui();
        ui.open_camera();
        wait("the live picture", 10, || page(&ui) == "live");
        dialog(&ui).unwrap().force_close();
        wait("the dialog to be forgotten", 5, || dialog(&ui).is_none());
        let peer = fake.peers.borrow()[0].try_clone().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        use std::io::Read;
        assert_eq!((&peer).read(&mut [0u8; 1]).unwrap_or(0), 0, "access ended with the dialog");
        // And the camera can be opened again afterwards.
        ui.open_camera();
        wait("a second live picture", 10, || page(&ui) == "live");
        assert_eq!(fake.called("AccessCamera"), 2);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn only_one_camera_dialog_exists_at_a_time() {
        use_test_source();
        let fake = portal(Behavior::default());
        let ui = ui();
        ui.open_camera();
        ui.open_camera();
        ui.open_camera();
        wait("the live picture", 10, || page(&ui) == "live");
        assert_eq!(fake.called("AccessCamera"), 1, "one question, however often it was asked for");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_camera_that_stops_shows_a_problem_page() {
        TEST_SOURCE.with(|source| *source.borrow_mut() = Some("videotestsrc num-buffers=2 ! video/x-raw,width=64,height=48,framerate=30/1".into()));
        let _fake = portal(Behavior::default());
        let ui = ui();
        ui.open_camera();
        wait("the problem page", 10, || page(&ui) == "reason");
        assert_eq!(reason_title(&ui), "Camera Problem");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_full_message_refuses_the_photo_and_leaves_no_file_behind() {
        use_test_source();
        let _fake = portal(Behavior::default());
        let ui = ui();
        let dir = crate::picture_editor_ui::temporary_copies_dir().unwrap();
        let before: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        for n in 0..64 {
            ui.attachments.borrow_mut().push(gio::File::for_path(format!("/nonexistent/file-{n}.txt")));
        }
        ui.open_camera();
        wait("the live picture", 10, || page(&ui) == "live");
        let take = visible_button(&ui, "Take Photo");
        wait("the button", 10, || take.is_sensitive());
        take.emit_clicked();
        settle(1500);
        assert_eq!(ui.attachments.borrow().len(), 64, "nothing was added");
        let after: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(before, after, "the refused photo was deleted, not left in the cache");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_attach_menu_offers_take_photo_and_it_opens_the_camera() {
        let ui = ui();
        let action = ui.window.lookup_action("attach-camera").expect("the action exists");
        assert!(action.is_enabled());
        action.activate(None);
        assert!(dialog(&ui).is_some(), "the action opens the dialog");
        ui.window.close();
    }
}
