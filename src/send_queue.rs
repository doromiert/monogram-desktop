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
        self.queue_local_files_with_preview(files,caption,sticker,clear_composer,None)
    }
    pub(crate) fn send_pack_sticker(self:&Rc<Self>,asset:&crate::packs::Asset)->bool {
        self.queue_local_files_with_preview(vec![gtk::gio::File::for_path(&asset.path)],"",true,false,Some(asset.item.id))
    }
    fn queue_local_files_with_preview(
        self: &Rc<Self>,
        files: Vec<gtk::gio::File>,
        caption: &str,
        sticker: bool,
        clear_composer: bool,
        local_sticker: Option<i64>,
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
                    local_sticker,
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
        self.notify_typing(false);
        self.mark_current_chat_read();
        self.history_window.content_changed();
        if sticker { if let Some(id)=ids.first(){self.animate_sent_sticker(id);} }
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
            self.animate_sent_text(&id);
            self.loading.set(true);self.composer.buffer().set_text("");self.loading.set(false);
            self.composer_spoiler.set_state(&false.to_variant());
            self.clear_composer_inline_emoji();self.cancel_reply();self.save_draft();
        }
        self.notify_typing(false);
        self.mark_current_chat_read();
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
            let skip_unreachable = self.offers_send_anyway()
                && (messages.iter().any(|message| message.skip_unreachable)
                    || self.send_anyway_active(&peer));
            if self.command(Command::ChooseEncryption {
                peer: peer.clone(),
                reply,
                skip_unreachable,
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
            if ui.encryption_revision.get() != revision {
                // The settings changed while the check ran, so its answer may be stale. Check
                // again under the current settings instead of failing the messages.
                ui.prepare_queued_peer(peer, messages);
                return;
            }
            if ui.peer_blocked(&peer) {
                ui.retain_blocked_queue(&owner, &peer, messages);
                return;
            }
            let mode = match result {
                Ok(mode) => mode,
                Err(error) => {
                    ui.fail_queued_selection(&owner, &peer, messages, &error);
                    return;
                }
            };
            // The server must not be able to talk an encrypted room into plaintext.
            if selected.is_none() && ui.encryption_downgrade_blocked(&peer, mode) {
                ui.fail_queued_downgrade(&owner, &peer, messages);
                return;
            }
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
                    chat.remember_selection(mode);
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

    /// Automatic selection could only offer plaintext for a room that has used encryption: mark the
    /// messages failed and, in a group, offer Send Anyway (still encrypted, without whoever has no
    /// usable keys). Plaintext is never chosen for the user.
    pub(crate) fn fail_queued_downgrade(
        self: &Rc<Self>,
        owner: &str,
        peer: &str,
        messages: Vec<Message>,
    ) {
        let ids: Vec<String> = messages.iter().map(|message| message.id.clone()).collect();
        for mut message in messages {
            message.delivery = "failed".into();
            self.store_queued_row(owner, peer, message);
        }
        self.notice_downgrade_blocked(peer, ids);
    }

    /// The encryption check for queued messages failed: mark them failed and say why. A Legacy
    /// OMEMO contact-authorization denial offers Resolve.
    pub(crate) fn fail_queued_selection(
        self: &Rc<Self>,
        owner: &str,
        peer: &str,
        messages: Vec<Message>,
        error: &str,
    ) {
        let ids: Vec<String> = messages.iter().map(|message| message.id.clone()).collect();
        for mut message in messages {
            message.delivery = "failed".into();
            self.store_queued_row(owner, peer, message);
        }
        self.notice_send_failure(peer, ids, error, error.to_owned());
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

    /// Send the messages `ids` of the room `peer` again, encrypted for every member whose keys can
    /// be used and without the ones that cannot. The choice is stored on each message, so it
    /// survives a restart, and it applies to these messages only.
    pub(crate) fn send_anyway(self: &Rc<Self>, peer: &str, ids: &[String]) {
        let (owner, room_id) = {
            let state = self.state.borrow();
            let Some(room) = state.chats.iter().find(|chat| chat.jid == peer && chat.group) else {
                return;
            };
            (state.account.clone(), room.id.clone())
        };
        if self.state.borrow().current().jid != peer {
            self.select_chat(&room_id);
        }
        // Later messages to this room skip the same members for a while, so the user is not asked
        // again for every message.
        self.send_anyway_until
            .borrow_mut()
            .insert(peer.to_owned(), std::time::Instant::now() + Self::SEND_ANYWAY_COOLDOWN);
        for id in ids {
            let message = {
                let mut state = self.state.borrow_mut();
                state.current_mut().messages.iter_mut().find(|message| message.id == *id).map(|message| {
                    message.skip_unreachable = true;
                    message.clone()
                })
            };
            let Some(message) = message else { continue };
            if message.outbox_pending {
                self.store_queued_row(&owner, peer, message);
                self.retry_queued_message(id);
                continue;
            }
            // Already sent once and failed: send it again without asking, the user just chose to.
            if !self.online.get() {
                self.notice("Reconnect before sending");
                return;
            }
            let mut message = message;
            message.delivery = "sending".into();
            if let Some(row) = self.state.borrow_mut().current_mut().messages.iter_mut().find(|row| row.id == *id) {
                *row = message.clone();
            }
            if self.persist() {
                self.command(Command::Send { peer: peer.into(), message, quote: String::new() });
            }
        }
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

    fn settle(milliseconds: u64) {
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
        while std::time::Instant::now() < until {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    fn shows_text(widget: &gtk::Widget, needle: &str) -> bool {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().contains(needle))
            || {
                let mut child = widget.first_child();
                let mut found = false;
                while let Some(current) = child {
                    found |= shows_text(&current, needle);
                    child = current.next_sibling();
                }
                found
            }
    }

    /// Encryption settings that change while a queued message's key check is running no longer fail
    /// the message with a toast: the check runs again under the new settings and the message goes on.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_queued_message_is_rechecked_automatically_when_settings_change_mid_check() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.negzero.StaleCheckFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        let (owner, peer) = ("owner@fixture.invalid", "peer@fixture.invalid");
        ui.store
            .save_account(&Account { jid: owner.into(), host: String::new(), port: 5222, direct_tls: false })
            .unwrap();
        let mut state = State::live(owner, vec![]);
        state.active = state.chat_for(peer).id.clone();
        ui.activate_state(state);
        ui.online.set(true);
        let queued = Message {
            id: "waiting".into(),
            wire_id: "waiting".into(),
            text: "Queued while settings change".into(),
            own: true,
            delivery: "queued".into(),
            outbox_pending: true,
            ..Default::default()
        };
        ui.state.borrow_mut().current_mut().messages.push(queued.clone());
        assert!(ui.persist());

        // Start the check, then change the encryption settings before its answer is handled.
        ui.prepare_queued_peer(peer.into(), vec![queued]);
        ui.encryption_revision.set(ui.encryption_revision.get().wrapping_add(1));
        settle(500);

        assert!(
            !shows_text(ui.toast.upcast_ref(), "Encryption settings changed"),
            "no stale-settings toast"
        );
        let message = ui.store.message(owner, peer, "waiting").unwrap();
        assert!(
            message.encryption_mode.is_some(),
            "the check ran again and the message was prepared under the current settings"
        );
        assert!(!message.outbox_pending, "the message left the queue instead of failing in it");
        ui.window.close();
    }

    fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_button(&current, label) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    const ROOM: &str = "team@conference.example.org";
    const OWNER: &str = "owner@fixture.invalid";
    type Commands = tokio::sync::mpsc::UnboundedReceiver<Command>;
    /// Route what the app sends to the server into a receiver the test can read.
    fn capture(ui: &Ui) -> Commands {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        ui.session.replace(Some(crate::protocol::Session::fixture_channel(tx)));
        rx
    }
    fn drain(rx: &mut Commands) -> Vec<Command> {
        let mut out = vec![];
        while let Ok(command) = rx.try_recv() {
            out.push(command);
        }
        out
    }
    /// The `skip_unreachable` of the encryption check asked for `peer`, if one was asked.
    fn probe_skip(commands: &[Command], peer: &str) -> Option<bool> {
        commands.iter().find_map(|c| match c {
            Command::ChooseEncryption { peer: p, skip_unreachable, .. } if p == peer => Some(*skip_unreachable),
            _ => None,
        })
    }
    /// The `skip_unreachable` of the message sent to `peer`, if one was sent.
    fn sent_skip(commands: &[Command], peer: &str) -> Option<(bool, bool)> {
        commands.iter().find_map(|c| match c {
            Command::Send { peer: p, message, .. } if p == peer => Some((message.skip_unreachable, message.action.is_some())),
            _ => None,
        })
    }
    fn refusal() -> String {
        crate::crypto::Error::ContactAuthorization("carol@example.org".into()).to_string()
    }

    /// A signed-in window in the group chat `ROOM`, holding one own message `id` that failed.
    fn room_with_failed_message(app_id: &str, id: &str, outbox_pending: bool) -> (Rc<Ui>, Message) {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id(app_id)
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        ui.window.present();
        ui.store
            .save_account(&Account { jid: OWNER.into(), host: String::new(), port: 5222, direct_tls: false })
            .unwrap();
        let mut state = State::live(OWNER, vec![]);
        let room = state.chat_for(ROOM);
        room.group = true;
        room.room_can_send = true;
        // Automatic selection, as in a real chat: without a preference the smoke fixture would
        // skip the encryption check these tests look at.
        room.sending_preference = Some(crate::encryption_policy::Preference::Automatic);
        let room_id = room.id.clone();
        state.active = room_id;
        ui.activate_state(state);
        ui.online.set(true);
        ui.legacy_crypto.borrow_mut().ready = true;
        let message = Message {
            id: id.into(),
            wire_id: format!("wire-{id}"),
            markable_id: format!("wire-{id}"),
            sender: OWNER.into(),
            text: "hello room".into(),
            own: true,
            delivery: "failed".into(),
            outbox_pending,
            timestamp: "2026-11-17T10:00:00Z".into(),
            ..Default::default()
        };
        ui.state.borrow_mut().current_mut().messages.push(message.clone());
        assert!(ui.persist());
        let chat = ui.state.borrow().current().clone();
        ui.store.save_chat_message(OWNER, &chat, Some(&message)).unwrap();
        (ui, message)
    }

    /// The encryption check for a queued room message failed on one member's keys: the toast offers
    /// Send Anyway (not Resolve), and choosing it retries the message without that member.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_queued_room_message_blocked_by_one_member_can_be_sent_anyway() {
        let (ui, message) = room_with_failed_message("com.negzero.SendAnywayQueuedFixture", "waiting", true);
        ui.fail_queued_selection(OWNER, ROOM, vec![message], &refusal());
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "Legacy OMEMO keys require contact authorization from carol@example.org"));
        assert!(find_button(ui.toast.upcast_ref(), "Resolve").is_none(), "a room offers Send Anyway instead");
        let button = find_button(ui.toast.upcast_ref(), "Send Anyway").expect("Send Anyway button");
        assert!(!ui.store.message(OWNER, ROOM, "waiting").unwrap().skip_unreachable, "not chosen yet");
        let mut commands = capture(&ui);

        button.emit_clicked();
        settle(300);
        let stored = ui.store.message(OWNER, ROOM, "waiting").unwrap();
        assert!(stored.skip_unreachable, "the choice is stored on the message");
        assert_ne!(stored.delivery, "failed", "the message was retried");
        let sent = drain(&mut commands);
        assert_eq!(probe_skip(&sent, ROOM), Some(true), "the encryption check is asked to leave members out");
        assert_eq!(
            ui.state.borrow().chats.iter().find(|c| c.jid == ROOM).unwrap().selected_encryption(),
            None,
            "the room is not locked to a protocol: automatic selection still chooses"
        );
        ui.window.close();
    }

    /// A room message that was sent and failed (not queued): the same offer, resent directly.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_failed_room_send_can_be_sent_anyway() {
        use crate::protocol::BackendEvent;
        let (ui, _) = room_with_failed_message("com.negzero.SendAnywayFailedFixture", "sent", false);
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        ui.backend_event(generation, &owner, BackendEvent::Failed { peer: ROOM.into(), id: "wire-sent".into(), reason: refusal() });
        settle(300);
        let button = find_button(ui.toast.upcast_ref(), "Send Anyway").expect("Send Anyway button");
        let mut commands = capture(&ui);
        button.emit_clicked();
        settle(300);
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((true, false)), "resent with the choice");
        let row = ui.state.borrow().current().messages.iter().find(|m| m.id == "sent").cloned().unwrap();
        assert!(row.skip_unreachable, "the choice is on the message that is resent");
        assert_eq!(row.delivery, "sending", "resent without a confirmation dialog");
        ui.window.close();
    }

    /// Nothing about unreviewed keys, a lost connection, or a one-to-one chat is skippable.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn send_anyway_is_offered_only_for_a_members_unusable_keys_in_a_group() {
        let (ui, message) = room_with_failed_message("com.negzero.SendAnywayNegativeFixture", "m", true);
        for reason in [
            crate::crypto::Error::TrustRequired.to_string(),
            crate::crypto::Error::OwnDeviceTrustRequired.to_string(),
            "The connection is unavailable".to_owned(),
            "Legacy OMEMO is not ready".to_owned(),
        ] {
            ui.fail_queued_selection(OWNER, ROOM, vec![message.clone()], &reason);
            settle(300);
            assert!(shows_text(ui.toast.upcast_ref(), &reason[..20]), "{reason}");
            assert!(find_button(ui.toast.upcast_ref(), "Send Anyway").is_none(), "no Send Anyway for {reason:?}");
            ui.toast.dismiss_all();
            settle(500);
        }
        // One-to-one: the same refusal offers Resolve, as before.
        let direct = ui.state.borrow_mut().chat_for("peer@fixture.invalid").id.clone();
        ui.select_chat(&direct);
        ui.fail_queued_selection(OWNER, "peer@fixture.invalid", vec![message], &refusal());
        settle(300);
        assert!(find_button(ui.toast.upcast_ref(), "Send Anyway").is_none());
        assert!(find_button(ui.toast.upcast_ref(), "Resolve").is_some());
        ui.window.close();
    }

    /// A room that is not members-only and non-anonymous can never be encrypted: Resolve opens
    /// Conversation Security for that room (switching to it first), where the user chooses.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_room_that_cannot_be_encrypted_offers_resolve_to_conversation_security() {
        use crate::protocol::BackendEvent;
        let (ui, _) = room_with_failed_message("com.negzero.RoomNotPrivateFixture", "m", false);
        let other = ui.state.borrow_mut().chat_for("peer@fixture.invalid").id.clone();
        ui.select_chat(&other);
        settle(300);
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let reason = crate::rooms::Error::NotPrivate.to_string();
        ui.backend_event(generation, &owner, BackendEvent::Failed { peer: ROOM.into(), id: "wire-m".into(), reason: reason.clone() });
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "This room is not members-only and non-anonymous"));
        assert!(find_button(ui.toast.upcast_ref(), "Send Anyway").is_none());
        find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button").emit_clicked();
        settle(500);
        assert_eq!(ui.state.borrow().current().jid, ROOM, "the room is shown first");
        let dialog = ui.window.visible_dialog().expect("a dialog is open");
        assert_eq!(dialog.title(), "Conversation Security");
        ui.window.close();
    }

    /// Someone in the room who is not a listed member or hides their address: the toast names them
    /// and Resolve opens the participants.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn an_unlisted_occupant_offers_resolve_to_the_participants() {
        use crate::protocol::BackendEvent;
        let (ui, _) = room_with_failed_message("com.negzero.RoomOccupantFixture", "m", false);
        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let reason = crate::rooms::Error::OccupantNotListed("lurker".into()).to_string();
        ui.backend_event(generation, &owner, BackendEvent::Failed { peer: ROOM.into(), id: "wire-m".into(), reason });
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "lurker is in this room but is not a listed member"));
        assert!(ui.participants.borrow().is_none());
        find_button(ui.toast.upcast_ref(), "Resolve").expect("Resolve button").emit_clicked();
        settle(300);
        assert_eq!(ui.participants.borrow().as_ref().map(|view| view.room.clone()).as_deref(), Some(ROOM));
        ui.window.close();
    }

    /// Automatic selection ended at plaintext for a room that has used encryption (the usual cause is
    /// one member with no keys). The notice no longer blames the room and offers Send Anyway.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_blocked_downgrade_in_a_room_offers_send_anyway_not_plaintext() {
        let (ui, message) = room_with_failed_message("com.negzero.DowngradeFixture", "waiting", true);
        ui.fail_queued_downgrade(OWNER, ROOM, vec![message]);
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "This room could not be fully encrypted, so nothing was sent."));
        assert!(!shows_text(ui.toast.upcast_ref(), "no longer offers"), "no longer blames the room");
        assert!(find_button(ui.toast.upcast_ref(), "Resolve").is_none());
        let button = find_button(ui.toast.upcast_ref(), "Send Anyway").expect("Send Anyway button");
        assert_eq!(ui.store.message(OWNER, ROOM, "waiting").unwrap().delivery, "failed");
        let mut commands = capture(&ui);

        button.emit_clicked();
        settle(300);
        let stored = ui.store.message(OWNER, ROOM, "waiting").unwrap();
        assert!(stored.skip_unreachable);
        assert_ne!(stored.delivery, "failed", "retried");
        assert_eq!(probe_skip(&drain(&mut commands), ROOM), Some(true), "checked again, leaving members out");
        let chat = ui.state.borrow().chats.iter().find(|c| c.jid == ROOM).cloned().unwrap();
        assert_eq!(chat.selected_encryption(), None, "neither plaintext nor a locked protocol");
        ui.window.close();
    }

    /// Without messages to resend (or outside a group) there is no button, only the explanation.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_downgrade_notice_has_no_button_without_messages_to_resend() {
        let (ui, _) = room_with_failed_message("com.negzero.DowngradeNoButtonFixture", "m", true);
        ui.notice_downgrade_blocked(ROOM, vec![]);
        settle(300);
        assert!(shows_text(ui.toast.upcast_ref(), "To send unencrypted anyway, choose Unencrypted in Conversation Security."));
        assert!(!shows_text(ui.toast.upcast_ref(), "no longer offers"));
        assert!(find_button(ui.toast.upcast_ref(), "Send Anyway").is_none());
        ui.window.close();
    }

    fn find_css(widget: &gtk::Widget, class: &str) -> Vec<gtk::Widget> {
        let mut found = vec![];
        if widget.has_css_class(class) {
            found.push(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            found.extend(find_css(&current, class));
            child = current.next_sibling();
        }
        found
    }

    /// A message sent without some room members: no toast, a coloured lock and the
    /// tooltip "Sent partially" on its bubble, and the members with the reason in its details.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_partial_send_marks_the_bubble_and_explains_itself_in_the_details() {
        use crate::protocol::BackendEvent;
        let (ui, _) = room_with_failed_message("com.negzero.PartialSendFixture", "sent", false);
        ui.window.set_default_size(1000, 700);
        for (id, delivery) in [("sent", "sent"), ("plain", "sent")] {
            let mut state = ui.state.borrow_mut();
            if state.current().messages.iter().all(|m| m.id != id) {
                let mut extra = state.current().messages[0].clone();
                extra.id = id.into();
                extra.wire_id = format!("wire-{id}");
                extra.text = "another message".into();
                extra.timestamp = "2026-11-17T10:01:00Z".into();
                state.current_mut().messages.push(extra);
            }
            let message = state.current_mut().messages.iter_mut().find(|m| m.id == id).unwrap();
            message.encrypted = true;
            message.encryption_mode = Some(Mode::Legacy);
            message.delivery = delivery.into();
            let saved = message.clone();
            let chat = state.current().clone();
            drop(state);
            ui.store.save_chat_message(OWNER, &chat, Some(&saved)).unwrap();
        }
        assert!(ui.persist());
        ui.render_messages(true);
        settle(500);
        assert!(find_css(ui.window.upcast_ref(), "message-partial-lock").is_empty(), "nothing partial yet");

        let (generation, owner) = (ui.generation.get(), ui.state.borrow().account.clone());
        let why = "Legacy OMEMO keys require contact authorization from carol@example.org".to_owned();
        ui.backend_event(
            generation,
            &owner,
            BackendEvent::SentPartially { peer: ROOM.into(), id: "wire-sent".into(), skipped: vec![("carol@example.org".into(), why.clone())] },
        );
        settle(500);

        // No toast: the long text lives in the details.
        assert!(!shows_text(ui.toast.upcast_ref(), "Sent without"), "no toast");
        assert!(!shows_text(ui.toast.upcast_ref(), "carol@example.org"), "no toast");
        // Stored on the message, so it survives a restart.
        assert_eq!(ui.store.message(OWNER, ROOM, "sent").unwrap().skipped_members, vec![("carol@example.org".to_owned(), why.clone())]);
        assert!(ui.store.message(OWNER, ROOM, "plain").unwrap().skipped_members.is_empty());

        // Exactly one bubble is marked: the ordinary lock, coloured, with the tooltip.
        let marked = find_css(ui.window.upcast_ref(), "message-partial-lock");
        assert_eq!(marked.len(), 1, "only the partially sent message");
        assert_eq!(marked[0].tooltip_text().as_deref(), Some("Legacy OMEMO with problems. See Message Details"));
        let lock = marked[0].downcast_ref::<gtk::Image>().expect("the lock is an image");
        assert_eq!(lock.icon_name().as_deref(), Some("channel-secure-symbolic"), "the regular lock");
        assert!(lock.has_css_class("accent"), "coloured with the theme's accent");
        // It follows the theme: blue by default, and it changes with light and dark mode instead of
        // being a fixed colour.
        let manager = adw::StyleManager::default();
        let mut colours = vec![];
        for scheme in [adw::ColorScheme::ForceLight, adw::ColorScheme::ForceDark] {
            manager.set_color_scheme(scheme);
            settle(300);
            let colour = lock.color();
            assert!(colour.blue() > colour.red() && colour.blue() > colour.green(), "{scheme:?}: {colour:?} is blue");
            colours.push(colour);
        }
        assert_ne!(colours[0], colours[1], "the colour follows light and dark themes");
        manager.set_color_scheme(adw::ColorScheme::Default);
        // The other encrypted message keeps its ordinary, uncoloured lock.
        assert_eq!(find_css(ui.window.upcast_ref(), "accent").len(), 1, "only the partial lock is coloured");

        // The details say who and why.
        let message = ui.store.message(OWNER, ROOM, "sent").unwrap();
        ui.message_details(&message);
        settle(500);
        let dialog = ui.window.visible_dialog().expect("details dialog");
        let content = dialog.child().expect("dialog content");
        assert!(shows_text(&content, "Sent Partially"));
        assert!(shows_text(&content, "carol@example.org"));
        assert!(shows_text(&content, &why));
        ui.window.close();
    }

    /// Reacting in a room keeps working after Send Anyway. It used to lock the room to Legacy OMEMO,
    /// which has no protected message actions, so every reaction was refused. A reaction sent while
    /// some members' keys are unusable now goes out like the messages do.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn reacting_in_a_room_still_works_after_send_anyway() {
        const REFUSAL: &str = "Legacy OMEMO does not support protected message actions here";
        let (ui, _) = room_with_failed_message("com.negzero.ReactAfterSendAnywayFixture", "m", false);
        let mut commands = capture(&ui);
        ui.react("m", "\u{1f44d}");
        settle(300);
        assert!(!shows_text(ui.toast.upcast_ref(), REFUSAL), "reacting works before Send Anyway");
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((false, true)), "a reaction was sent");
        ui.toast.dismiss_all();
        settle(500);

        ui.send_anyway(ROOM, &["m".to_owned()]);
        settle(300);
        drain(&mut commands);
        ui.toast.dismiss_all();
        settle(500);
        ui.react("m", "\u{2764}");
        settle(300);
        assert!(!shows_text(ui.toast.upcast_ref(), REFUSAL), "reacting still works after Send Anyway");
        assert_eq!(
            sent_skip(&drain(&mut commands), ROOM),
            Some((true, true)),
            "the reaction is sent, leaving out the members Send Anyway skips"
        );
        assert_eq!(
            ui.state.borrow().chats.iter().find(|c| c.jid == ROOM).unwrap().selected_encryption(),
            None,
            "the room is not locked to Legacy OMEMO"
        );
        ui.window.close();
    }

    /// Send Anyway keeps applying to the room for a while, so the user is not asked for every
    /// message; it ends, it is per room, and the setting turns it off.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn send_anyway_lasts_thirty_minutes_per_room_and_can_be_turned_off() {
        assert_eq!(Ui::SEND_ANYWAY_COOLDOWN, std::time::Duration::from_secs(30 * 60));
        let (ui, message) = room_with_failed_message("com.negzero.SendAnywayCooldownFixture", "m", true);
        let mut commands = capture(&ui);
        let send = |ui: &Ui, peer: &str| {
            ui.command(Command::Send { peer: peer.into(), message: Message::default(), quote: String::new() });
        };
        send(&ui, ROOM);
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((false, false)), "not chosen yet");

        ui.fail_queued_selection(OWNER, ROOM, vec![message], &refusal());
        settle(300);
        find_button(ui.toast.upcast_ref(), "Send Anyway").expect("button").emit_clicked();
        settle(300);
        drain(&mut commands);
        ui.toast.dismiss_all();
        settle(500);

        // Later messages to this room skip without asking, and so does their encryption check.
        send(&ui, ROOM);
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((true, false)), "later messages skip");
        let fresh = Message { id: "fresh".into(), wire_id: "fresh".into(), own: true, outbox_pending: true, delivery: "queued".into(), ..Default::default() };
        ui.prepare_queued_peer(ROOM.into(), vec![fresh]);
        assert_eq!(probe_skip(&drain(&mut commands), ROOM), Some(true), "a new message's check skips too");
        // Another conversation is unaffected.
        send(&ui, "other@conference.example.org");
        assert_eq!(sent_skip(&drain(&mut commands), "other@conference.example.org"), Some((false, false)));

        // It ends.
        ui.send_anyway_until
            .borrow_mut()
            .insert(ROOM.into(), std::time::Instant::now() - std::time::Duration::from_secs(1));
        send(&ui, ROOM);
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((false, false)), "after the cooldown");

        // Turned off: nothing is skipped and no button is offered.
        ui.send_anyway_until
            .borrow_mut()
            .insert(ROOM.into(), std::time::Instant::now() + Ui::SEND_ANYWAY_COOLDOWN);
        ui.store.set_pref(&format!("offer_send_anyway:{OWNER}"), "false").unwrap();
        send(&ui, ROOM);
        assert_eq!(sent_skip(&drain(&mut commands), ROOM), Some((false, false)), "the setting is off");
        let m = Message { id: "again".into(), wire_id: "again".into(), own: true, outbox_pending: true, ..Default::default() };
        ui.fail_queued_selection(OWNER, ROOM, vec![m], &refusal());
        settle(300);
        assert!(find_button(ui.toast.upcast_ref(), "Send Anyway").is_none(), "no button when turned off");
        ui.window.close();
    }

    /// The switch in Preferences controls the button.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_preferences_switch_turns_send_anyway_off() {
        fn find_switch(widget: &gtk::Widget) -> Option<adw::SwitchRow> {
            if let Some(row) = widget.downcast_ref::<adw::SwitchRow>() {
                if row.title() == "Offer Send Anyway" {
                    return Some(row.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(found) = find_switch(&current) {
                    return Some(found);
                }
                child = current.next_sibling();
            }
            None
        }
        let (ui, _) = room_with_failed_message("com.negzero.SendAnywaySwitchFixture", "m", true);
        assert!(ui.offers_send_anyway(), "on by default");
        ui.preferences();
        settle(500);
        let dialog = ui.window.visible_dialog().expect("preferences");
        let row = find_switch(&dialog.child().expect("content")).expect("the Offer Send Anyway switch");
        assert!(row.is_active());
        row.set_active(false);
        settle(200);
        assert!(!ui.offers_send_anyway(), "the switch turns it off");
        row.set_active(true);
        settle(200);
        assert!(ui.offers_send_anyway());
        ui.window.close();
    }

    /// Earlier versions pinned a room to Legacy OMEMO for a day after Send Anyway. That is forgotten
    /// once, for rooms only, so reactions work again without waiting a day.
    #[test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn an_old_legacy_pin_on_a_room_is_cleared_once() {
        use crate::encryption_policy::Selection;
        let (ui, _) = room_with_failed_message("com.negzero.LegacyPinFixture", "m", false);
        let pinned = || Some(Selection { mode: Mode::Legacy, time_ms: chrono::Utc::now().timestamp_millis() });
        // The fixture already activated this account once; start from "not cleared yet".
        ui.store.set_pref(&format!("legacy_room_pins_cleared:{OWNER}"), "false").unwrap();
        let mut state = State::live(OWNER, vec![]);
        let room = state.chat_for(ROOM);
        room.group = true;
        room.automatic_encryption = pinned();
        let room_id = room.id.clone();
        let direct = state.chat_for("peer@fixture.invalid");
        direct.automatic_encryption = pinned();
        state.active = room_id;
        ui.activate_state(state);
        let selection = |ui: &Ui, jid: &str| ui.state.borrow().chats.iter().find(|c| c.jid == jid).unwrap().automatic_encryption.map(|s| s.mode);
        assert_eq!(selection(&ui, ROOM), None, "the room's old pin is forgotten");
        assert_eq!(selection(&ui, "peer@fixture.invalid"), Some(Mode::Legacy), "a direct chat keeps its choice");

        // Only once: a Legacy selection made later is left alone.
        let mut state = State::live(OWNER, vec![]);
        let room = state.chat_for(ROOM);
        room.group = true;
        room.automatic_encryption = pinned();
        let room_id = room.id.clone();
        state.active = room_id;
        ui.activate_state(state);
        assert_eq!(selection(&ui, ROOM), Some(Mode::Legacy), "not cleared a second time");
        ui.window.close();
    }
}
