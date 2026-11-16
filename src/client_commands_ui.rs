//! Client-side slash commands: typed in the composer, handled locally and never sent.
//!
//! `/neofetch` opens a dialog with this machine's model, processor, memory, disk, uptime and
//! distribution plus the Monogram version and commit (see `sysinfo`), with a Copy button for bug
//! reports. It contains no user name or host name.
use crate::{sysinfo, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gdk, gio};
use std::rc::Rc;

impl Ui {
    /// Handle `text` if it is a client command. Returns true when it was one, in which case the
    /// composer has been cleared and nothing must be sent.
    pub fn run_client_command(self: &Rc<Self>, text: &str) -> bool {
        if !sysinfo::is_neofetch(text) {
            return false;
        }
        self.composer.buffer().set_text("");
        let weak = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            // Reading /proc and /sys is quick, but it stays off the GTK thread like other I/O.
            let Ok(report) = gio::spawn_blocking(|| sysinfo::collect().render()).await else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                ui.show_system_info(&report);
            }
        });
        true
    }

    fn show_system_info(self: &Rc<Self>, report: &str) {
        let output = gtk::Label::builder()
            .label(report)
            .selectable(true)
            .xalign(0.)
            .css_classes(["monospace"])
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        output.update_property(&[gtk::accessible::Property::Label("System information")]);
        let toasts = adw::ToastOverlay::new();
        let header = adw::HeaderBar::new();
        let copy = icon_button("edit-copy-symbolic", "Copy system information");
        header.pack_start(&copy);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        toasts.set_child(Some(&scroll(&output)));
        view.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder()
            .title("System Information")
            .content_width(520)
            .child(&view)
            .build();
        let report = report.to_owned();
        let toasts = toasts.downgrade();
        copy.connect_clicked(move |button| {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&report);
            }
            if let Some(toasts) = toasts.upgrade() {
                toasts.add_toast(adw::Toast::new("System information copied"));
            }
            let _ = button;
        });
        dialog.present(Some(&self.window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib;
    use std::time::{Duration, Instant};

    fn settle(milliseconds: u64) {
        let context = glib::MainContext::default();
        let until = Instant::now() + Duration::from_millis(milliseconds);
        while Instant::now() < until {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn neofetch_is_handled_locally_and_ordinary_text_is_still_sent() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.ClientCommandFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        settle(300);
        let sent = |ui: &Rc<Ui>| ui.state.borrow().current().messages.len();
        let before = sent(&ui);

        ui.composer.buffer().set_text("/neofetch");
        ui.send();
        settle(500);
        assert_eq!(ui.composer_text(), "", "the command is consumed");
        assert_eq!(sent(&ui), before, "a client command is never sent");
        let dialog = ui.window.visible_dialog().expect("the report opens in a dialog");
        assert_eq!(dialog.downcast_ref::<adw::Dialog>().unwrap().title(), "System Information");
        dialog.downcast_ref::<adw::Dialog>().unwrap().close();
        settle(300);

        ui.composer.buffer().set_text("hello there");
        ui.send();
        settle(300);
        assert_eq!(sent(&ui), before + 1, "ordinary text is unaffected");
        assert!(ui.window.visible_dialog().is_none());
        ui.window.close();
    }
}
