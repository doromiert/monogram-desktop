//! Run on a private X11 display and D-Bus with the keyring fixture.
use super::*;
use crate::{model, ui};
use gtk::{gio, glib};
use std::time::Instant;

fn descendants(root: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut found = vec![];
    let mut pending = vec![root.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = pending.pop() {
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            pending.push(current)
        }
        found.push(widget)
    }
    found
}
fn position(ui: &ui::Ui, id: &str) -> f64 {
    ui.bubbles.borrow()[id]
        .compute_bounds(&ui.timeline)
        .unwrap()
        .y() as f64
        - ui.scroll.vadjustment().value()
}
fn focus(ui: &ui::Ui) -> String {
    gtk::prelude::GtkWindowExt::focus(&ui.window)
        .map(|w| {
            format!(
                "{}:{}:{:?}",
                w.type_().name(),
                w.widget_name(),
                w.css_classes()
            )
        })
        .unwrap_or_else(|| "none".into())
}
async fn settle(ui: &ui::Ui) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        glib::timeout_future(Duration::from_millis(20)).await;
        if !ui.rendering_messages() && ui.message_scroll.borrow().is_none() {
            glib::timeout_future(Duration::from_millis(100)).await;
            return;
        }
        assert!(Instant::now() < deadline);
    }
}
fn pointer(ui: &ui::Ui, widget: &impl IsA<gtk::Widget>, button: &str) {
    let b = widget.compute_bounds(&ui.window).unwrap();
    let v = ui.scroll.compute_bounds(&ui.window).unwrap();
    let x = (b.x() + b.width() / 2.).round().to_string();
    let y = if widget.has_css_class("message-bubble") {
        ((b.y().max(v.y()) + (b.y() + b.height()).min(v.y() + v.height())) / 2.)
            .round()
            .to_string()
    } else {
        (b.y() + b.height() / 2.).round().to_string()
    };
    assert!(
        std::process::Command::new("xdotool")
            .args([
                "search",
                "--name",
                "Reaction scroll acceptance",
                "mousemove",
                "--sync",
                "--window",
                "%@",
                &x,
                &y,
                "click",
                button
            ])
            .status()
            .unwrap()
            .success()
    );
}
fn feedback(ui: &ui::Ui, button: &impl IsA<gtk::Button>) -> Rc<Cell<Option<Duration>>> {
    let start = Rc::new(Cell::new(None::<Instant>));
    let clicked = start.clone();
    button.connect_clicked(move |_| clicked.set(Some(Instant::now())));
    let measured = Rc::new(Cell::new(None));
    let result = measured.clone();
    ui.window
        .frame_clock()
        .unwrap()
        .connect_after_paint(move |_| {
            if result.get().is_none() {
                if let Some(start) = start.get() {
                    result.set(Some(start.elapsed()));
                }
            }
        });
    measured
}
#[test]
#[ignore = "requires a private X11 display, D-Bus and xdotool"]
fn pointer_reactions_keep_viewport_and_keyboard_focus() {
    let app = adw::Application::builder()
        .application_id("com.negzero.ReactionScrollAcceptance")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    app.connect_activate(move |app| {
        let ui = ui::Ui::new(app, true);
        let mut state = model::State::live("", vec![]);
        let peer = "peer@fixture.invalid";
        for n in 0..50 {
            state.chat_for(peer).messages.push(model::Message {
                id: format!("message-{n:02}"),
                wire_id: format!("message-{n:02}"),
                sender: peer.into(),
                author: "Fixture".into(),
                kind: "text".into(),
                text: format!(
                    "Message {n}{}",
                    "\nA line of message text".repeat(5 + n % 4)
                ),
                timestamp: format!("2026-11-12T10:{n:02}:00Z"),
                reactions: vec!["👍 1".into()],
                ..Default::default()
            });
        }
        state.active = state.chat_for(peer).id.clone();
        ui.activate_state(state);
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&ui.window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        ui.window.set_title(Some("Reaction scroll acceptance"));
        ui.window.set_default_size(1000, 700);
        ui.window.present();
        let app = app.clone();
        let result = result.clone();
        glib::spawn_future_local(async move {
            settle(&ui).await;
            let adjustment = ui.scroll.vadjustment();
            adjustment.set_value((adjustment.upper() - adjustment.page_size()) * 0.7);
            glib::timeout_future(Duration::from_millis(120)).await;
            ui.composer.grab_focus();
            let anchor = ui
                .timeline
                .row_at_y(adjustment.value() as i32)
                .unwrap()
                .widget_name()
                .to_string();
            let target = ui
                .timeline
                .row_at_y((adjustment.value() + adjustment.page_size() / 2.) as i32)
                .unwrap()
                .widget_name()
                .to_string();
            let before = position(&ui, &anchor);
            let captured_value = adjustment.value();
            let observed = Rc::downgrade(&ui);
            ui.window.connect_focus_widget_notify(move |_| {
                if let Some(ui) = observed.upgrade() {
                    println!(
                        "TRACE focus={} adjustment={:.2}",
                        focus(&ui),
                        ui.scroll.vadjustment().value()
                    );
                }
            });
            let observed = Rc::downgrade(&ui);
            adjustment.connect_value_changed(move |a| {
                if let Some(ui) = observed.upgrade() {
                    println!(
                        "TRACE adjustment={:.2} focus={} pending={}",
                        a.value(),
                        focus(&ui),
                        ui.message_scroll.borrow().is_some()
                    );
                }
            });
            println!(
                "BEFORE menu anchor={anchor} target={target} offset={before:.2} focus={}",
                focus(&ui)
            );
            pointer(&ui, &ui.bubbles.borrow()[&target].clone(), "3");
            glib::timeout_future(Duration::from_millis(200)).await;
            println!(
                "AFTER popup offset={:.2} focus={}",
                position(&ui, &anchor),
                focus(&ui)
            );
            let quick = descendants(&ui.window)
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| {
                    b.has_css_class("reaction-choice")
                        && b.is_mapped()
                        && b.label().as_deref() == Some("👍")
                })
                .expect("actual right-click must open quick choices");
            let emoji = quick.label().unwrap().to_string();
            let quick_feedback = feedback(&ui, &quick);
            let started = Instant::now();
            pointer(&ui, &quick, "1");
            settle(&ui).await;
            let reacted = position(&ui, &anchor);
            assert!(
                ui.state
                    .borrow()
                    .message(&target)
                    .unwrap()
                    .my_reactions
                    .contains(&emoji)
            );
            assert!(quick_feedback.get().unwrap() < Duration::from_millis(400));
            println!("Quick first painted feedback {:?}", quick_feedback.get());
            println!(
                "AFTER quick offset={reacted:.2} delta={:.2} elapsed_ms={} focus={}",
                reacted - before,
                started.elapsed().as_millis(),
                focus(&ui)
            );
            ui.composer.grab_focus();
            glib::timeout_future(Duration::from_millis(300)).await;
            adjustment.set_value(captured_value);
            glib::timeout_future(Duration::from_millis(100)).await;
            let chip_before = position(&ui, &anchor);
            let chip = descendants(&ui.bubbles.borrow()[&target])
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok())
                .find(|b| b.has_css_class("reaction"))
                .unwrap();
            let chip_feedback = feedback(&ui, &chip);
            let started = Instant::now();
            pointer(&ui, &chip, "1");
            settle(&ui).await;
            let unreacted = position(&ui, &anchor);
            assert!(
                !ui.state
                    .borrow()
                    .message(&target)
                    .unwrap()
                    .my_reactions
                    .contains(&emoji)
            );
            assert!(chip_feedback.get().unwrap() < Duration::from_millis(400));
            println!("Chip first painted feedback {:?}", chip_feedback.get());
            println!(
                "AFTER chip offset={unreacted:.2} delta={:.2} elapsed_ms={} focus={}",
                unreacted - chip_before,
                started.elapsed().as_millis(),
                focus(&ui)
            );
            let mut keyboard_ok = true;
            for count in 1..=2 {
                let chip = descendants(&ui.bubbles.borrow()[&target])
                    .into_iter()
                    .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok())
                    .find(|b| b.has_css_class("reaction"))
                    .unwrap();
                chip.grab_focus();
                let keyboard_feedback = feedback(&ui, &chip);
                let before_key = position(&ui, &anchor);
                assert!(
                    std::process::Command::new("xdotool")
                        .args([
                            "search",
                            "--name",
                            "Reaction scroll acceptance",
                            "windowfocus",
                            "--sync",
                            "key",
                            "space"
                        ])
                        .status()
                        .unwrap()
                        .success()
                );
                settle(&ui).await;
                let after_key = position(&ui, &anchor);
                let focus_ok = gtk::prelude::GtkWindowExt::focus(&ui.window)
                    .is_some_and(|w| w.has_css_class("reaction"));
                let expected = count == 1;
                let toggled = ui
                    .state
                    .borrow()
                    .message(&target)
                    .unwrap()
                    .my_reactions
                    .contains(&emoji)
                    == expected;
                println!(
                    "KEYBOARD {count} delta={:.2} focus_same_chip={focus_ok} toggled={toggled}",
                    after_key - before_key
                );
                keyboard_ok &= (after_key - before_key).abs() < 2.
                    && focus_ok
                    && toggled
                    && keyboard_feedback
                        .get()
                        .is_some_and(|elapsed| elapsed < Duration::from_millis(400));
            }
            adjustment.set_value((adjustment.upper() - adjustment.page_size()) / 2.);
            glib::timeout_future(Duration::from_millis(100)).await;
            let wheel_value = adjustment.value();
            let wheel_anchor = ui
                .timeline
                .row_at_y(wheel_value as i32)
                .unwrap()
                .widget_name()
                .to_string();
            let delta = position(&ui, &wheel_anchor);
            let pending = Rc::new(Cell::new(false));
            let seen = pending.clone();
            let weak = Rc::downgrade(&ui);
            let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
            wheel.connect_scroll(move |_, _, _| {
                if let Some(ui) = weak.upgrade() {
                    seen.set(ui.message_scroll.borrow().is_some());
                }
                glib::Propagation::Proceed
            });
            ui.scroll.add_controller(wheel);
            ui.queue_message_scroll(ScrollTarget::Position {
                value: wheel_value,
                anchor: Some((wheel_anchor, delta)),
            });
            pointer(&ui, &ui.scroll, "5");
            glib::timeout_future(Duration::from_millis(220)).await;
            let movement = adjustment.value() - wheel_value;
            println!("WHEEL pending={} moved={movement:.2}", pending.get());
            ui.composer.grab_focus();
            for message in ui.state.borrow_mut().current_mut().messages.iter_mut().rev().take(5) {
                message.reactions.clear();
                message.my_reactions.clear();
                message.text.push_str(&" A naturally wrapping message line near the viewport boundary.".repeat(3));
            }
            ui.render_messages(false);
            settle(&ui).await;
            adjustment.set_value(adjustment.upper() - adjustment.page_size() - 40.);
            glib::timeout_future(Duration::from_millis(100)).await;
            let near_anchor = ui.timeline.row_at_y(adjustment.value() as i32).unwrap().widget_name().to_string();
            let near_target = ui.timeline.row_at_y((adjustment.value() + adjustment.page_size()/2.) as i32).unwrap().widget_name().to_string();
            let near_before = position(&ui, &near_anchor);
            let near_height = ui.bubbles.borrow()[&near_target].height();
            pointer(&ui, &ui.bubbles.borrow()[&near_target].clone(), "3");
            glib::timeout_future(Duration::from_millis(200)).await;
            let quick = descendants(&ui.window).into_iter().filter_map(|widget| widget.downcast::<gtk::Button>().ok()).find(|button| button.has_css_class("reaction-choice") && button.is_mapped() && button.label().as_deref() == Some("👍")).unwrap();
            let painted = feedback(&ui, &quick);
            pointer(&ui, &quick, "1");
            settle(&ui).await;
            let near_reacted = position(&ui, &near_anchor);
            assert!(painted.get().unwrap() < Duration::from_millis(400));
            assert!(ui.state.borrow().message(&near_target).unwrap().my_reactions.contains("👍"));
            let chip = descendants(&ui.bubbles.borrow()[&near_target]).into_iter().filter_map(|widget| widget.downcast::<gtk::ToggleButton>().ok()).find(|button| button.has_css_class("reaction")).unwrap();
            let painted = feedback(&ui, &chip);
            pointer(&ui, &chip, "1");
            settle(&ui).await;
            let exit_deadline = Instant::now() + Duration::from_millis(600);
            while descendants(&ui.bubbles.borrow()[&near_target]).iter().any(|widget| widget.has_css_class("reaction")) {
                assert!(Instant::now() < exit_deadline, "last reaction exit did not finish");
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            settle(&ui).await;
            let near_unreacted = position(&ui, &near_anchor);
            assert!(painted.get().unwrap() < Duration::from_millis(400));
            assert!(ui.state.borrow().message(&near_target).unwrap().reactions.is_empty());
            let fallback = gtk::prelude::GtkWindowExt::focus(&ui.window).is_some_and(|widget| widget.has_css_class("message-text"));
            println!("NEAR BOTTOM quick_delta={:.2} chip_delta={:.2} last_chip_text_fallback={fallback} height={}→{}", near_reacted-near_before, near_unreacted-near_reacted, near_height, ui.bubbles.borrow()[&near_target].height());
            let near_ok = (near_reacted-near_before).abs()<2. && (near_unreacted-near_reacted).abs()<2. && fallback;
            result.set(
                near_ok && (reacted - before).abs() < 2.
                    && (unreacted - chip_before).abs() < 2.
                    && keyboard_ok
                    && pending.get()
                    && movement > 2.,
            );
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    assert!(passed.get(), "actual pointer reaction changed viewport");
}
