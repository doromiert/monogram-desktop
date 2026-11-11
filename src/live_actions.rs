//! Live message controls share the same durable projection for local sends and remote replay.
use crate::{
    message_actions::{self as actions, Action},
    model::*,
    protocol::Command,
    ui::Ui,
};
use std::rc::Rc;
impl Ui {
    fn public_legacy_reaction(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        base: &Message,
        current: &Message,
    ) -> bool {
        let state = self.state.borrow();
        let selected = self.sending_mode(state.current());
        !state.current().group
            && (current.mode() == crate::encryption_policy::Mode::Legacy
                || selected == Some(crate::encryption_policy::Mode::Legacy))
            && base.mode() != crate::encryption_policy::Mode::Omemo2
            && selected != Some(crate::encryption_policy::Mode::Omemo2)
            && !self.store.action_protected(owner, peer, id)
    }
    pub(crate) fn public_reaction_consent_required(&self, id: &str) -> bool {
        let state = self.state.borrow();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        drop(state);
        let (Ok(base), Ok(current)) = (
            self.store.action_base(&owner, &peer, id),
            self.store.action_target(&owner, &peer, id, "react"),
        ) else {
            return false;
        };
        self.public_legacy_reaction(&owner, &peer, id, &base, &current)
            && self
                .store
                .pref(&format!("plain-reactions:{owner}\n{peer}"), "false")
                != "true"
    }
    pub fn sync_actions(&self, owner: &str, peer: &str) {
        let mut state = self.state.borrow_mut();
        if state.account == owner && state.current().jid == peer {
            self.history_window.content_changed();
        }
        let c = state.chat_for(peer);
        let attachments: Vec<_> = c
            .messages
            .iter()
            .filter_map(|m| m.attachment.clone().map(|file| (m.id.clone(), file)))
            .collect();
        c.messages = c
            .messages
            .iter()
            .filter_map(|m| self.store.message(owner, peer, &m.id))
            .filter(|m| !m.retracted)
            .collect();
        c.latest = self
            .store
            .history(owner, peer, None, 1)
            .first()
            .map(MessagePreview::from);
        for (id, file) in attachments {
            if self
                .store
                .message(owner, peer, &id)
                .is_none_or(|m| m.retracted)
            {
                self.downloads.borrow_mut().cancel(owner, &file);
                self.images.borrow_mut().invalidate(&file);
            }
        }
        c.unread = self.store.unread_count(owner, peer);
        for m in &mut c.messages {
            if let Some(target) = self
                .store
                .resolve_reply(owner, peer, &m.reply_to, &m.reply_id)
            {
                m.reply = if target.retracted {
                    "Original message was removed".into()
                } else {
                    format!(
                        "{} · {}",
                        target.author,
                        target.preview().chars().take(80).collect::<String>()
                    )
                };
            }
        }
    }
    pub fn send_action(self: &Rc<Self>, id: &str, action: Action, text: &str) -> bool {
        self.send_action_payload(id, action, text, None)
    }
    pub fn send_action_payload(
        self: &Rc<Self>,
        id: &str,
        action: Action,
        text: &str,
        poll: Option<crate::polls::Poll>,
    ) -> bool {
        let state = self.state.borrow();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        if owner.is_empty() || !self.online.get() {
            drop(state);
            self.notice("Reconnect before changing a message");
            return false;
        }
        drop(state);
        let kind = match action {
            Action::Correct { .. } => "correct",
            Action::React { .. } => "react",
            Action::Retract { .. } => "retract",
        };
        let Ok(current) = self.store.action_target(&owner, &peer, id, kind) else {
            self.notice("This message has no unique, usable reference");
            return false;
        };
        let Ok(base) = self.store.action_base(&owner, &peer, id) else {
            return false;
        };
        if kind == "correct"
            && !match (&base.poll, &poll) {
                (None, None) => base.poll_source() != crate::polls::Source::Structured,
                (Some(original), Some(edit)) => edit.closed && original.same_definition(edit),
                _ => false,
            }
        {
            self.notice("A poll's question and choices cannot be changed");
            return false;
        }
        let legacy_body = if kind == "correct" && base.poll_source() == crate::polls::Source::Legacy
        {
            let Some(body) = poll.as_ref().and_then(crate::legacy_polls::body) else {
                self.notice("This historical poll exceeds the legacy closure size limit");
                return false;
            };
            Some(body)
        } else {
            None
        };
        let expected = if kind == "react" {
            actions::reaction_reference(&base)
        } else {
            actions::original_wire(&base)
        };
        if expected.as_deref() != Some(action.target()) {
            self.notice("Message changed; reopen its menu");
            return false;
        }
        let legacy = current.mode() == crate::encryption_policy::Mode::Legacy
            || self.sending_mode(self.state.borrow().current())
                == Some(crate::encryption_policy::Mode::Legacy);
        if legacy && (kind == "correct" || self.state.borrow().current().group) {
            self.notice("Legacy OMEMO does not support protected message actions here");
            return false;
        }
        let protected_reaction = kind == "react" && self.store.action_protected(&owner, &peer, id);
        let encrypted =
            current.encrypted || self.state.borrow().current().encryption || protected_reaction;
        // Legacy reactions carry public XEP-0444 metadata, as on mobile.
        let public_reaction =
            kind == "react" && self.public_legacy_reaction(&owner, &peer, id, &base, &current);
        if public_reaction
            && self
                .store
                .pref(&format!("plain-reactions:{owner}\n{peer}"), "false")
                != "true"
        {
            self.notice("Allow public reactions in this conversation before reacting");
            return false;
        }
        let mode = if kind == "retract" {
            base.mode()
        } else if public_reaction {
            crate::encryption_policy::Mode::Plain
        } else if encrypted {
            crate::encryption_policy::Mode::Omemo2
        } else {
            crate::encryption_policy::Mode::Plain
        };
        let Some(fingerprint) = self.fingerprint_for_send(mode) else {
            self.notice("The original message's OMEMO version is not ready");
            return false;
        };
        let wire = new_id();
        let message = Message {
            forwarded: if kind == "correct" {
                current.forwarded.clone()
            } else {
                None
            },
            poll_source: if legacy_body.is_some() {
                crate::polls::Source::Legacy
            } else {
                crate::polls::Source::None
            },
            poll_fallback: poll.is_some() && current.poll_fallback,
            poll,
            action: Some(action),
            sender: owner.clone(),
            author: "You".into(),
            id: wire.clone(),
            wire_id: wire.clone(),
            aliases: vec![format!("wire:{wire}"), format!("origin:{wire}")],
            own: true,
            encrypted: mode.encrypted(),
            encryption_mode: Some(mode),
            fingerprint,
            text: legacy_body.unwrap_or_else(|| text.into()),
            delivery: "sending".into(),
            reply_id: if kind == "correct" {
                current.reply_id
            } else {
                String::new()
            },
            reply_to: if kind == "correct" {
                current.reply_to
            } else {
                String::new()
            },
            reply: if kind == "correct" {
                current.reply
            } else {
                String::new()
            },
            ..Default::default()
        };
        // Pending retractions only hide content after the transport confirms the send.
        if self
            .store
            .record_action(&owner, &peer, &message, "sending")
            .and_then(|_| self.store.reconcile_actions(&owner, &peer).map(|_| ()))
            .is_err()
        {
            self.notice("Message change could not be saved");
            return false;
        }
        self.sync_actions(&owner, &peer);
        let quote = message.reply.clone();
        let sent = self.command(Command::Send {
            peer: peer.clone(),
            message: message.clone(),
            quote,
        });
        if !sent {
            let _ = self.store.action_delivery(&owner, &peer, &wire, "failed");
            self.sync_actions(&owner, &peer);
            self.notice("Message change could not be queued");
        }
        self.render_messages(false);
        self.refresh_chats(None);
        sent
    }
}
