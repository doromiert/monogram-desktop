//! Live message controls share the same durable projection for local sends and remote replay.
use crate::{
    message_actions::{self as actions, Action},
    model::*,
    protocol::Command,
    ui::Ui,
};
use std::rc::Rc;
impl Ui {
    pub fn sync_actions(&self, owner: &str, peer: &str) {
        let mut state = self.state.borrow_mut();
        let c = state.chat_for(peer);
        let attachments: Vec<_> = c
            .messages
            .iter()
            .filter_map(|m| m.attachment.clone().map(|file| (m.id.clone(), file)))
            .collect();
        let count = c.messages.len().max(200).min(u32::MAX as usize) as u32;
        c.messages = self.store.history(owner, peer, None, count);
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
        let snapshot = c.messages.clone();
        for m in &mut c.messages {
            if m.reply_id.is_empty() {
                continue;
            }
            let candidates: Vec<_> = snapshot
                .iter()
                .filter(|target| {
                    (m.reply_to.is_empty() || m.reply_to == if target.own { owner } else { peer })
                        && (target.wire_id == m.reply_id
                            || target
                                .aliases
                                .iter()
                                .any(|a| a == &format!("origin:{}", m.reply_id)))
                })
                .collect();
            if candidates.len() == 1 {
                let target = candidates[0];
                m.reply = format!(
                    "{} · {}",
                    target.author,
                    target.preview().chars().take(80).collect::<String>()
                );
            } else if let Some(id) = self.store.known(
                owner,
                peer,
                m.reply_to == owner,
                &[
                    format!("wire:{}", m.reply_id),
                    format!("origin:{}", m.reply_id),
                ],
            ) {
                if self
                    .store
                    .message(owner, peer, &id)
                    .is_none_or(|m| m.retracted)
                {
                    m.reply = "Original message was removed".into();
                }
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
        if current.mode() == crate::encryption_policy::Mode::Legacy
            || self.sending_mode(self.state.borrow().current())
                == Some(crate::encryption_policy::Mode::Legacy)
        {
            self.notice("Legacy OMEMO does not support protected message actions here");
            return false;
        }
        let encrypted = current.encrypted
            || self.state.borrow().current().encryption
            || kind == "react" && self.store.action_protected(&owner, &peer, id);
        let crypto = self.crypto.borrow();
        if encrypted && !crypto.ready {
            drop(crypto);
            self.notice("Enable OMEMO 2 and review devices before changing this message");
            return false;
        }
        if kind == "retract" && base.encrypted && base.fingerprint != crypto.fingerprint {
            drop(crypto);
            self.notice("An encrypted message can only be retracted by its original device");
            return false;
        }
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
            encrypted,
            fingerprint: if encrypted {
                crypto.fingerprint.clone()
            } else {
                String::new()
            },
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
        drop(crypto);
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
