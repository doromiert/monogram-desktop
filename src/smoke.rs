//! Exercises the real native widgets using fictional data and an in-memory DB.
use crate::{model::*, ui::Ui};
use adw::prelude::*;
use gtk::glib;
use std::{cell::Cell, rc::Rc, time::Duration};

fn layout(ui: &Ui) {
    let s = ui.state.borrow();
    let bubbles = ui.bubbles.borrow();
    let mut previous_bottom = None;
    for message in &s.current().messages {
        let widget = &bubbles[&message.id];
        let bounds = widget.compute_bounds(&ui.timeline).expect("mapped bubble");
        if let Some(bottom) = previous_bottom {
            let gap = bounds.y() - bottom;
            assert!(
                (0. ..=30.).contains(&gap),
                "unexpected message gap: {gap}px"
            );
        }
        if message.kind == "photo" {
            assert!(
                bounds.height() < 360.,
                "photo row expanded: {}px",
                bounds.height()
            );
        }
        previous_bottom = Some(bounds.y() + bounds.height());
    }
    let (_, natural, _, _) = ui
        .timeline
        .measure(gtk::Orientation::Vertical, ui.timeline.width());
    assert!(
        ui.timeline.height() <= natural + 2,
        "timeline absorbed spare viewport height"
    );
}

fn close_dialogs(ui: &Ui) {
    let dialogs = std::mem::take(&mut *ui.dialogs.borrow_mut());
    for dialog in dialogs {
        dialog.close();
    }
}

pub fn run(ui: Rc<Ui>) {
    let step = Cell::new(0);
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let stage = step.get();
        step.set(stage + 1);
        match stage {
            0 => {
                let mira = ui
                    .state
                    .borrow()
                    .chats
                    .iter()
                    .find(|c| c.name == "Mira")
                    .unwrap()
                    .id
                    .clone();
                ui.select_chat(&mira);
                ui.window.set_default_size(1920, 1080);
                ui.folder_bar.set_width_request(700);
                ui.stories.set_width_request(700);
            }
            1 => {
                layout(&ui);
                for widget in [
                    ui.folder_bar.clone().upcast::<gtk::Widget>(),
                    ui.stories.clone().upcast(),
                ] {
                    let mut parent = widget.parent();
                    while let Some(p) = parent {
                        if let Ok(strip) = p.clone().downcast::<gtk::ScrolledWindow>() {
                            let controllers = strip.observe_controllers();
                            let controller = (0..controllers.n_items())
                                .filter_map(|i| {
                                    controllers
                                        .item(i)
                                        .and_downcast::<gtk::EventControllerScroll>()
                                })
                                .find(|c| c.propagation_phase() == gtk::PropagationPhase::Capture)
                                .expect("strip wheel controller");
                            let handled: bool = controller.emit_by_name("scroll", &[&0f64, &1f64]);
                            assert!(
                                handled && strip.hadjustment().value() > 0.,
                                "vertical wheel must scroll strip horizontally"
                            );
                            break;
                        }
                        parent = p.parent();
                    }
                    widget.set_width_request(-1);
                }
                ui.window.set_default_size(960, 760);
            }
            2 => {
                layout(&ui);
                assert!(!ui.split.is_collapsed());
                ui.window.set_default_size(460, 720);
            }
            3 => {
                layout(&ui);
                assert!(ui.split.is_collapsed() && ui.utility.is_collapsed());
                ui.window.set_default_size(1380, 900);
                ui.set_folder("Unread");
            }
            4 => {
                ui.set_folder("All");
                // Nine real fixture messages exercise all grid positions.
                let mut s = ui.state.borrow_mut();
                let template = s
                    .current()
                    .messages
                    .iter()
                    .find(|m| m.kind == "photo")
                    .unwrap()
                    .clone();
                for _ in 0..8 {
                    let mut m = template.clone();
                    m.id = new_id();
                    s.current_mut().messages.push(m);
                }
                drop(s);
                ui.shared_media();
                assert!(
                    ui.active_media.borrow().is_none(),
                    "grid must not open viewer"
                );
            }
            5 => {
                let pane = ui
                    .utility
                    .sidebar()
                    .unwrap()
                    .downcast::<adw::ToolbarView>()
                    .unwrap();
                let grid = pane
                    .content()
                    .unwrap()
                    .downcast::<gtk::ScrolledWindow>()
                    .unwrap()
                    .child()
                    .unwrap()
                    .first_child()
                    .unwrap_or_else(|| panic!("grid content"));
                // ScrolledWindow may insert a viewport around the grid.
                let grid = if grid.is::<gtk::Grid>() {
                    grid
                } else {
                    pane.content()
                        .unwrap()
                        .downcast::<gtk::ScrolledWindow>()
                        .unwrap()
                        .child()
                        .unwrap()
                };
                let grid = grid.downcast::<gtk::Grid>().expect("media grid");
                for y in 0..3 {
                    for x in 0..3 {
                        assert!(grid.child_at(x, y).is_some());
                    }
                }
                grid.child_at(1, 1)
                    .unwrap()
                    .downcast::<gtk::Button>()
                    .unwrap()
                    .emit_clicked();
            }
            6 => {
                let viewer = ui
                    .active_media
                    .borrow()
                    .clone()
                    .expect("thumbnail opens viewer");
                viewer.close();
            }
            7 => {
                assert!(ui.active_media.borrow().is_none());
                ui.utility.set_show_sidebar(false);
                ui.edit_folder("Unread");
            }
            8 => {
                close_dialogs(&ui);
                ui.shortcuts();
            }
            9 => {
                close_dialogs(&ui);
                ui.preferences();
            }
            10 => {
                close_dialogs(&ui);
                ui.wallet();
            }
            11 => {
                for card in ui.cards.borrow().iter() {
                    assert!(card.area.error().is_none(), "wallet GL failed");
                }
                ui.toggle_media();
                ui.media_stack.set_visible_child_name("monero");
            }
            12 => {
                ui.show_chat();
                ui.utility.set_show_sidebar(false);
                ui.open_story("Mira");
            }
            13 => {
                let story = ui.active_story.borrow().clone().expect("story viewer");
                story.close();
            }
            14 => {
                assert!(ui.active_story.borrow().is_none());
                println!(
                    "Native Rust smoke passed: compact photo rows at 1920/960/460px, breakpoints, folder transition, 3×3 media grid → viewer → close, native dialogs, GL wallets and stories"
                );
                ui.window.close();
                return glib::ControlFlow::Break;
            }
            _ => unreachable!(),
        }
        glib::ControlFlow::Continue
    });
}

/// The actual composer and event handler, connected only to the loopback fixture.
pub fn run_live(ui: Rc<Ui>) {
    use crate::protocol::{BackendEvent, Command, Session};
    use std::{cell::RefCell, sync::mpsc, time::Instant};
    let port = std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap();
    let config = Account {
        jid: "alice@localhost".into(),
        host: "127.0.0.1".into(),
        port,
        direct_tls: false,
    };
    let (tx, rx) = mpsc::channel();
    let bob = RefCell::new(Some(Session::start(
        Account {
            jid: "bob@localhost".into(),
            ..config.clone()
        },
        "fixture-password".into(),
        77,
        tx,
    )));
    ui.store
        .set_pref("notifications:alice@localhost", "false")
        .unwrap();
    ui.connect_account(config.clone(), "fixture-password".into());
    let peer = ui.state.borrow_mut().chat_for("bob@localhost").id.clone();
    ui.select_chat(&peer);
    let stage = Cell::new(0);
    let bob_online = Cell::new(false);
    let started = Instant::now();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "native live stage {} timed out",
            stage.get()
        );
        for (_, _, event) in rx.try_iter() {
            match event {
                BackendEvent::Status { online, .. } => bob_online.set(online),
                BackendEvent::Message {
                    message,
                    historical: false,
                    ..
                } if !message.own => {
                    if stage.get() == 1 && message.text == "Native Rust composer 🌿" {
                        let reply = Message {
                            text: "Native fixture reply 🦊".into(),
                            wire_id: new_id(),
                            reply_id: message.wire_id.clone(),
                            reply_to: "alice@localhost".into(),
                            ..Default::default()
                        };
                        bob.borrow().as_ref().unwrap().send(Command::Receipt {
                            peer: "alice@localhost".into(),
                            id: message.wire_id,
                        });
                        bob.borrow().as_ref().unwrap().send(Command::Send {
                            peer: "alice@localhost".into(),
                            message: reply,
                            quote: message.text,
                        });
                        stage.set(2);
                    } else if stage.get() == 4 && message.text == "After restoration" {
                        println!(
                            "Native live Rust composer: TLS send/reply, delivery state, draft/reply restoration, account isolation, stale callback rejection and MAM dedup passed"
                        );
                        bob.borrow_mut().take();
                        ui.window.close();
                        return glib::ControlFlow::Break;
                    }
                }
                _ => {}
            }
        }
        match stage.get() {
            0 if ui.online.get() && bob_online.get() => {
                assert!(!ui.window.lookup_action("attach-file").unwrap().is_enabled());
                ui.composer.buffer().set_text("Native Rust composer 🌿");
                ui.send();
                assert!(ui.composer_text().is_empty());
                stage.set(1);
            }
            2 => {
                let reply = ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .find(|m| m.text == "Native fixture reply 🦊")
                    .cloned();
                if let Some(reply) = reply {
                    ui.replying.replace(Some(reply.id.clone()));
                    ui.update_reply();
                    ui.composer.buffer().set_text("Durable draft 🌿");
                    ui.save_draft();
                    let generation = ui.generation.get();
                    let count = ui.state.borrow().current().messages.len();
                    let stale = BackendEvent::Message {
                        peer: "bob@localhost".into(),
                        message: Message {
                            text: "Must be rejected".into(),
                            ..Default::default()
                        },
                        historical: false,
                        receipt_to: None,
                    };
                    ui.backend_event(generation - 1, &config.jid, stale.clone());
                    ui.backend_event(generation, "foreign@localhost", stale);
                    assert_eq!(ui.state.borrow().current().messages.len(), count);
                    ui.activate_state(State::demo());
                    assert!(
                        ui.state
                            .borrow()
                            .chats
                            .iter()
                            .flat_map(|c| &c.messages)
                            .all(|m| m.text != "Durable draft 🌿")
                    );
                    ui.connect_account(config.clone(), "fixture-password".into());
                    ui.select_chat(&peer);
                    assert_eq!(ui.composer_text(), "Durable draft 🌿");
                    assert_eq!(ui.replying.borrow().as_deref(), Some(reply.id.as_str()));
                    stage.set(3);
                }
            }
            3 if ui.online.get() && !ui.history_pending.borrow().contains("bob@localhost") => {
                assert_eq!(
                    ui.state
                        .borrow()
                        .current()
                        .messages
                        .iter()
                        .filter(|m| m.text == "Native fixture reply 🦊")
                        .count(),
                    1
                );
                ui.composer.buffer().set_text("After restoration");
                ui.send();
                stage.set(4);
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
}
