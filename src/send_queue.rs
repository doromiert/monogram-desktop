//! An unsent row is durable before choosing encryption or submitting a packet.
use crate::{
    encryption_policy::{Mode, Preference, Selection},
    model::*,
    protocol::Command,
    ui::Ui,
};
use adw::prelude::*;
use gtk::glib;
use std::{
    collections::{BTreeMap, HashSet},
    rc::Rc,
};

#[derive(Default)]
pub struct Queue {
    loading: bool,
    peers: HashSet<(u64, String)>,
}

impl crate::storage::Store {
    pub fn queued_messages(&self, owner: &str) -> rusqlite::Result<Vec<(String, Message)>> {
        let mut query = self.db.prepare("SELECT peer,body FROM messages WHERE account=? AND json_extract(body,'$.outbox_pending')=1 AND json_extract(body,'$.delivery') IN('queued','preparing-encryption') ORDER BY timestamp,id LIMIT 64")?;
        query
            .query_map([owner], |row| {
                let body: String = row.get(1)?;
                let message = Message::decode(&body).map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok((row.get(0)?, message))
            })?
            .collect()
    }
}

impl Ui {
    pub(crate) fn peer_blocked(&self, peer: &str) -> bool {
        self.blocking_snapshot
            .borrow()
            .jids
            .as_ref()
            .is_some_and(|list| !crate::blocking::matching_rules(list, peer).is_empty())
    }
    fn retain_blocked_queue(self: &Rc<Self>, owner: &str, peer: &str, messages: Vec<Message>) {
        for mut message in messages {
            message.delivery = "failed".into();
            if let Some(draft) = message.upload.as_mut() {
                draft.error = Some("Unblock this address before retrying".into());
            }
            self.store_queued_row(owner, peer, message);
        }
    }
    pub fn send_library_file(self: &Rc<Self>, path: &std::path::Path, sticker: bool) -> bool {
        if !sticker {
            return self.stage_attachment_files(vec![gtk::gio::File::for_path(path)]);
        }
        self.queue_local_files(vec![gtk::gio::File::for_path(path)], "", sticker, false)
    }

    pub fn queue_local_files(
        self: &Rc<Self>,
        files: Vec<gtk::gio::File>,
        caption: &str,
        sticker: bool,
        clear_composer: bool,
    ) -> bool {
        if files.is_empty()
            || files.len() > 64
            || caption.len() > 64 * 1024
            || self.state.borrow().account.is_empty()
        {
            return false;
        }
        if self.peer_blocked(&self.state.borrow().current().jid) {
            self.notice("Unblock this address before sending. Your files and caption are kept.");
            return false;
        }
        let caption_markup=if self.composer_text()==caption{self.composer_formatting().trim().styled()}else{crate::formatting::Markup::plain(caption.trim())};
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.active_message(id));
        let mut state = self.state.borrow_mut();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        let group = state.current().group;
        let mut ids = Vec::new();
        for (index, file) in files.into_iter().enumerate() {
            let id = new_id();
            let mut message = Message {
                id: id.clone(),
                wire_id: id.clone(),
                aliases: vec![format!("wire:{id}"), format!("origin:{id}")],
                sender: owner.clone(),
                author: "You".into(),
                own: true,
                kind: "file".into(),
                text: if index == 0 {caption_markup.body.clone()} else {String::new()},
                formatting: Some(if index==0 {caption_markup.clone()}else{crate::formatting::Markup{spoiler:caption_markup.spoiler,..Default::default()}}),
                preview_spoilers:Some(if index==0{caption_markup.spoiler_context()}else{crate::formatting::Markup{spoiler:caption_markup.spoiler,..Default::default()}.spoiler_context()}),
                outbox_pending: true,
                delivery: "queued".into(),
                unsupported_attachment: true,
                upload: Some(UploadDraft {
                    uri: file.uri().into(),
                    name: file
                        .basename()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "attachment".into()),
                    error: None,
                    sticker,
                }),
                ..Default::default()
            };
            if let Some(reply) = &reply {
                message.reply = format!("{} · {}", reply.author, reply.reply_preview());
                message.reply_id = reply.wire_id.clone();
                message.reply_to = if group && !reply.sender.is_empty() {
                    reply.sender.clone()
                } else if reply.own {
                    owner.clone()
                } else {
                    peer.clone()
                };
            }
            state.current_mut().messages.push(message);
            ids.push(id);
        }
        drop(state);
        if !self.persist() {
            self.state
                .borrow_mut()
                .current_mut()
                .messages
                .retain(|message| !ids.contains(&message.id));
            return false;
        }
        if clear_composer {
            self.loading.set(true);
            self.composer.buffer().set_text("");
            self.loading.set(false);
            self.attachments.borrow_mut().clear();
            crate::widgets::clear(&self.chips);
            self.chips.set_visible(false);
            self.update_send_button();
        }
        self.cancel_reply();
        self.save_draft();
        self.history_window.content_changed();
        self.render_messages(true);
        self.refresh_chats(None);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.flush_send_queue();
            }
        });
        true
    }
    pub fn queue_text(self:&Rc<Self>,value:&str){let _=self.queue_formatted_text(value,None);}
    pub fn queue_formatted_text(self: &Rc<Self>, value: &str, formatting:Option<crate::formatting::Markup>) -> Option<String> {
        if self.peer_blocked(&self.state.borrow().current().jid) {
            self.notice("Unblock this address before sending. Your draft is kept.");
            return None;
        }
        if value.len() > 64 * 1024 {
            self.notice("Message is too long");
            return None;
        }
        let from_composer=self.composer_text()==value;
        let authored=formatting.unwrap_or_else(||if from_composer{self.composer_formatting()}else{crate::formatting::Markup::plain(value)}).trim().styled();
        if authored.body.len()>64*1024 || authored.markdown && (authored.body.len()>32768 || authored.body.encode_utf16().count()>32768){self.notice("Formatted message is too long");return None;}
        let inline = self
            .composer_inline_emoji()
            .filter(|emoji| emoji.body == value)
            .and_then(|emoji| {
                let start = value[..value.len() - value.trim_start().len()]
                    .chars()
                    .count();
                emoji
                    .slice(start, start + value.trim().chars().count())
                    .ok()
            });
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.active_message(id));
        let mut state = self.state.borrow_mut();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        let group = state.current().group;
        let Some(id) = state.send(&authored.body, "text") else {
            return None;
        };
        let message = state
            .current_mut()
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .unwrap();
        message.outbox_pending = true;
        message.inline_emoji = inline.and_then(|emoji|crate::formatting::remap_inline(&authored,&emoji));
        message.preview_spoilers=Some(authored.spoiler_context());
        message.formatting=Some(authored);
        message.delivery = "queued".into();
        // Encryption is unknown until the sending policy has actually completed.
        message.encrypted = false;
        message.encryption_mode = None;
        if let Some(reply) = reply {
            message.reply = format!("{} · {}", reply.author, reply.reply_preview());
            message.reply_id = reply.wire_id.clone();
            message.reply_to = if group && !reply.sender.is_empty() {
                reply.sender
            } else if reply.own {
                owner
            } else {
                peer.clone()
            };
        }
        drop(state);
        if !self.persist() {
            self.state
                .borrow_mut()
                .current_mut()
                .messages
                .retain(|message| message.id != id);
            return None;
        }
        if from_composer {
            self.loading.set(true);self.composer.buffer().set_text("");self.loading.set(false);
            self.composer_spoiler.set_state(&false.to_variant());
            self.clear_composer_inline_emoji();self.cancel_reply();self.save_draft();
        }
        self.history_window.content_changed();
        self.render_messages(true);
        self.refresh_chats(None);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.flush_send_queue();
            }
        });
        Some(id)
    }

    pub fn flush_send_queue(self: &Rc<Self>) {
        self.flush_local_retractions();
        if !self.online.get() || self.send_queue.borrow().loading {
            return;
        }
        self.send_queue.borrow_mut().loading = true;
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let account = owner.clone();
            let rows = ui
                .store
                .read_snapshot(move |store| store.queued_messages(&account))
                .await;
            ui.send_queue.borrow_mut().loading = false;
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            let Ok(rows) = rows else {
                ui.notice("Queued messages could not be loaded");
                return;
            };
            let mut peers: BTreeMap<String, Vec<Message>> = BTreeMap::new();
            for (peer, message) in rows {
                peers.entry(peer).or_default().push(message);
            }
            for (peer, messages) in peers {
                ui.prepare_queued_peer(peer, messages);
            }
        });
    }

    fn prepare_queued_peer(self: &Rc<Self>, peer: String, messages: Vec<Message>) {
        if self.peer_blocked(&peer) {
            let owner = self.state.borrow().account.clone();
            self.retain_blocked_queue(&owner, &peer, messages);
            return;
        }
        let generation = self.generation.get();
        let revision = self.encryption_revision.get();
        let state = self.state.borrow();
        let Some(chat) = state.chats.iter().find(|chat| chat.jid == peer) else {
            return;
        };
        if chat.group && !chat.room_can_send {
            return;
        }
        let selected = self.sending_mode(chat);
        if let Some(mode) = selected {
            if self.fingerprint_for_send(mode).is_none() {
                return;
            }
        } else if !self.crypto.borrow().ready && !self.legacy_crypto.borrow().ready {
            return;
        }
        let owner = state.account.clone();
        drop(state);
        let key = (generation, peer.clone());
        if !self.send_queue.borrow_mut().peers.insert(key.clone()) {
            return;
        }
        let result: Option<Result<Mode, String>> = selected.map(Ok);
        let receive = if result.is_none() {
            let (reply, receive) = tokio::sync::oneshot::channel();
            if self.command(Command::ChooseEncryption {
                peer: peer.clone(),
                reply,
            }) {
                Some(receive)
            } else {
                None
            }
        } else {
            None
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = match result {
                Some(result) => result,
                None => match receive {
                    Some(receive) => receive
                        .await
                        .map_err(|_| "Encryption preparation was interrupted".to_owned())
                        .and_then(|result| result.map_err(|error| error.to_string())),
                    None => Err("The connection is unavailable".into()),
                },
            };
            let Some(ui) = weak.upgrade() else { return };
            ui.send_queue.borrow_mut().peers.remove(&key);
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            if !ui.online.get() {
                return;
            }
            let result = if ui.encryption_revision.get() != revision {
                Err("Encryption settings changed. Retry this message.".into())
            } else {
                result
            };
            if ui.peer_blocked(&peer) {
                ui.retain_blocked_queue(&owner, &peer, messages);
                return;
            }
            let mode = match result {
                Ok(mode) => mode,
                Err(error) => {
                    for mut message in messages {
                        message.delivery = "failed".into();
                        ui.store_queued_row(&owner, &peer, message);
                    }
                    ui.notice(&error);
                    return;
                }
            };
            let Some(fingerprint) = ui.fingerprint_for_send(mode) else {
                return;
            };
            if selected.is_none() {
                if let Some(chat) = ui
                    .state
                    .borrow_mut()
                    .chats
                    .iter_mut()
                    .find(|chat| chat.jid == peer)
                {
                    chat.sending_preference = Some(Preference::Automatic);
                    chat.automatic_encryption = Some(Selection {
                        mode,
                        time_ms: chrono::Utc::now().timestamp_millis(),
                    });
                    chat.encryption = mode.encrypted();
                }
                if !ui.persist() {
                    return;
                }
                if mode == Mode::Plain {
                    ui.notice("Sending without end-to-end encryption in this conversation");
                }
            }
            for message in messages {
                // A user may remove a queued message while the key check is running.
                let Some(mut current) =
                    ui.store
                        .message(&owner, &peer, &message.id)
                        .filter(|message| {
                            message.outbox_pending
                                && !message.retracted
                                && matches!(
                                    message.delivery.as_str(),
                                    "queued" | "preparing-encryption"
                                )
                        })
                else {
                    continue;
                };
                current.set_encryption(mode, fingerprint.clone());
                current.outbox_pending = false;
                current.delivery = "sending".into();
                if current.upload.is_some() {
                    if mode == Mode::Legacy && !current.text.trim().is_empty() {
                        let caption_id = new_id();
                        let mut caption = Message {
                            id: caption_id.clone(),
                            wire_id: caption_id.clone(),
                            aliases: vec![
                                format!("wire:{caption_id}"),
                                format!("origin:{caption_id}"),
                            ],
                            sender: owner.clone(),
                            author: "You".into(),
                            own: true,
                            text: std::mem::take(&mut current.text),
                            formatting:current.formatting.take(),
                            preview_spoilers:current.preview_spoilers.take(),
                            upload_caption_for: current.id.clone(),
                            delivery: "waiting-upload".into(),
                            ..Default::default()
                        };
                        caption.set_encryption(mode, fingerprint.clone());
                        if ui
                            .store
                            .save_outbox_pair(&owner, &peer, &current, &caption)
                            .is_err()
                        {
                            ui.notice("Media caption could not be saved");
                            continue;
                        }
                        if let Some(chat) = ui
                            .state
                            .borrow_mut()
                            .chats
                            .iter_mut()
                            .find(|chat| chat.jid == peer)
                        {
                            if let Some(row) =
                                chat.messages.iter_mut().find(|row| row.id == current.id)
                            {
                                *row = current.clone();
                            }
                            chat.messages.push(caption);
                        }
                    } else if !ui.store_queued_row(&owner, &peer, current.clone()) {
                        continue;
                    }
                    ui.start_upload(&peer, &current.id);
                    continue;
                }
                if !ui.store_queued_row(&owner, &peer, current.clone()) {
                    continue;
                }
                let wire_id = current.wire_id.clone();
                let quote = current
                    .reply
                    .split_once(" · ")
                    .map_or(String::new(), |(_, body)| body.into());
                if !ui.command(Command::Send {
                    peer: peer.clone(),
                    message: current,
                    quote,
                }) {
                    ui.delivery(&peer, &wire_id, "failed");
                }
            }
            ui.flush_send_queue();
        });
    }

    pub fn store_queued_row(self: &Rc<Self>, owner: &str, peer: &str, message: Message) -> bool {
        if self.store.save_outbox_row(owner, peer, &message).is_err() {
            self.notice("Queued message could not be saved");
            return false;
        }
        let mut state = self.state.borrow_mut();
        if let Some(chat) = state.chats.iter_mut().find(|chat| chat.jid == peer) {
            if let Some(row) = chat.messages.iter_mut().find(|row| row.id == message.id) {
                *row = message;
            }
            chat.remember_latest();
        }
        let active = state.current().jid == peer;
        drop(state);
        if active {
            self.render_messages(false);
        }
        self.refresh_chats(None);
        true
    }

    pub fn retry_queued_message(self: &Rc<Self>, id: &str) -> bool {
        let state = self.state.borrow();
        let Some(mut message) = state
            .message(id)
            .filter(|message| message.outbox_pending)
            .cloned()
        else {
            return false;
        };
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        drop(state);
        if self.peer_blocked(&peer) {
            self.notice("Unblock this address before retrying");
            return true;
        }
        message.delivery = "queued".into();
        if self.store_queued_row(&owner, &peer, message) {
            self.flush_send_queue();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn blocked_sends_keep_drafts_and_require_explicit_retry() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.BlockingSendFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let owner = "owner@fixture.invalid";
        let peer = "peer@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = State::live(owner, vec![]);
        state.active = state.chat_for(peer).id.clone();
        ui.activate_state(state);
        ui.blocking_snapshot.replace(crate::blocking::Snapshot {
            jids: Some(vec![peer.into()]),
            fresh: true,
            supported: Some(true),
        });
        ui.composer.buffer().set_text("Keep the blocked draft");
        let file = gtk::gio::File::for_path("/nonexistent/blocked-fixture.png");
        ui.attachments.borrow_mut().push(file.clone());
        let started = std::time::Instant::now();
        ui.send();
        assert!(started.elapsed() < std::time::Duration::from_millis(400));
        ui.queue_text("Also blocked");
        assert!(!ui.queue_local_files(vec![file], "caption", false, true));
        assert_eq!(ui.composer_text(), "Keep the blocked draft");
        assert_eq!(ui.attachments.borrow().len(), 1);
        assert!(ui.state.borrow().current().messages.is_empty());
        assert!(ui.store.queued_messages(owner).unwrap().is_empty());
        assert!(ui.session.borrow().is_none());
        let queued = Message {
            id: "waiting".into(),
            wire_id: "waiting".into(),
            text: "Previously queued".into(),
            own: true,
            delivery: "queued".into(),
            outbox_pending: true,
            ..Default::default()
        };
        ui.state
            .borrow_mut()
            .current_mut()
            .messages
            .push(queued.clone());
        assert!(ui.persist());
        ui.prepare_queued_peer(peer.into(), vec![queued]);
        let failed = ui.store.message(owner, peer, "waiting").unwrap();
        assert_eq!(failed.delivery, "failed");
        assert!(failed.outbox_pending);
        assert!(ui.store.queued_messages(owner).unwrap().is_empty());
        assert!(ui.retry_queued_message("waiting"));
        assert!(ui.store.queued_messages(owner).unwrap().is_empty());
        ui.blocking_snapshot.borrow_mut().jids = Some(vec![]);
        assert!(ui.store.queued_messages(owner).unwrap().is_empty());
        assert!(ui.retry_queued_message("waiting"));
        assert_eq!(ui.store.queued_messages(owner).unwrap().len(), 1);
        ui.window.close();
    }
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn optimistic_text_is_durable_before_any_connection() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.OutboxFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let owner = "owner@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = State::live(owner, vec![]);
        state.active = state.chat_for("peer@fixture.invalid").id.clone();
        ui.activate_state(state);
        ui.composer
            .buffer()
            .set_text("Keep this while disconnected");
        let start = std::time::Instant::now();
        ui.send();
        assert!(start.elapsed() < std::time::Duration::from_millis(400));
        assert!(ui.composer_text().is_empty());
        let queued = ui.store.queued_messages(owner).unwrap();
        assert_eq!(queued.len(), 1);
        let (peer, message) = &queued[0];
        assert_eq!(message.text, "Keep this while disconnected");
        assert!(message.outbox_pending);
        assert_eq!(message.delivery, "queued");
        assert!(message.encryption_mode.is_none());
        assert!(
            ui.session.borrow().is_none(),
            "the check must never connect or submit a packet"
        );
        let mut failed = message.clone();
        failed.delivery = "failed".into();
        assert!(ui.store_queued_row(owner, peer, failed));
        assert!(ui.retry_queued_message(&message.id));
        assert_eq!(ui.store.queued_messages(owner).unwrap().len(), 1);
        assert_eq!(
            ui.state.borrow().current().messages.len(),
            1,
            "retry must retain one ID"
        );
        ui.composer.buffer().set_text("Preserve this draft");
        assert!(ui.send_library_file(std::path::Path::new("/fictional/sticker.webp"), true));
        assert_eq!(ui.composer_text(), "Preserve this draft");
        let queued = ui.store.queued_messages(owner).unwrap();
        assert_eq!(queued.len(), 2);
        assert!(
            queued
                .iter()
                .any(|(_, message)| message.upload.as_ref().is_some_and(|draft| draft.sticker))
        );
        ui.window.destroy();
    }
}
