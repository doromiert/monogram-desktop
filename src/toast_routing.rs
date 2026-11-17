//! Put a toast where the user can actually see it.
//!
//! The window's `ToastOverlay` sits under every dialog, so a toast raised while a dialog is open
//! used to be hidden behind it and the feedback never arrived. `show` picks the right place:
//! the window when nothing is open, the dialog's own overlay for an ordinary dialog (added on
//! demand), the preferences dialog's native toast, and, for dialogs with fixed content such as
//! alerts, the moment the dialog closes.
use adw::prelude::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Window,
    /// An ordinary dialog: use (or add) a `ToastOverlay` inside it.
    Overlay,
    /// `AdwPreferencesDialog` has its own `add_toast`.
    Preferences,
    /// Dialogs whose content is fixed (alerts, About, shortcuts) cannot hold an overlay.
    AfterClose,
}

pub fn route(dialog: Option<&adw::Dialog>) -> Route {
    match dialog {
        None => Route::Window,
        Some(dialog) if dialog.is::<adw::PreferencesDialog>() => Route::Preferences,
        Some(dialog) if dialog.is::<adw::AlertDialog>() || dialog.is::<adw::AboutDialog>() || dialog.is::<adw::ShortcutsDialog>() => Route::AfterClose,
        Some(_) => Route::Overlay,
    }
}

fn find_overlay(widget: &gtk::Widget) -> Option<adw::ToastOverlay> {
    if let Ok(overlay) = widget.clone().downcast::<adw::ToastOverlay>() {
        return Some(overlay);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_overlay(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// The dialog's toast overlay, wrapping its content in one the first time it is needed.
fn overlay_in(dialog: &adw::Dialog) -> Option<adw::ToastOverlay> {
    let child = dialog.child()?;
    if let Some(found) = find_overlay(&child) {
        return Some(found);
    }
    dialog.set_child(gtk::Widget::NONE);
    let overlay = adw::ToastOverlay::new();
    overlay.set_widget_name("monogram-dialog-toasts");
    overlay.set_child(Some(&child));
    dialog.set_child(Some(&overlay));
    Some(overlay)
}

pub fn show(window: &adw::ApplicationWindow, fallback: &adw::ToastOverlay, toast: adw::Toast) {
    let dialog = window.visible_dialog();
    match route(dialog.as_ref()) {
        Route::Window => fallback.add_toast(toast),
        Route::Preferences => {
            if let Some(preferences) = dialog.as_ref().and_then(|d| d.downcast_ref::<adw::PreferencesDialog>()) {
                preferences.add_toast(toast);
            }
        }
        Route::Overlay => match dialog.as_ref().and_then(overlay_in) {
            Some(overlay) => overlay.add_toast(toast),
            None => defer(window, fallback, dialog.as_ref(), toast),
        },
        Route::AfterClose => defer(window, fallback, dialog.as_ref(), toast),
    }
}

/// Show `toast` again once `dialog` has closed (through `show`, in case another dialog opened).
fn defer(window: &adw::ApplicationWindow, fallback: &adw::ToastOverlay, dialog: Option<&adw::Dialog>, toast: adw::Toast) {
    let Some(dialog) = dialog else {
        fallback.add_toast(toast);
        return;
    };
    let pending = Rc::new(RefCell::new(Some(toast)));
    let (window, fallback) = (window.downgrade(), fallback.downgrade());
    dialog.connect_closed(move |_| {
        if let (Some(toast), Some(window), Some(fallback)) = (pending.borrow_mut().take(), window.upgrade(), fallback.upgrade()) {
            show(&window, &fallback, toast);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn each_kind_of_dialog_gets_the_route_that_can_show_a_toast() {
        adw::init().unwrap();
        assert_eq!(route(None), Route::Window);
        assert_eq!(route(Some(&adw::Dialog::new())), Route::Overlay);
        assert_eq!(route(Some(adw::PreferencesDialog::new().upcast_ref())), Route::Preferences);
        assert_eq!(route(Some(adw::AlertDialog::new(Some("x"), None).upcast_ref())), Route::AfterClose);
        assert_eq!(route(Some(adw::AboutDialog::new().upcast_ref())), Route::AfterClose);
    }

    fn shows_text(widget: &gtk::Widget, text: &str) -> bool {
        widget.downcast_ref::<gtk::Label>().is_some_and(|label| label.text() == text) || {
            let mut child = widget.first_child();
            let mut found = false;
            while let Some(current) = child {
                found |= shows_text(&current, text);
                child = current.next_sibling();
            }
            found
        }
    }
    fn settle(milliseconds: u64) {
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
        while std::time::Instant::now() < until {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// The real thing: a toast raised through `Ui::notice` while a dialog covers the window must
    /// appear in the dialog, or for a fixed-content dialog, right after it closes.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_notice_raised_over_a_dialog_is_shown_above_it_not_hidden_behind() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.ToastRoutingFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = crate::ui::Ui::new(&app, true);
        ui.window.present();
        settle(300);

        // 1. Nothing open: the window shows it (as before).
        ui.notice("window toast");
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "window toast"));

        // 2. An ordinary dialog: the toast is inside the dialog, which covers the window overlay.
        let dialog = adw::Dialog::builder().title("Fixture").child(&gtk::Label::new(Some("body"))).build();
        dialog.present(Some(&ui.window));
        settle(300);
        ui.notice("dialog toast");
        settle(300);
        assert!(shows_text(dialog.child().unwrap().upcast_ref(), "dialog toast"), "shown inside the dialog");
        assert!(!shows_text(ui.toast.upcast_ref(), "dialog toast"), "not left behind it");
        dialog.force_close();
        settle(300);

        // 3. An alert cannot hold a toast: it is shown the moment the alert closes. (A toast
        // overlay shows one toast at a time, so clear the first one or this would just queue.)
        ui.toast.dismiss_all();
        settle(500);
        let alert = adw::AlertDialog::new(Some("Confirm"), Some("Are you sure?"));
        alert.add_response("ok", "OK");
        alert.present(Some(&ui.window));
        settle(300);
        ui.notice("after the alert");
        settle(300);
        assert!(!shows_text(ui.toast.upcast_ref(), "after the alert"), "held while the alert is open");
        alert.force_close();
        settle(500);
        assert!(shows_text(ui.toast.upcast_ref(), "after the alert"), "shown once the alert closed");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn an_ordinary_dialog_gets_one_overlay_that_is_reused_and_keeps_its_content() {
        adw::init().unwrap();
        let content = gtk::Label::new(Some("content"));
        let dialog = adw::Dialog::builder().child(&content).build();
        let overlay = overlay_in(&dialog).expect("an overlay is added");
        assert_eq!(overlay.widget_name(), "monogram-dialog-toasts");
        assert_eq!(overlay.child().as_ref(), Some(content.upcast_ref()), "the content moved inside it");
        assert_eq!(dialog.child().as_ref(), Some(overlay.upcast_ref()));
        let again = overlay_in(&dialog).unwrap();
        assert_eq!(again, overlay, "asking again reuses it instead of wrapping twice");
        // A dialog that already has its own overlay deeper inside is left alone.
        let own = adw::ToastOverlay::new();
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.append(&own);
        let dialog = adw::Dialog::builder().child(&wrapper).build();
        assert_eq!(overlay_in(&dialog), Some(own));
        assert_eq!(dialog.child().as_ref(), Some(wrapper.upcast_ref()));
    }

    fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_button(&current, label) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_contact_authorization_toast_has_a_resolve_button_that_opens_contact_access() {
        use crate::protocol::BackendEvent;
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.ResolveToastFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = crate::ui::Ui::new(&app, true);
        ui.window.present();
        settle(300);
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());

        // Any other crypto error stays a plain toast: no button.
        ui.backend_event(generation, &owner, BackendEvent::CryptoError { peer: "a@x".into(), text: "Something else".into() });
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "a@x: Something else"));
        assert!(find_button(ui.toast.upcast_ref(), "Resolve").is_none(), "no Resolve on other errors");
        ui.toast.dismiss_all();
        settle(500);

        ui.backend_event(
            generation,
            &owner,
            BackendEvent::CryptoError { peer: "bob@example.org".into(), text: crate::crypto::legacy::DENIED.into() },
        );
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "bob@example.org: Legacy OMEMO keys require contact authorization"));
        let button = find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button on the toast");
        assert!(ui.contact_access_widgets.borrow().is_none());
        button.emit_clicked();
        settle(300);
        let peer = ui.contact_access_widgets.borrow().as_ref().map(|c| c.peer().to_owned());
        assert_eq!(peer.as_deref(), Some("bob@example.org"), "Control Access opened for that contact");
        ui.window.close();
    }

    /// The path a real failed send takes: a stored own message and a `Failed` event, whose toast is
    /// wrapped as "Message not sent: <reason>".
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_failed_send_denied_for_contact_authorization_gets_a_resolve_button() {
        use crate::{model::{Chat, Message}, protocol::BackendEvent};
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.ResolveFailedFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = crate::ui::Ui::new(&app, true);
        ui.window.present();
        settle(300);
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let chat = Chat { jid: "bob@example.org".into(), name: "Bob".into(), ..Default::default() };
        let message = |n: &str| Message {
            id: format!("local-{n}"),
            wire_id: format!("wire-{n}"),
            markable_id: format!("wire-{n}"),
            sender: owner.clone(),
            timestamp: "2026-11-17T10:00:00Z".into(),
            text: "hi".into(),
            own: true,
            ..Default::default()
        };
        ui.store
            .save_account(&crate::model::Account { jid: owner.clone(), host: String::new(), port: 5222, direct_tls: false })
            .unwrap();
        ui.store.save_chat_message(&owner, &chat, Some(&message("a"))).unwrap();
        ui.store.save_chat_message(&owner, &chat, Some(&message("b"))).unwrap();

        // Another failure reason: plain toast.
        ui.backend_event(generation, &owner, BackendEvent::Failed {
            peer: "bob@example.org".into(),
            id: "wire-b".into(),
            reason: "the server connection dropped".into(),
        });
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "Message not sent: the server connection dropped"));
        assert!(find_button(ui.toast.upcast_ref(), "Resolve").is_none());
        ui.toast.dismiss_all();
        settle(500);

        ui.backend_event(generation, &owner, BackendEvent::Failed {
            peer: "bob@example.org".into(),
            id: "wire-a".into(),
            reason: crate::crypto::legacy::DENIED.into(),
        });
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "Message not sent: Legacy OMEMO keys require contact authorization"));
        let button = find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button on the failed-send toast");
        button.emit_clicked();
        settle(300);
        let peer = ui.contact_access_widgets.borrow().as_ref().map(|c| c.peer().to_owned());
        assert_eq!(peer.as_deref(), Some("bob@example.org"));
        ui.window.close();
    }

    // ---- Resolve toast on every path that can report the contact-authorization denial ----

    const PEER: &str = "bob@example.org";
    const DENIED_TEXT: &str = "Legacy OMEMO keys require contact authorization";

    /// A window with an account, a chat with `PEER` and two stored own messages (`wire-a`, `wire-b`).
    fn fixture(app_id: &str) -> std::rc::Rc<crate::ui::Ui> {
        use crate::model::{Account, Chat, Message};
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id(app_id)
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = crate::ui::Ui::new(&app, true);
        ui.window.present();
        settle(300);
        let owner = ui.state.borrow().account.clone();
        let chat = Chat { jid: PEER.into(), name: "Bob".into(), ..Default::default() };
        ui.store
            .save_account(&Account { jid: owner.clone(), host: String::new(), port: 5222, direct_tls: false })
            .unwrap();
        for n in ["a", "b"] {
            let message = Message {
                id: format!("local-{n}"),
                wire_id: format!("wire-{n}"),
                markable_id: format!("wire-{n}"),
                sender: owner.clone(),
                timestamp: "2026-11-17T10:00:00Z".into(),
                text: "hi".into(),
                own: true,
                ..Default::default()
            };
            ui.store.save_chat_message(&owner, &chat, Some(&message)).unwrap();
        }
        ui
    }
    fn clear(ui: &crate::ui::Ui) {
        ui.toast.dismiss_all();
        settle(500);
    }
    fn open_contact_access_peer(ui: &crate::ui::Ui) -> Option<String> {
        ui.contact_access_widgets.borrow().as_ref().map(|c| c.peer().to_owned())
    }
    /// The toast is shown, has no Resolve button when `resolvable` is false, and otherwise has one
    /// that opens Control Access for `PEER`.
    fn assert_toast(ui: &crate::ui::Ui, title: &str, resolvable: bool) {
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), title), "toast text {title:?}");
        let button = find_button(ui.toast.upcast_ref(), "Resolve");
        assert_eq!(button.is_some(), resolvable, "Resolve button presence for {title:?}");
        if let Some(button) = button {
            assert!(open_contact_access_peer(ui).is_none());
            button.emit_clicked();
            settle(300);
            assert_eq!(open_contact_access_peer(ui).as_deref(), Some(PEER), "Control Access opened for the contact");
            ui.contact_access_widgets.borrow_mut().take();
        }
        clear(ui);
    }

    /// A queued message whose encryption could not be chosen: this toast is the raw reason.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn queued_send_encryption_failure_offers_resolve_for_the_denial_only() {
        let ui = fixture("com.negzero.ResolveQueuedFixture");
        let owner = ui.state.borrow().account.clone();
        let queued = |n: &str| ui.store.message(&owner, PEER, &format!("local-{n}")).unwrap();

        ui.fail_queued_selection(&owner, PEER, vec![queued("b")], "The connection is unavailable");
        assert_toast(&ui, "The connection is unavailable", false);

        ui.fail_queued_selection(&owner, PEER, vec![queued("a")], DENIED_TEXT);
        assert_toast(&ui, DENIED_TEXT, true);
        ui.window.close();
    }

    /// The composer's encryption check failing: "Could not check conversation encryption. <reason>".
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn encryption_check_failure_offers_resolve_for_the_denial_only() {
        use crate::crypto::Error;
        let ui = fixture("com.negzero.ResolveCheckFixture");

        ui.report_encryption_check_failure(PEER, &Error::Unavailable("Connection interrupted"));
        assert_toast(&ui, "Could not check conversation encryption. Connection interrupted", false);

        ui.report_encryption_check_failure(PEER, &Error::Unavailable(crate::crypto::legacy::DENIED));
        assert_toast(&ui, &format!("Could not check conversation encryption. {DENIED_TEXT}"), true);
        ui.window.close();
    }

    /// The `CryptoError` backend event.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn crypto_error_event_offers_resolve_for_the_denial_only() {
        use crate::protocol::BackendEvent;
        let ui = fixture("com.negzero.ResolveEventFixture");
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let event = |text: &str| BackendEvent::CryptoError { peer: PEER.into(), text: text.into() };

        ui.backend_event(generation, &owner, event("Something else"));
        assert_toast(&ui, &format!("{PEER}: Something else"), false);

        ui.backend_event(generation, &owner, event(DENIED_TEXT));
        assert_toast(&ui, &format!("{PEER}: {DENIED_TEXT}"), true);
        ui.window.close();
    }

    /// The `Failed` backend event for one of our own messages.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn failed_send_event_offers_resolve_for_the_denial_only() {
        use crate::protocol::BackendEvent;
        let ui = fixture("com.negzero.ResolveFailedEventFixture");
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let event = |id: &str, reason: &str| BackendEvent::Failed { peer: PEER.into(), id: id.into(), reason: reason.into() };

        ui.backend_event(generation, &owner, event("wire-b", "the server connection dropped"));
        assert_toast(&ui, "Message not sent: the server connection dropped", false);

        ui.backend_event(generation, &owner, event("wire-a", DENIED_TEXT));
        assert_toast(&ui, &format!("Message not sent: {DENIED_TEXT}"), true);
        ui.window.close();
    }

    /// A server cannot fake the button: its words are prefixed or differ, so they never match.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_reason_that_only_contains_the_denial_text_gets_no_resolve_button() {
        let ui = fixture("com.negzero.ResolveSpoofFixture");
        ui.notice_resolving_authorization(PEER, &format!("the server said: {DENIED_TEXT}"), "spoof".into());
        assert_toast(&ui, "spoof", false);
        ui.window.close();
    }

    const ROOM: &str = "team@conference.example.org";

    fn add_room(ui: &std::rc::Rc<crate::ui::Ui>) -> String {
        let mut state = ui.state.borrow_mut();
        let chat = state.chat_for(ROOM);
        chat.group = true;
        chat.id.clone()
    }

    /// In a group chat the denied member is unknown and the room has no contact access: Resolve
    /// must not open the "Contact Access Unavailable / A direct contact is required" dialog.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn resolve_in_a_room_opens_its_participants_not_contact_access() {
        let ui = fixture("com.negzero.ResolveRoomFixture");
        let room_id = add_room(&ui);
        ui.select_chat(&room_id);
        settle(300);

        ui.notice_resolving_authorization(ROOM, DENIED_TEXT, format!("Message not sent: {DENIED_TEXT}"));
        settle(300);
        assert!(ui.participants.borrow().is_none(), "nothing open yet");
        find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button").emit_clicked();
        settle(300);
        assert!(open_contact_access_peer(&ui).is_none(), "no Control Access dialog for a room");
        assert_eq!(ui.participants.borrow().as_ref().map(|view| view.room.clone()).as_deref(), Some(ROOM));
        ui.window.close();
    }

    /// The toast can belong to a room that is not the one in view: Resolve switches to it first.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn resolve_for_a_room_that_is_not_open_switches_to_it() {
        let ui = fixture("com.negzero.ResolveOtherRoomFixture");
        let room_id = add_room(&ui);
        let other_id = ui.state.borrow_mut().chat_for(PEER).id.clone();
        ui.select_chat(&other_id);
        settle(300);
        assert_eq!(ui.state.borrow().current().jid, PEER);

        ui.notice_resolving_authorization(ROOM, DENIED_TEXT, "title".into());
        settle(300);
        find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button").emit_clicked();
        settle(300);
        assert_eq!(ui.state.borrow().current().id, room_id, "switched to the room");
        assert_eq!(ui.participants.borrow().as_ref().map(|view| view.room.clone()).as_deref(), Some(ROOM));
        assert!(open_contact_access_peer(&ui).is_none());
        ui.window.close();
    }

    /// A room member's profile card (reached from the participants list) leads to Control Access.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_member_profile_card_has_control_access_only_for_a_real_contact() {
        let ui = fixture("com.negzero.ResolveProfileFixture");
        let popover = |ui: &crate::ui::Ui| ui.active_profile.borrow().clone().expect("profile card open");

        // A room occupant whose real address is hidden: only a room nick, no contact access.
        ui.profile_card(&ui.window, &format!("{ROOM}/bob"), "bob");
        settle(300);
        assert!(find_button(popover(&ui).upcast_ref(), "Control Access").is_none());

        ui.profile_card(&ui.window, PEER, "Bob");
        settle(300);
        let button = find_button(popover(&ui).upcast_ref(), "Control Access").expect("Control Access on the card");
        assert!(open_contact_access_peer(&ui).is_none());
        button.emit_clicked();
        settle(300);
        assert_eq!(open_contact_access_peer(&ui).as_deref(), Some(PEER));
        ui.window.close();
    }
}
