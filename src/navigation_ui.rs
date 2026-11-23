use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

pub(crate) fn destination(top: f64, height: f64, current: f64, viewport: f64) -> f64 {
    if top >= current + 16. && top + height <= current + viewport - 16. {
        current
    } else if top < current + 16. || height > viewport - 32. {
        (top - 16.).max(0.)
    } else {
        (top + height - viewport + 16.).max(0.)
    }
}
impl Ui {
    pub(crate) fn cancel_reply_scroll(&self) {
        if let Some(animation) = self.reply_scroll.borrow_mut().take() {
            animation.pause();
        }
        self.history_window.moving.set(false);
    }
    pub(crate) fn animate_reply_scroll(self: &Rc<Self>, value: f64) {
        self.cancel_reply_scroll();
        let adjustment = self.scroll.vadjustment();
        let value = value.clamp(
            adjustment.lower(),
            (adjustment.upper() - adjustment.page_size()).max(0.),
        );
        if (adjustment.value() - value).abs() < 1. {
            self.schedule_history_maintenance();
            return;
        }
        self.history_window.moving.set(true);
        let a = adjustment.clone();
        let target = adw::CallbackAnimationTarget::new(move |value| a.set_value(value));
        let animation =
            adw::TimedAnimation::new(&self.scroll, adjustment.value(), value, 180, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        let weak = Rc::downgrade(self);
        animation.connect_done(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.reply_scroll.borrow_mut().take();
                ui.history_window.moving.set(false);
                ui.schedule_history_maintenance();
            }
        });
        self.reply_scroll.replace(Some(animation.clone()));
        animation.play();
    }
    pub(crate) fn install_message_scroll_input(self: &Rc<Self>) {
        let controller =
            gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        controller.connect_scroll(move |_, _, _| {
            if let Some(ui) = weak.upgrade() {
                ui.cancel_reply_scroll();
            }
            gtk::glib::Propagation::Proceed
        });
        self.scroll.add_controller(controller);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(ui) = weak.upgrade() else { return gtk::glib::Propagation::Proceed };
            let scroll = match key {
                gtk::gdk::Key::Page_Up | gtk::gdk::Key::KP_Page_Up => gtk::ScrollType::PageUp,
                gtk::gdk::Key::Page_Down | gtk::gdk::Key::KP_Page_Down => gtk::ScrollType::PageDown,
                gtk::gdk::Key::End | gtk::gdk::Key::KP_End => gtk::ScrollType::End,
                _ => return gtk::glib::Propagation::Proceed,
            };
            if modifiers.intersects(gtk::gdk::ModifierType::SHIFT_MASK | gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::META_MASK | gtk::gdk::ModifierType::HYPER_MASK)
                || !ui.scroll.is_mapped() || ui.window.visible_dialog().is_some()
                || ui.active_media.borrow().is_some() || ui.active_story.borrow().is_some()
            { return gtk::glib::Propagation::Proceed; }
            let mut focus = gtk::prelude::GtkWindowExt::focus(&ui.window);
            while let Some(widget) = focus {
                if widget.is::<gtk::Popover>() { return gtk::glib::Propagation::Proceed; }
                focus = widget.parent();
            }
            ui.cancel_reply_scroll();
            ui.message_scroll.borrow_mut().take();
            if scroll == gtk::ScrollType::End {
                if ui.history_window.newest() {
                    ui.queue_message_scroll(crate::messages::ScrollTarget::Bottom);
                } else {
                    ui.load_timeline(None);
                }
            } else {
                ui.scroll.emit_scroll_child(scroll, false);
                ui.history_edge(if scroll == gtk::ScrollType::PageUp {
                    crate::history_window::Side::Older
                } else {
                    crate::history_window::Side::Newer
                });
            }
            gtk::glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }
    pub fn about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name("Monogram")
            .application_icon("com.negzero.MonogramDesktop")
            .developer_name("doromiert")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Private XMPP messaging for Linux and Android.")
            .website("https://github.com/doromiert/monogram-desktop")
            .issue_url("https://github.com/doromiert/monogram-desktop/issues")
            .license_type(gtk::License::Gpl30)
            .build();
        dialog.add_link(
            "Source code",
            "https://github.com/doromiert/monogram-desktop",
        );
        // CC BY 4.0 asks for credit, a link to the licence and a note of any changes.
        dialog.add_legal_section(
            "Animated emoji",
            Some("© Google LLC"),
            gtk::License::Custom,
            Some(
                "Noto Emoji animations by Google, licensed under Creative Commons Attribution 4.0 International: https://creativecommons.org/licenses/by/4.0/\n\nChanges: the Lottie files were repackaged as minified, gzip-compressed JSON. The artwork is unchanged.",
            ),
        );
        dialog.present(Some(&self.window));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_targets_do_not_move_and_offscreen_targets_move_just_enough() {
        assert_eq!(destination(140., 80., 100., 400.), 100.);
        assert_eq!(destination(40., 80., 100., 400.), 24.);
        assert_eq!(destination(460., 80., 100., 400.), 156.);
        assert_eq!(destination(120., 800., 100., 400.), 104.);
    }
}
#[cfg(test)]
mod native_tests {
    use super::*;
    use gtk::{gio, glib};
    use std::time::{Duration, Instant};
    #[gtk::test]
    #[ignore = "requires private X11, xdotool and D-Bus"]
    fn page_keys_scroll_messages_without_moving_focus_or_the_draft() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.MessagePagingFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1100, 700);
        let content = gtk::Label::new(Some("Fictional message history"));
        content.set_size_request(400, 10000);
        ui.scroll.set_child(Some(&content));
        ui.window.present();
        let pump = |ms| {
            let until = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < until {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        pump(250);
        let windows = std::process::Command::new("xdotool").args(["search", "--name", "^Monogram$"]).output().unwrap();
        let id = String::from_utf8(windows.stdout).unwrap().lines().last().unwrap().to_owned();
        assert!(std::process::Command::new("xdotool").args(["windowfocus", "--sync", &id]).status().unwrap().success());
        ui.composer.buffer().set_text("draft\nwith several\nunchanged lines");
        ui.composer.buffer().place_cursor(&ui.composer.buffer().iter_at_offset(3));
        ui.composer.grab_focus();
        pump(120);
        let adjustment = ui.scroll.vadjustment();
        assert!(adjustment.page_increment() > 100.);
        adjustment.set_value(2400.);
        ui.message_scroll.borrow_mut().take();
        let key = |key: &str| {
            assert!(std::process::Command::new("xdotool").args(["key", "--clearmodifiers", key]).status().unwrap().success());
            pump(280);
        };
        let before = adjustment.value();
        let page = adjustment.page_increment();
        let focus = gtk::prelude::GtkWindowExt::focus(&ui.window);
        let started = Instant::now();
        key("Prior");
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!((adjustment.value() - (before - page)).abs() < 2.);
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&ui.window), focus);
        assert_eq!(ui.composer.buffer().cursor_position(), 3);
        assert_eq!(ui.composer_text(), "draft\nwith several\nunchanged lines");
        key("Next");
        assert!((adjustment.value() - before).abs() < 2.);
        ui.history_window.reset(true);
        adjustment.set_value(1200.);
        key("End");
        assert!((adjustment.value() - (adjustment.upper() - adjustment.page_size())).abs() < 2.);
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&ui.window), focus);
        assert_eq!(ui.composer.buffer().cursor_position(), 3);
        adjustment.set_value(before);
        key("shift+Prior");
        assert!((adjustment.value() - before).abs() < 2., "modified keys retain text selection behavior");
        ui.search_toggle.set_active(true);
        ui.search.grab_focus();
        pump(120);
        let focus = gtk::prelude::GtkWindowExt::focus(&ui.window);
        let before = adjustment.value();
        key("Prior");
        assert!(adjustment.value() < before - 100.);
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&ui.window), focus);
        ui.shortcuts();
        pump(120);
        let before = adjustment.value();
        key("Next");
        assert_eq!(adjustment.value(), before, "dialogs must not page the conversation behind them");
        key("End");
        assert_eq!(adjustment.value(), before);
        ui.window.visible_dialog().unwrap().close();
        pump(120);
        adjustment.set_value(adjustment.lower());
        key("Prior");
        assert_eq!(adjustment.value(), adjustment.lower());
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        key("Next");
        assert_eq!(adjustment.value(), adjustment.upper() - adjustment.page_size());
        ui.window.close();
    }
    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn native_navigation_feedback_and_about() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramNavigationTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(1000, 650);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let spacer = gtk::Label::new(Some("Fictional navigation fixture"));
        spacer.set_size_request(400, 3200);
        content.append(&spacer);
        ui.scroll.set_child(Some(&content));
        ui.window.present();
        glib::MainContext::default().block_on(async{
   glib::timeout_future(Duration::from_millis(250)).await;ui.scroll.vadjustment().set_value(20.);
   let start=Instant::now();ui.animate_reply_scroll(700.);let mut samples=Vec::new();let mut previous=Instant::now();let mut gap=Duration::ZERO;
   while start.elapsed()<Duration::from_millis(300){glib::timeout_future(Duration::from_millis(16)).await;let now=Instant::now();gap=gap.max(now-previous);previous=now;samples.push(ui.scroll.vadjustment().value().round()as i32);}
   samples.sort();samples.dedup();assert!(samples.len()>=3,"Missing smooth scroll frames: {samples:?}");assert!((ui.scroll.vadjustment().value()-700.).abs()<1.);assert!(gap<Duration::from_millis(400),"UI stalled: {gap:?}");
   ui.about();glib::timeout_future(Duration::from_millis(80)).await;assert!(ui.window.visible_dialog().is_some_and(|d|d.is::<adw::AboutDialog>()));ui.window.visible_dialog().unwrap().close();
   println!("PASS native About and smooth reply navigation: {} distinct positions, largest main-loop gap {:?}",samples.len(),gap);
  });
        ui.window.close();
    }
}
