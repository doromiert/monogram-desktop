use crate::{
    message_actions::{self as actions, Action},
    model::*,
    polls::{Poll, Tally},
    ui::Ui,
    widgets::*,
};
use adw::prelude::*;
use std::rc::Rc;
impl Ui {
    pub fn poll_content(self: &Rc<Self>, message: &Message) -> gtk::Box {
        let poll = message.poll.as_ref().unwrap();
        let tally = message.poll_tally.clone().unwrap_or_else(|| Tally {
            counts: vec![0; poll.options.len()],
            ..Default::default()
        });
        let body = bx(true, 6);
        body.append(&wrap(&poll.question, "heading"));
        body.append(&text(
            if poll.closed {
                "Public poll · closed"
            } else if poll.multiple {
                "Public poll · choose any"
            } else {
                "Public poll · choose one"
            },
            "caption dim-label",
        ));
        if !poll.description.is_empty() {
            body.append(&wrap(&poll.description, ""));
        }
        if !message.text.is_empty() && message.poll_source() != crate::polls::Source::Legacy {
            body.append(&wrap(&message.text, ""));
        }
        for (index, option) in poll.options.iter().enumerate() {
            let button = gtk::ToggleButton::new();
            button.add_css_class("poll-answer");
            let count = tally.counts.get(index).copied().unwrap_or_default();
            let line = bx(false, 16);
            let label = wrap(&option.label, "");
            label.set_hexpand(true);
            line.append(&label);
            line.append(&text(&count.to_string(), ""));
            button.set_child(Some(&line));
            button.set_active(tally.chosen.contains(&index));
            button.set_sensitive(!poll.closed);
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "{}: {} votes",
                option.label, count
            ))]);
            let weak = Rc::downgrade(self);
            let id = message.id.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.vote_poll(&id, index);
                }
            });
            body.append(&button);
        }
        let results = gtk::Button::with_label(&format!("{} voters", tally.ballots.len()));
        results.add_css_class("flat");
        results.set_halign(gtk::Align::Start);
        let weak = Rc::downgrade(self);
        let message = message.clone();
        results.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.poll_voters(&message);
            }
        });
        body.append(&results);
        body
    }
    pub fn vote_poll(self: &Rc<Self>, id: &str, index: usize) {
        let Some(message) = self.state.borrow().message(id).cloned() else {
            return;
        };
        let Some(poll) = message.poll.clone() else {
            return;
        };
        if poll.closed || index >= poll.options.len() {
            return;
        }
        let mut emojis = message.my_reactions.clone();
        let key = &poll.options[index].emoji;
        let was_chosen = message
            .poll_tally
            .as_ref()
            .is_some_and(|t| t.chosen.contains(&index));
        if !poll.multiple {
            emojis.retain(|e| !poll.is_option(e));
        }
        if was_chosen {
            emojis.retain(|e| e.replace('\u{fe0f}', "") != key.replace('\u{fe0f}', ""));
        } else {
            emojis.insert(key.clone());
        }
        if emojis.len() > 32 {
            self.notice("Remove another reaction before adding this vote");
            self.render_messages(false);
            return;
        }
        if self.state.borrow().account.is_empty() {
            let tally = poll.tally(&[(true, emojis.clone())].into(), "", "");
            if let Some(m) = self
                .state
                .borrow_mut()
                .current_mut()
                .messages
                .iter_mut()
                .find(|m| m.id == id)
            {
                m.my_reactions = emojis;
                m.poll_tally = Some(tally);
            }
            self.render_messages(false);
            return;
        }
        let Some(target) = actions::reaction_reference(&message) else {
            return;
        };
        if !self.send_action(id, Action::React { target, emojis }, "") {
            self.render_messages(false);
        }
    }
    pub fn close_poll(self: &Rc<Self>, id: &str) {
        let Some(message) = self.state.borrow().message(id).cloned() else {
            return;
        };
        let Some(mut poll) = message.poll.clone() else {
            return;
        };
        if poll.closed || !message.own {
            return;
        }
        poll.closed = true;
        let legacy_body = if message.poll_source() == crate::polls::Source::Legacy {
            let Some(body) = crate::legacy_polls::body(&poll) else {
                self.notice("This historical poll exceeds the legacy closure size limit");
                return;
            };
            Some(body)
        } else {
            None
        };
        if self.state.borrow().account.is_empty() {
            if let Some(m) = self
                .state
                .borrow_mut()
                .current_mut()
                .messages
                .iter_mut()
                .find(|m| m.id == id)
            {
                m.poll = Some(poll);
                if let Some(body) = legacy_body {
                    m.text = body;
                }
            }
            self.render_messages(false);
            return;
        }
        let Some(target) = actions::original_wire(&message) else {
            return;
        };
        self.send_action_payload(id, Action::Correct { target }, &message.text, Some(poll));
    }
    pub fn poll_voters(self: &Rc<Self>, message: &Message) {
        let Some(poll) = &message.poll else { return };
        let tally = message.poll_tally.clone().unwrap_or_default();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Public votes")
            .description("Votes are visible to everyone in this conversation.")
            .build();
        for (actor, selected) in &tally.ballots {
            let state = self.state.borrow();
            let name = if actor == &state.account {
                "You"
            } else {
                &state.current().name
            };
            group.add(
                &adw::ActionRow::builder()
                    .title(name)
                    .subtitle(
                        selected
                            .iter()
                            .filter_map(|i| poll.options.get(*i))
                            .map(|o| o.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                    .use_markup(false)
                    .build(),
            );
        }
        page.add(&group);
        self.dialog("Poll Results", &page, 460, 360);
    }
    pub fn send_poll(self: &Rc<Self>, poll: Poll, fallback: bool) -> bool {
        self.send_poll_text(poll, fallback, "")
    }
    pub fn send_poll_text(
        self: &Rc<Self>,
        poll: Poll,
        fallback: bool,
        accompaniment: &str,
    ) -> bool {
        self.send_poll_text_attribution(poll, fallback, accompaniment, None)
    }
    pub fn send_poll_text_attribution(
        self: &Rc<Self>,
        mut poll: Poll,
        fallback: bool,
        accompaniment: &str,
        attribution: Option<crate::forwarding::Attribution>,
    ) -> bool {
        let Ok(canonical) = poll.canonicalized() else {
            self.notice(
                "Enter a valid question and 2–32 nonempty options within the poll size limits",
            );
            return false;
        };
        poll = canonical;
        let state = self.state.borrow();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        let sending = self.sending_mode(state.current());
        if owner.is_empty() {
            drop(state);
            let Some(id) = self.state.borrow_mut().send(&poll.question, "poll") else {
                return false;
            };
            if let Some(m) = self
                .state
                .borrow_mut()
                .current_mut()
                .messages
                .iter_mut()
                .find(|m| m.id == id)
            {
                m.text = accompaniment.into();
                m.poll = Some(poll);
                m.poll_fallback = fallback;
                m.forwarded = attribution;
            }
            self.render_messages(true);
            self.refresh_chats(None);
            return true;
        }
        if !self.online.get() {
            drop(state);
            self.notice("Connect before creating a live poll");
            return false;
        }
        let Some(mode) = sending else { return false };
        if mode == crate::encryption_policy::Mode::Legacy {
            drop(state);
            self.notice("Structured encrypted polls require OMEMO 2");
            return false;
        }
        let encrypted = mode.encrypted();
        if encrypted && !self.crypto.borrow().ready {
            drop(state);
            self.notice("Enable OMEMO 2 and review devices before sending this poll");
            return false;
        }
        drop(state);
        let wire = new_id();
        let message = Message {
            forwarded: attribution,
            id: wire.clone(),
            wire_id: wire.clone(),
            aliases: vec![format!("wire:{wire}"), format!("origin:{wire}")],
            sender: owner,
            author: "You".into(),
            own: true,
            kind: "poll".into(),
            text: accompaniment.into(),
            poll: Some(poll),
            poll_fallback: fallback,
            encrypted,
            fingerprint: if encrypted {
                self.crypto.borrow().fingerprint.clone()
            } else {
                String::new()
            },
            delivery: "sending".into(),
            ..Default::default()
        };
        self.state
            .borrow_mut()
            .current_mut()
            .messages
            .push(message.clone());
        if !self.persist() {
            self.state
                .borrow_mut()
                .current_mut()
                .messages
                .retain(|m| m.id != wire);
            return false;
        }
        if !self.command(crate::protocol::Command::Send {
            peer: peer.clone(),
            message,
            quote: String::new(),
        }) {
            self.delivery(&peer, &wire, "failed");
            return false;
        }
        self.render_messages(true);
        self.refresh_chats(None);
        true
    }
}
