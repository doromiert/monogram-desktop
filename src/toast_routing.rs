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
}
