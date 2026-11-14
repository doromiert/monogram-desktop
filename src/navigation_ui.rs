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
    pub(crate) fn install_reply_scroll_input(self: &Rc<Self>) {
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
