//! A toast for work that is usually instant: it appears only if the work is still going after a
//! delay, and goes away as soon as the work ends.
//!
//! Hold the returned [`SlowNotice`] for as long as the work runs and let it drop at the end. It
//! then does the right thing on every way out of the work, early returns and errors included: a
//! timer that has not fired is cancelled, so a quick job never shows anything, and a toast that
//! has appeared is dismissed.
use crate::ui::Ui;
use gtk::glib;
use std::{cell::RefCell, rc::Rc, time::Duration};

/// How long work may take before it is worth saying anything about.
pub const SLOW_AFTER: Duration = Duration::from_secs(3);

#[derive(Default)]
struct State {
    /// The timer, until it fires or is cancelled.
    timer: Option<glib::SourceId>,
    /// The toast, once it has been shown.
    toast: Option<adw::Toast>,
}

#[must_use = "the notice lasts as long as this value does; dropping it at once means it never shows"]
pub struct SlowNotice(Rc<RefCell<State>>);

impl SlowNotice {
    /// Whether the toast has appeared.
    #[cfg(test)]
    pub fn shown(&self) -> bool {
        self.0.borrow().toast.is_some()
    }
}

impl Drop for SlowNotice {
    fn drop(&mut self) {
        let (timer, toast) = {
            let mut state = self.0.borrow_mut();
            (state.timer.take(), state.toast.take())
        };
        if let Some(timer) = timer {
            timer.remove();
        }
        if let Some(toast) = toast {
            toast.dismiss();
        }
    }
}

impl Ui {
    /// Say `text` if the caller is still working `after` from now.
    pub(crate) fn notice_if_slow(self: &Rc<Self>, text: &str, after: Duration) -> SlowNotice {
        let state = Rc::new(RefCell::new(State::default()));
        let (weak_ui, weak_state, text) = (Rc::downgrade(self), Rc::downgrade(&state), text.to_owned());
        let timer = glib::timeout_add_local_once(after, move || {
            let Some(state) = weak_state.upgrade() else { return };
            // The timer has fired, whatever happens next: it must never be removed again.
            state.borrow_mut().timer = None;
            let Some(ui) = weak_ui.upgrade() else { return };
            let toast = adw::Toast::builder().title(&text).timeout(0).build();
            state.borrow_mut().toast = Some(toast.clone());
            ui.show_toast(toast);
        });
        state.borrow_mut().timer = Some(timer);
        SlowNotice(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adw::prelude::*;
    use gtk::gio;
    use std::time::Instant;

    fn ui() -> Rc<Ui> {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.SlowNoticeFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.set_default_size(900, 600);
        ui.window.present();
        ui
    }
    fn settle(ms: u64) {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn labels(root: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = root.downcast_ref::<gtk::Label>() {
            found.push(label.label().to_string());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            labels(&widget, found);
        }
    }
    /// The words on the toasts currently on screen.
    fn on_screen(ui: &Rc<Ui>) -> Vec<String> {
        let mut found = vec![];
        labels(ui.toast.upcast_ref(), &mut found);
        found
    }
    fn says(ui: &Rc<Ui>, text: &str) -> bool {
        on_screen(ui).iter().any(|label| label.contains(text))
    }

    /// Run the main loop until `done`, for at most `limit`; whether it became true.
    fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let end = Instant::now() + limit;
        while Instant::now() < end {
            context.iteration(false);
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        done()
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn work_that_finishes_in_time_never_shows_anything() {
        let ui = ui();
        // Finished at once, and finished with plenty of the delay to spare.
        for finishes_after in [0u64, 100] {
            let notice = ui.notice_if_slow("Working on it…", Duration::from_millis(1500));
            settle(finishes_after);
            assert!(!notice.shown());
            drop(notice);
        }
        // Long past when it would have appeared.
        settle(2200);
        assert!(!says(&ui, "Working on it"), "{:?}", on_screen(&ui).into_iter().filter(|l| l.contains("Working")).collect::<Vec<_>>());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn work_that_runs_long_shows_it_and_it_goes_when_the_work_ends() {
        let ui = ui();
        let notice = ui.notice_if_slow("Working on it…", Duration::from_millis(150));
        assert!(wait_until(Duration::from_secs(5), || notice.shown()), "it appears");
        assert!(wait_until(Duration::from_secs(5), || says(&ui, "Working on it")), "and is on screen");
        // It stays for as long as the work does: it is not a toast that times out.
        settle(7000);
        assert!(says(&ui, "Working on it"), "still there while the work goes on");
        drop(notice);
        assert!(wait_until(Duration::from_secs(5), || !says(&ui, "Working on it")), "gone when the work ended");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_notice_does_not_appear_before_the_delay() {
        let ui = ui();
        let started = Instant::now();
        let notice = ui.notice_if_slow("Slow", Duration::from_millis(600));
        assert!(wait_until(Duration::from_secs(5), || notice.shown()));
        assert!(started.elapsed() >= Duration::from_millis(580), "{:?}", started.elapsed());
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn several_notices_are_independent_and_dropping_twice_is_harmless() {
        let ui = ui();
        // One job far from its delay, one close to it: the second shows, the first does not.
        let quick = ui.notice_if_slow("Quick job", Duration::from_secs(30));
        let slow = ui.notice_if_slow("Slow job", Duration::from_millis(100));
        assert!(wait_until(Duration::from_secs(5), || slow.shown()));
        assert!(!quick.shown(), "its own, much longer, delay has not passed");
        drop(quick);
        assert!(wait_until(Duration::from_secs(5), || says(&ui, "Slow job")));
        assert!(!says(&ui, "Quick job"));
        drop(slow);
        assert!(wait_until(Duration::from_secs(5), || !says(&ui, "Slow job")));
        // Dropped before, around and after the timer firing: no warning, no crash.
        for after in [0u64, 50, 150] {
            let notice = ui.notice_if_slow("again", Duration::from_millis(after));
            settle(100);
            drop(notice);
        }
        settle(300);
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_notice_outliving_the_window_does_nothing() {
        let ui = ui();
        let notice = ui.notice_if_slow("Late", Duration::from_millis(100));
        ui.window.close();
        drop(ui);
        settle(300);
        drop(notice);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_default_delay_is_three_seconds() {
        assert_eq!(SLOW_AFTER, Duration::from_secs(3));
    }

    // ---- deleting messages: no toast for the usual instant case --------------------------

    use crate::model::{Account, Message, State};

    const OWNER: &str = "alice@example.invalid";
    const PEER: &str = "bob@example.invalid";

    fn with_messages(ui: &Rc<Ui>, count: usize) {
        ui.store.save_account(&Account { jid: OWNER.into(), host: String::new(), port: 5222, direct_tls: false }).unwrap();
        let mut state = State::live(OWNER, vec![]);
        let chat = state.chat_for(PEER);
        chat.messages = (0..count)
            .map(|n| Message {
                id: format!("m{n}"),
                text: format!("message {n}"),
                author: "Bob".into(),
                sender: PEER.into(),
                time: "12:00".into(),
                ..Default::default()
            })
            .collect();
        state.active = state.chat_for(PEER).id.clone();
        ui.store.save(&state).unwrap();
        ui.activate_state(state);
        ui.show_main();
        settle(300);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn asking_to_delete_shows_no_preparing_toast_just_the_confirmation() {
        let ui = ui();
        with_messages(&ui, 3);
        ui.delete_confirmation(vec!["m0".into()]);
        // Checked straight away and again while the confirmation comes up.
        for _ in 0..8 {
            settle(60);
            assert!(!says(&ui, "Preparing"), "{:?}", on_screen(&ui));
            assert!(!says(&ui, "message deletion"), "{:?}", on_screen(&ui));
        }
        assert!(ui.window.visible_dialog().is_some(), "the confirmation did appear");
        ui.window.close();
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_normal_deletion_shows_no_toast_at_all_and_really_deletes() {
        let ui = ui();
        with_messages(&ui, 4);
        let done = Rc::new(std::cell::Cell::new(false));
        let (flag, weak) = (done.clone(), Rc::downgrade(&ui));
        glib::spawn_future_local(async move {
            let ui = weak.upgrade().unwrap();
            let prepared = ui.prepare_deletion(vec!["m1".into(), "m2".into()]).await.unwrap();
            ui.apply_deletion(prepared, false);
            flag.set(true);
        });
        let started = Instant::now();
        let context = glib::MainContext::default();
        let mut seen: Vec<String> = vec![];
        while started.elapsed() < Duration::from_millis(1500) {
            context.iteration(false);
            seen.extend(on_screen(&ui));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(done.get());
        assert!(ui.store.message(OWNER, PEER, "m1").is_none() && ui.store.message(OWNER, PEER, "m2").is_none(), "deleted");
        assert!(ui.store.message(OWNER, PEER, "m0").is_some() && ui.store.message(OWNER, PEER, "m3").is_some(), "the rest kept");
        assert!(
            !seen.iter().any(|text| text.contains("Deleting") || text.contains("Preparing")),
            "no deletion toast for a quick deletion: {seen:?}"
        );
        ui.window.close();
    }
}
