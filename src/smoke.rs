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
fn has_label(widget: &gtk::Widget, title: &str) -> bool {
    if widget
        .downcast_ref::<gtk::Label>()
        .is_some_and(|label| label.label() == title)
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        child = w.next_sibling();
        if has_label(&w, title) {
            return true;
        }
    }
    false
}
#[derive(Default)]
struct PollSmoke {
    legacy: Cell<bool>,
    stage: Cell<u8>,
    local: std::cell::RefCell<String>,
    definition: std::cell::RefCell<Option<crate::polls::Poll>>,
}
impl PollSmoke {
    fn start(&self, ui: &Rc<Ui>) {
        fn fields(
            widget: &gtk::Widget,
            editors: &mut Vec<gtk::TextView>,
            entries: &mut Vec<adw::EntryRow>,
            switches: &mut Vec<adw::SwitchRow>,
        ) {
            if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
                editors.push(view.clone());
            }
            if let Some(row) = widget.downcast_ref::<adw::EntryRow>() {
                entries.push(row.clone());
            }
            if let Some(row) = widget.downcast_ref::<adw::SwitchRow>() {
                switches.push(row.clone());
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                fields(&w, editors, entries, switches);
            }
        }
        ui.composer
            .buffer()
            .set_text("Keep the ordinary poll draft");
        ui.save_draft();
        ui.new_poll();
        let dialog = ui.dialogs.borrow().last().unwrap().clone();
        let child = dialog.child().unwrap();
        let (mut editors, mut entries, mut switches) = (Vec::new(), Vec::new(), Vec::new());
        fields(&child, &mut editors, &mut entries, &mut switches);
        assert_eq!(editors.len(), 2);
        assert_eq!(entries.len(), 2);
        editors[0].buffer().set_text("Where for desktop lunch?");
        editors[1].buffer().set_text("Choose a place");
        entries[0].set_text("Pizza");
        entries[1].set_text("Noodles");
        switches
            .iter()
            .find(|s| s.title() == "Include Readable Fallback")
            .unwrap()
            .set_active(false);
        button(&child, "Create Poll").unwrap().emit_clicked();
        assert_eq!(ui.composer_text(), "Keep the ordinary poll draft");
        self.stage.set(1);
    }
    fn incoming(
        &self,
        ui: &Rc<Ui>,
        session: &crate::protocol::Session,
        message: &Message,
        encrypted: bool,
    ) -> bool {
        use crate::{message_actions::Action, protocol::Command};
        if self.stage.get() == 0 {
            return false;
        }
        let owner = ui.state.borrow().account.clone();
        match self.stage.get() {
            1 if message.poll.is_some() => {
                let poll = message.poll.clone().unwrap();
                assert_eq!(poll.question, "Where for desktop lunch?");
                assert_eq!(poll.options[0].label, "Pizza");
                assert_eq!(poll.options[1].label, "Noodles");
                if self.legacy.get() {
                    assert_eq!(message.poll_source(), crate::polls::Source::Legacy);
                    assert!(crate::legacy_polls::parse(&message.text).is_some());
                } else {
                    assert!(message.text.is_empty());
                    assert_eq!(message.poll_source(), crate::polls::Source::Structured);
                }
                assert_eq!(message.encrypted, encrypted);
                self.local.replace(
                    ui.state
                        .borrow()
                        .current()
                        .messages
                        .iter()
                        .find(|m| m.wire_id == message.wire_id)
                        .unwrap()
                        .id
                        .clone(),
                );
                let pending = ui
                    .state
                    .borrow()
                    .message(&self.local.borrow())
                    .unwrap()
                    .clone();
                ui.poll_voters(&pending);
                assert!(has_label(
                    &ui.dialogs.borrow().last().unwrap().child().unwrap(),
                    "Public votes"
                ));
                close_dialogs(ui);
                self.definition.replace(Some(poll.clone()));
                session.send(Command::Send {
                    peer: owner,
                    message: Message {
                        wire_id: new_id(),
                        encrypted,
                        action: Some(Action::React {
                            target: message.wire_id.clone(),
                            emojis: [poll.options[1].emoji.clone()].into(),
                        }),
                        ..Default::default()
                    },
                    quote: String::new(),
                });
                self.stage.set(2);
                true
            }
            3 if matches!(message.action, Some(Action::React { .. })) => {
                let Action::React { emojis, .. } = message.action.as_ref().unwrap() else {
                    unreachable!()
                };
                let poll = self.definition.borrow();
                assert_eq!(
                    emojis,
                    &[poll.as_ref().unwrap().options[0].emoji.clone()].into()
                );
                self.stage.set(4);
                true
            }
            5 if matches!(message.action, Some(Action::Correct { .. })) => {
                assert_eq!(
                    message.poll_source() == crate::polls::Source::Legacy,
                    self.legacy.get()
                );
                assert!(
                    message.poll.as_ref().is_some_and(|p| p.closed
                        && p.same_definition(self.definition.borrow().as_ref().unwrap()))
                );
                self.stage.set(6);
                true
            }
            9 if message.poll.is_some() => {
                assert_eq!(message.poll_source(), crate::polls::Source::Structured);
                assert!(!message.poll.as_ref().unwrap().closed);
                assert!(message.text.is_empty());
                assert!(
                    message
                        .poll
                        .as_ref()
                        .unwrap()
                        .same_definition(self.definition.borrow().as_ref().unwrap())
                );
                assert_ne!(
                    ui.state
                        .borrow()
                        .message(&self.local.borrow())
                        .unwrap()
                        .wire_id,
                    message.wire_id
                );
                self.stage.set(10);
                true
            }
            _ => false,
        }
    }
    fn tick(&self, ui: &Rc<Ui>, session: &crate::protocol::Session, encrypted: bool) -> bool {
        use crate::{message_actions::Action, protocol::Command};
        let id = self.local.borrow().clone();
        let message = ui.state.borrow().message(&id).cloned();
        match (self.stage.get(), message) {
            (2, Some(m))
                if m.poll_tally
                    .as_ref()
                    .is_some_and(|t| t.counts == vec![0, 1]) =>
            {
                let bubble = ui.bubbles.borrow()[&id].clone();
                if self.legacy.get() {
                    assert!(!has_label(&bubble, &m.text));
                    assert!(has_label(&bubble, "Choose a place"));
                }
                button(&bubble, "Pizza").unwrap().emit_clicked();
                self.stage.set(3);
            }
            (4, Some(m))
                if m.poll_tally
                    .as_ref()
                    .is_some_and(|t| t.counts == vec![1, 1]) =>
            {
                ui.poll_voters(&m);
                let child = ui.dialogs.borrow().last().unwrap().child().unwrap();
                assert!(has_label(&child, "Public votes"));
                close_dialogs(ui);
                let bubble = ui.bubbles.borrow()[&id]
                    .clone()
                    .downcast::<gtk::Box>()
                    .unwrap();
                ui.message_menu(&bubble, &id, 0., 0.);
                let mut child = bubble.first_child();
                let mut clicked = false;
                while let Some(w) = child {
                    child = w.next_sibling();
                    if let Some(b) = button(&w, "Close Poll") {
                        b.emit_clicked();
                        clicked = true;
                        break;
                    }
                }
                assert!(clicked);
                self.stage.set(5);
            }
            (6, Some(m)) if m.poll.as_ref().is_some_and(|p| p.closed) => {
                session.send(Command::Send {
                    peer: ui.state.borrow().account.clone(),
                    message: Message {
                        wire_id: new_id(),
                        encrypted,
                        action: Some(Action::React {
                            target: m.wire_id,
                            emojis: [self.definition.borrow().as_ref().unwrap().options[0]
                                .emoji
                                .clone()]
                            .into(),
                        }),
                        ..Default::default()
                    },
                    quote: String::new(),
                });
                self.stage.set(7);
            }
            (7, Some(m))
                if m.reactions.iter().any(|r| {
                    r == &format!(
                        "{} 2",
                        crate::message_actions::canonical_emoji(
                            &self.definition.borrow().as_ref().unwrap().options[0].emoji
                        )
                        .unwrap()
                    )
                }) =>
            {
                assert_eq!(m.poll_tally.as_ref().unwrap().counts, vec![1, 1]);
                assert_eq!(m.poll_tally.as_ref().unwrap().ballots.len(), 2);
                assert!(m.poll.as_ref().unwrap().closed);
                let state = ui.store.load(&ui.state.borrow().account);
                let restored = state
                    .chats
                    .iter()
                    .flat_map(|c| &c.messages)
                    .find(|m| m.id == id)
                    .unwrap();
                assert_eq!(restored.poll_tally.as_ref().unwrap().counts, vec![1, 1]);
                assert_eq!(ui.composer_text(), "Keep the ordinary poll draft");
                if self.legacy.get() {
                    assert_eq!(restored.poll_source(), crate::polls::Source::Legacy);
                    ui.forward(vec![m]);
                    let dialog = ui.dialogs.borrow().last().unwrap().child().unwrap();
                    fn activate(widget: &gtk::Widget, title: &str) -> bool {
                        if let Some(row) = widget.downcast_ref::<adw::ActionRow>()
                            && row.title() == title
                        {
                            row.emit_by_name::<()>("activated", &[]);
                            return true;
                        }
                        let mut child = widget.first_child();
                        while let Some(w) = child {
                            child = w.next_sibling();
                            if activate(&w, title) {
                                return true;
                            }
                        }
                        false
                    }
                    let name = ui.state.borrow().current().name.clone();
                    self.stage.set(9);
                    assert!(activate(&dialog, &name));
                    return false;
                }
                println!(
                    "Native structured poll: bodyless composer, explicit choices, public remote/local votes, voter list, authenticated close, cutoff and reload passed (encrypted={encrypted})"
                );
                self.legacy.set(true);
                let poll = crate::polls::Poll {
                    question: "Where for desktop lunch?".into(),
                    description: "Choose a place".into(),
                    options: vec![
                        crate::polls::OptionValue {
                            emoji: crate::legacy_polls::EMOJI[0].into(),
                            label: "Pizza".into(),
                        },
                        crate::polls::OptionValue {
                            emoji: crate::legacy_polls::EMOJI[1].into(),
                            label: "Noodles".into(),
                        },
                    ],
                    multiple: false,
                    closed: false,
                };
                ui.composer
                    .buffer()
                    .set_text(&crate::legacy_polls::body(&poll).unwrap());
                ui.send();
                ui.composer
                    .buffer()
                    .set_text("Keep the ordinary poll draft");
                ui.save_draft();
                self.local.borrow_mut().clear();
                self.stage.set(1);
            }
            (10, _) => {
                assert_eq!(ui.composer_text(), "Keep the ordinary poll draft");
                println!(
                    "Native historical poll: canonical body, votes/voters, legacy closure/cutoff/reload and fresh structured forwarding passed (encrypted={encrypted})"
                );
                return true;
            }
            _ => {}
        }
        false
    }
}

fn button(widget: &gtk::Widget, title: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.label().as_deref() == Some(title)
            || button.child().is_some_and(|child| has_label(&child, title))
        {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        if let Some(found) = button(&w, title) {
            return Some(found);
        }
        child = w.next_sibling();
    }
    None
}
fn edit_control(ui: &Rc<Ui>, id: &str, value: &str) {
    ui.edit_message(id);
    let bubble = ui.bubbles.borrow()[id].clone();
    let mut child = bubble.first_child();
    let mut edited = false;
    while let Some(w) = child {
        child = w.next_sibling();
        if let Some(scroll) = w.downcast_ref::<gtk::ScrolledWindow>() {
            if let Some(editor) = scroll.child().and_downcast::<gtk::TextView>() {
                editor.buffer().set_text(value);
                edited = true;
            }
        }
    }
    assert!(edited, "inline edit control missing");
    button(&bubble, "Save")
        .expect("inline Save button")
        .emit_clicked();
}

fn crypto_history_failures(ui: &Rc<Ui>) {
    let owner = ui.state.borrow().account.clone();
    let peer = ui.state.borrow().current().jid.clone();
    let id = new_id();
    let message = Message {
        id: id.clone(),
        text: "Persistence fixture".into(),
        wire_id: id.clone(),
        aliases: vec![format!("wire:{id}")],
        encrypted: true,
        ..Default::default()
    };
    ui.store.db.execute_batch(&format!("CREATE TEMP TRIGGER fixture_history_failure BEFORE INSERT ON messages WHEN NEW.id='{id}' BEGIN SELECT RAISE(ABORT,'fixture disk failure');END;")).unwrap();
    for _ in 0..2 {
        assert!(!ui.receive_message(&owner, peer.clone(), message.clone(), false, None));
    }
    assert_eq!(
        ui.state
            .borrow()
            .current()
            .messages
            .iter()
            .filter(|m| m.id == id)
            .count(),
        1,
        "Persistence retries duplicated the in-memory message"
    );
    assert!(ui.store.message(&owner, &peer, &id).is_none());
    ui.store
        .db
        .execute_batch("DROP TRIGGER fixture_history_failure;")
        .unwrap();
    assert!(ui.persist());
    let mut unavailable = Message {
        text: "Not decrypted".into(),
        unavailable: true,
        ciphertext: "fictional ciphertext".into(),
        ..Default::default()
    };
    unavailable.wire_id = unavailable.id.clone();
    unavailable.aliases = vec![format!("wire:{}", unavailable.id)];
    let original = unavailable.id.clone();
    assert!(ui.receive_message(&owner, peer.clone(), unavailable.clone(), false, None));
    let unread = ui.state.borrow().current().unread;
    let mut decoded = unavailable.clone();
    decoded.id = new_id();
    decoded.text = "Recovered fixture".into();
    decoded.unavailable = false;
    decoded.encrypted = true;
    decoded.ciphertext.clear();
    decoded.aliases.push("omemo2:fictional-recovery".into());
    assert!(ui.receive_message(&owner, peer.clone(), decoded, false, None));
    assert_eq!(ui.state.borrow().current().unread, unread);
    let recovered = ui.store.message(&owner, &peer, &original).unwrap();
    assert_eq!(recovered.text, "Recovered fixture");
    assert!(recovered.ciphertext.is_empty());
    assert!(recovered.encrypted);
    assert!(ui.receive_message(&owner, peer.clone(), unavailable, false, None));
    assert_eq!(
        ui.store.message(&owner, &peer, &original).unwrap().text,
        "Recovered fixture",
        "A replayed placeholder replaced authenticated text"
    );
    for row in [&id, &original] {
        ui.store.delete(&owner, &peer, row).unwrap();
    }
    ui.state
        .borrow_mut()
        .current_mut()
        .messages
        .retain(|m| m.id != id && m.id != original);
    assert!(ui.persist());
}

/// Actual account/security widgets and composer, on a private bus, keyring and server.
pub fn run_crypto(ui: Rc<Ui>) {
    use crate::{
        crypto::service::Request,
        protocol::{BackendEvent, Command, Session},
    };
    use std::{cell::RefCell, collections::BTreeMap, sync::mpsc, time::Instant};
    assert_eq!(
        std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
        Ok("1")
    );
    let config = Account {
        jid: "erin@localhost".into(),
        host: "127.0.0.1".into(),
        port: std::env::var("MONOGRAM_IT_PORT").unwrap().parse().unwrap(),
        direct_tls: false,
    };
    let other = Account {
        jid: "frank@localhost".into(),
        ..config.clone()
    };
    ui.store.save_account(&other).unwrap();
    let (tx, rx) = mpsc::channel();
    let frank = RefCell::new(Some(Session::start(
        other,
        "fixture-password".into(),
        31,
        tx,
    )));
    let stage = Cell::new(0);
    let poll_smoke = PollSmoke::default();
    let frank_online = Cell::new(false);
    let frank_ready = Cell::new(false);
    let frank_trusted = Cell::new(false);
    let deadline = Instant::now();
    ui.store
        .set_pref("notifications:erin@localhost", "false")
        .unwrap();
    ui.connect_account(config.clone(), "fixture-password".into());
    let chat = ui.state.borrow_mut().chat_for("frank@localhost").id.clone();
    ui.select_chat(&chat);
    glib::timeout_add_local(Duration::from_millis(80), move || {
        assert!(
            deadline.elapsed() < Duration::from_secs(110),
            "Native crypto stage {} timed out",
            stage.get()
        );
        while let Ok((_, _, event)) = rx.try_recv() {
            match event {
                BackendEvent::Status { online, .. } => frank_online.set(online),
                BackendEvent::CryptoStatus(status) => frank_ready.set(status.ready),
                BackendEvent::CryptoReview { rows, errors, .. } => {
                    assert!(errors.is_empty(), "{errors:?}");
                    if rows.iter().all(|r| r.trust == "Trusted") {
                        frank_trusted.set(true)
                    } else {
                        let mut keys: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
                        for row in rows {
                            keys.entry(row.peer)
                                .or_default()
                                .insert(row.device, row.key);
                        }
                        frank
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .send(Command::Crypto(Request::Decide {
                                keys,
                                approve: true,
                            }));
                    }
                }
                BackendEvent::EncryptedMessage { message, token, .. } => {
                    assert!(message.encrypted && !message.unavailable);
                    if message.action.is_some() {
                        ui.store
                            .record_action(
                                "frank@localhost",
                                "erin@localhost",
                                &message,
                                "received",
                            )
                            .unwrap();
                        ui.store
                            .reconcile_actions("frank@localhost", "erin@localhost")
                            .unwrap();
                    } else {
                        let mut state = ui.store.load("frank@localhost");
                        state
                            .chat_for("erin@localhost")
                            .messages
                            .push(message.clone());
                        ui.store.save(&state).unwrap();
                    }
                    frank
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .send(Command::Crypto(Request::Accepted(token)));
                    if poll_smoke.incoming(&ui, frank.borrow().as_ref().unwrap(), &message, true) {
                        continue;
                    }
                    if let Some(action) = &message.action {
                        use crate::message_actions::Action;
                        match (stage.get(), action) {
                            (8, Action::Correct { target }) => {
                                assert_eq!(message.text, "Encrypted native edit");
                                frank.borrow().as_ref().unwrap().send(Command::Send {
                                    peer: "erin@localhost".into(),
                                    message: Message {
                                        wire_id: new_id(),
                                        action: Some(Action::React {
                                            target: target.clone(),
                                            emojis: ["👍".into()].into(),
                                        }),
                                        encrypted: true,
                                        ..Default::default()
                                    },
                                    quote: String::new(),
                                });
                                stage.set(9);
                            }
                            (10, Action::React { target, emojis }) => {
                                assert!(emojis.contains("❤️"));
                                let m = ui
                                    .state
                                    .borrow()
                                    .current()
                                    .messages
                                    .iter()
                                    .find(|m| {
                                        m.wire_id == *target
                                            || m.aliases.contains(&format!("origin:{target}"))
                                    })
                                    .unwrap()
                                    .clone();
                                assert!(ui.send_action(
                                    &m.id,
                                    Action::Retract {
                                        target: m.wire_id,
                                        legacy: false
                                    },
                                    ""
                                ));
                                stage.set(11);
                            }
                            (11, Action::Retract { .. }) => stage.set(12),
                            _ => {}
                        }
                        continue;
                    }
                    if message.text == "Native encrypted composer 🌿" {
                        frank.borrow().as_ref().unwrap().send(Command::Send {
                            peer: "erin@localhost".into(),
                            message: Message {
                                text: "Native encrypted reply 🦊".into(),
                                wire_id: new_id(),
                                reply_id: message.wire_id,
                                reply_to: "erin@localhost".into(),
                                encrypted: true,
                                ..Default::default()
                            },
                            quote: message.text,
                        });
                    } else if message.text == "Encrypted after restoration" {
                        assert_eq!(stage.get(), 6);
                        let id = ui
                            .state
                            .borrow()
                            .current()
                            .messages
                            .iter()
                            .find(|m| m.wire_id == message.wire_id)
                            .unwrap()
                            .id
                            .clone();
                        ui.composer.buffer().set_text("Preserve encrypted draft");
                        edit_control(&ui, &id, "Encrypted native edit");
                        stage.set(8);
                    }
                }
                _ => {}
            }
        }
        if poll_smoke.tick(&ui, frank.borrow().as_ref().unwrap(), true) {
            frank.borrow_mut().take();
            ui.window.close();
            return glib::ControlFlow::Break;
        }
        match stage.get() {
            0 if ui.online.get() && frank_online.get() => {
                ui.security();
                let enable = ui
                    .security_widgets
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .enable
                    .clone();
                enable.emit_clicked();
                frank
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .send(Command::Crypto(Request::Enable));
                stage.set(1);
            }
            1 if ui.crypto.borrow().ready && frank_ready.get() => {
                let widgets = ui.security_widgets.borrow();
                let w = widgets.as_ref().unwrap();
                assert!(!w.enable.is_visible());
                w.encrypted.set_active(true);
                w.review.emit_clicked();
                drop(widgets);
                frank
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .send(Command::Crypto(Request::Review("erin@localhost".into())));
                stage.set(2);
            }
            2 => {
                let dialogs = ui.dialogs.borrow().clone();
                if let Some(trust) = dialogs
                    .last()
                    .and_then(|d| d.child())
                    .and_then(|w| button(&w, "Trust Shown"))
                {
                    trust.emit_clicked();
                    stage.set(3);
                }
            }
            3 if frank_trusted.get() => {
                assert!(ui.state.borrow().current().encryption);
                ui.composer
                    .buffer()
                    .set_text("Native encrypted composer 🌿");
                ui.send();
                stage.set(4);
            }
            4 => {
                if let Some(reply) = ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .find(|m| m.text == "Native encrypted reply 🦊")
                    .cloned()
                {
                    assert!(reply.encrypted && !reply.unavailable);
                    assert!(reply.reply.contains("Native encrypted composer"));
                    assert!(reply.aliases.iter().any(|v| v.starts_with("omemo2:")));
                    ui.persist();
                    stage.set(5);
                }
            }
            5 => {
                crypto_history_failures(&ui);
                ui.activate_state(State::demo());
                ui.connect_account(config.clone(), "fixture-password".into());
                ui.select_chat(&chat);
                stage.set(7);
            }
            9 => {
                let m = ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .find(|m| {
                        m.text == "Encrypted native edit" && m.reactions.iter().any(|r| r == "👍 1")
                    })
                    .cloned();
                if let Some(m) = m {
                    assert!(m.edited && m.encrypted);
                    ui.react(&m.id, "❤️");
                    stage.set(10);
                }
            }
            12 => {
                if !ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .any(|m| m.text == "Encrypted native edit")
                {
                    assert_eq!(ui.composer_text(), "Preserve encrypted draft");
                    let loaded = ui.store.load(&config.jid);
                    assert!(
                        !loaded
                            .chats
                            .iter()
                            .flat_map(|c| &c.messages)
                            .any(|m| m.text == "Encrypted native edit")
                    );
                    println!(
                        "Native encrypted controls: explicit setup/trust, composer/reply, durable recovery, protected inline edit, reactions, original-device retraction and reload passed"
                    );
                    poll_smoke.start(&ui);
                    stage.set(13);
                }
            }
            7 if ui.online.get()
                && ui.crypto.borrow().ready
                && !ui.history_pending.borrow().contains("frank@localhost") =>
            {
                assert!(ui.state.borrow().current().encryption);
                assert_eq!(
                    ui.state
                        .borrow()
                        .current()
                        .messages
                        .iter()
                        .filter(|m| m.text == "Native encrypted reply 🦊")
                        .count(),
                    1
                );
                ui.composer.buffer().set_text("Encrypted after restoration");
                ui.send();
                stage.set(6);
            }
            _ => {}
        }
        glib::ControlFlow::Continue
    });
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
    let poll_smoke = PollSmoke::default();
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
                    if poll_smoke.incoming(&ui, bob.borrow().as_ref().unwrap(), &message, false) {
                        continue;
                    }
                    if let Some(action) = &message.action {
                        use crate::message_actions::Action;
                        match (stage.get(), action) {
                            (5, Action::Correct { target }) => {
                                assert_eq!(message.text, "Edited native message");
                                bob.borrow().as_ref().unwrap().send(Command::Send {
                                    peer: "alice@localhost".into(),
                                    message: Message {
                                        wire_id: new_id(),
                                        action: Some(Action::React {
                                            target: target.clone(),
                                            emojis: ["👍".into()].into(),
                                        }),
                                        ..Default::default()
                                    },
                                    quote: String::new(),
                                });
                                stage.set(6);
                            }
                            (7, Action::React { target, emojis }) => {
                                assert!(emojis.contains("❤️"));
                                let original = ui
                                    .state
                                    .borrow()
                                    .current()
                                    .messages
                                    .iter()
                                    .find(|m| {
                                        m.wire_id == *target
                                            || m.aliases.contains(&format!("origin:{target}"))
                                    })
                                    .unwrap()
                                    .clone();
                                assert!(ui.send_action(
                                    &original.id,
                                    Action::Retract {
                                        target: original.wire_id,
                                        legacy: false
                                    },
                                    ""
                                ));
                                stage.set(8);
                            }
                            (8, Action::Retract { .. }) => {
                                stage.set(9);
                            }
                            _ => {}
                        }
                        continue;
                    }
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
                        ui.composer
                            .buffer()
                            .set_text("Keep this draft while editing");
                        let id = ui
                            .state
                            .borrow()
                            .current()
                            .messages
                            .iter()
                            .find(|m| m.wire_id == message.wire_id)
                            .unwrap()
                            .id
                            .clone();
                        edit_control(&ui, &id, "Edited native message");
                        assert_eq!(ui.composer_text(), "Keep this draft while editing");
                        stage.set(5);
                    }
                }
                _ => {}
            }
        }
        if poll_smoke.tick(&ui, bob.borrow().as_ref().unwrap(), false) {
            bob.borrow_mut().take();
            ui.window.close();
            return glib::ControlFlow::Break;
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
            6 => {
                let original = ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .find(|m| {
                        m.text == "Edited native message" && m.reactions.iter().any(|r| r == "👍 1")
                    })
                    .cloned();
                if let Some(m) = original {
                    assert!(m.edited);
                    assert_eq!(
                        m.edit_history,
                        vec!["After restoration", "Edited native message"]
                    );
                    let bubble = ui.bubbles.borrow()[&m.id]
                        .clone()
                        .downcast::<gtk::Box>()
                        .unwrap();
                    ui.message_menu(&bubble, &m.id, 0., 0.);
                    let mut child = bubble.first_child();
                    let mut chosen = false;
                    while let Some(w) = child {
                        child = w.next_sibling();
                        if let Some(b) = button(&w, "❤️") {
                            b.emit_clicked();
                            chosen = true;
                            break;
                        }
                    }
                    assert!(chosen, "live reaction menu missing");
                    stage.set(7);
                }
            }
            9 => {
                if !ui
                    .state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .any(|m| m.text == "Edited native message")
                {
                    let loaded = ui.store.load(&config.jid);
                    assert!(
                        !loaded
                            .chats
                            .iter()
                            .flat_map(|c| &c.messages)
                            .any(|m| m.text == "Edited native message")
                    );
                    assert_eq!(ui.composer_text(), "Keep this draft while editing");
                    println!(
                        "Native live Rust controls: inline correction, preserved composer, remote reaction, narrow message menu, local reaction and confirmed retraction/reload passed"
                    );
                    poll_smoke.start(&ui);
                    stage.set(10);
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
