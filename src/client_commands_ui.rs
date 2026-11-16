//! Client-side slash commands: typed in the composer, handled locally and never sent as typed.
//!
//! A command generates message content. `/neofetch` replaces itself with this machine's model,
//! processor, memory, total disk, uptime and distribution plus the Monogram version and commit
//! (see `sysinfo`), as a code block so the columns stay aligned. It is left in the composer for
//! the user to read, edit and send: nothing about their machine leaves it without that step. The
//! report contains no user name or host name.
use crate::{formatting::Style, sysinfo, ui::Ui};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

impl Ui {
    /// Handle `text` if it is a client command. Returns true when it was one, in which case the
    /// caller must not send it; the generated content arrives in the composer a moment later.
    pub fn run_client_command(self: &Rc<Self>, text: &str) -> bool {
        if !sysinfo::is_neofetch(text) {
            return false;
        }
        let (weak, typed) = (Rc::downgrade(self), self.composer_text());
        glib::spawn_future_local(async move {
            // Reading /proc and /sys is quick, but it stays off the GTK thread like other I/O.
            let Ok(report) = gio::spawn_blocking(|| sysinfo::collect().render()).await else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                ui.fill_composer_with_code(&typed, &report);
            }
        });
        true
    }

    /// Replace the command in the composer with `content` as a code block. If the user changed
    /// the composer while it was being generated, their text wins and nothing is replaced.
    fn fill_composer_with_code(self: &Rc<Self>, expected: &str, content: &str) {
        if self.composer_text() != expected {
            return;
        }
        let buffer = self.composer.buffer();
        buffer.set_text(content);
        if let Some(tag) = buffer.tag_table().lookup("format-pre") {
            buffer.apply_tag(&tag, &buffer.start_iter(), &buffer.end_iter());
        }
        buffer.place_cursor(&buffer.end_iter());
        self.composer.grab_focus();
        self.save_draft();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn neofetch_becomes_an_editable_code_block_that_is_sent_only_when_the_user_sends_it() {
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
        // Sending the command generated the content; it did not send anything.
        assert_eq!(sent(&ui), before, "the command itself is never sent");
        let report = ui.composer_text();
        assert!(report.starts_with("Model") || report.starts_with("Motherboard"), "{report}");
        assert!(report.lines().count() == 7 && report.contains("Monogram"), "{report}");
        assert!(!report.contains("/neofetch"));
        let markup = ui.composer_formatting();
        let length = report.chars().count();
        assert!(
            markup.ranges.iter().any(|r| r.has(Style::Pre) && r.start == 0 && r.end == length),
            "the whole report is a code block so the columns stay aligned"
        );

        // The user reviews it and sends it; now it is an ordinary message with that body.
        ui.send();
        settle(300);
        assert_eq!(sent(&ui), before + 1);
        let message = ui.state.borrow().current().messages.last().cloned().unwrap();
        // Code blocks are stored with XEP-0393 fences so other clients show them monospaced.
        assert_eq!(message.text, format!("```\n{report}\n```"));
        assert!(message.formatting.as_ref().is_some_and(|m| m.ranges.iter().any(|r| r.has(Style::Pre))));

        // Text typed while a command is being generated is never overwritten.
        ui.composer.buffer().set_text("/neofetch");
        ui.send();
        ui.composer.buffer().set_text("changed my mind");
        settle(500);
        assert_eq!(ui.composer_text(), "changed my mind");
        ui.window.close();
    }
}
