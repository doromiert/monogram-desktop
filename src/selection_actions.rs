//! Bulk selection resolves stored IDs without expanding the mounted history window.
use crate::{
    encryption_policy::Mode,
    message_actions::{self as actions, Action},
    model::*,
    protocol::Command,
    storage::Store,
    ui::Ui,
};
use gtk::glib;
use rusqlite::params;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub(crate) struct DeletePreparation {
    owner: String,
    peer: String,
    chat: String,
    generation: u64,
    ids: Vec<String>,
    targets: Vec<Message>,
    pub local_count: usize,
    pub own_count: usize,
    pub eligible_own_count: usize,
    pub ineligible_own_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWNER: &str = "owner@fixture.invalid";
    const PEER: &str = "peer@fixture.invalid";
    fn setup() -> (Store, State) {
        let store = Store::open(true).unwrap();
        store
            .save_account(&Account {
                jid: OWNER.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = State::live(OWNER, vec![]);
        state.chat_for(PEER);
        (store, state)
    }
    fn message(id: &str, own: bool) -> Message {
        Message {
            id: id.into(),
            wire_id: format!("wire-{id}"),
            aliases: vec![format!("wire:wire-{id}")],
            sender: format!("{}/one", if own { OWNER } else { PEER }),
            own,
            text: "private content".into(),
            delivery: "sent".into(),
            timestamp: "2026-11-12T10:00:00Z".into(),
            encryption_mode: Some(Mode::Plain),
            ..Default::default()
        }
    }
    fn request(base: &Message) -> Message {
        let wire = new_id();
        Message {
            id: wire.clone(),
            wire_id: wire.clone(),
            sender: OWNER.into(),
            own: true,
            aliases: vec![format!("wire:{wire}"), format!("origin:{wire}")],
            encrypted: base.mode().encrypted(),
            encryption_mode: Some(base.mode()),
            fingerprint: if base.mode().encrypted() {
                "sender-device".into()
            } else {
                String::new()
            },
            action: Some(Action::Retract {
                target: base.wire_id.clone(),
                legacy: false,
            }),
            delivery: "sending".into(),
            ..Default::default()
        }
    }
    #[test]
    fn mixed_deletion_survives_restart_and_retries_same_request_without_resurrection() {
        let (store, mut state) = setup();
        let mut own = message("own", true);
        own.encryption_mode = Some(Mode::Pq);
        own.encrypted = true;
        own.fingerprint = "original-device".into();
        let incoming = message("incoming", false);
        let mut unsent = message("unsent", true);
        unsent.outbox_pending = true;
        unsent.delivery = "queued".into();
        state.chat_for(PEER).messages = vec![own.clone(), incoming, unsent];
        store.save(&state).unwrap();
        assert!(store.eligible_retraction(OWNER, PEER, "incoming").is_none());
        assert!(store.eligible_retraction(OWNER, PEER, "unsent").is_none());
        let outgoing = request(&own);
        let wire = outgoing.wire_id.clone();
        let rows = store
            .apply_selected_deletion(
                OWNER,
                PEER,
                &["own".into(), "incoming".into(), "unsent".into()],
                &[("own".into(), outgoing.clone())],
            )
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(store.message(OWNER, PEER, "incoming").is_none());
        assert!(store.message(OWNER, PEER, "unsent").is_none());
        let tombstone = store.message(OWNER, PEER, "own").unwrap();
        assert!(tombstone.retracted && tombstone.text.is_empty() && tombstone.attachment.is_none());
        assert!(
            store
                .action_base(OWNER, PEER, "own")
                .unwrap()
                .text
                .is_empty()
        );
        assert!(store.history(OWNER, PEER, None, 100).is_empty());
        // Persist the exact DB then reopen it, without a GTK app or transport.
        let path = std::env::temp_dir().join(format!("monogram-selection-{}.sqlite", new_id()));
        store
            .db
            .execute("VACUUM INTO ?", [path.to_str().unwrap()])
            .unwrap();
        drop(store);
        let store = Store {
            db: rusqlite::Connection::open(&path).unwrap(),
        };
        let pending = store.pending_local_retractions(OWNER).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            serde_json::to_string(&pending[0].1).unwrap(),
            serde_json::to_string(&outgoing).unwrap()
        );
        assert!(store.claim_local_retraction(OWNER, PEER, &wire).unwrap());
        assert!(!store.claim_local_retraction(OWNER, PEER, &wire).unwrap());
        store.action_delivery(OWNER, PEER, &wire, "failed").unwrap();
        assert!(store.message(OWNER, PEER, "own").unwrap().retracted);
        store.interrupt_actions(OWNER).unwrap();
        store.reset_local_retractions(OWNER).unwrap();
        let retry = store.pending_local_retractions(OWNER).unwrap();
        assert_eq!(retry[0].1.wire_id, wire);
        assert_eq!(retry[0].1.fingerprint, "sender-device");
        assert_eq!(retry[0].1.mode(), Mode::Pq);
        assert!(store.claim_local_retraction(OWNER, PEER, &wire).unwrap());
        store.action_delivery(OWNER, PEER, &wire, "sent").unwrap();
        store.reset_local_retractions(OWNER).unwrap();
        assert!(store.pending_local_retractions(OWNER).unwrap().is_empty());
        assert!(!store.claim_local_retraction(OWNER, PEER, &wire).unwrap());
        // Stale archive/state saves cannot restore scrubbed own content.
        store.save(&state).unwrap();
        assert!(store.message(OWNER, PEER, "own").unwrap().text.is_empty());
        assert!(store.message(OWNER, PEER, "own").unwrap().retracted);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn incoming_spoof_rolls_back_entire_deletion_and_unchecked_is_local_only() {
        let (store, mut state) = setup();
        let own = message("own", true);
        let incoming = message("incoming", false);
        state.chat_for(PEER).messages = vec![own.clone(), incoming.clone()];
        store.save(&state).unwrap();
        let forged = request(&incoming);
        assert!(
            store
                .apply_selected_deletion(
                    OWNER,
                    PEER,
                    &["own".into(), "incoming".into()],
                    &[("own".into(), request(&own)), ("incoming".into(), forged)]
                )
                .is_err()
        );
        assert_eq!(store.history(OWNER, PEER, None, 100).len(), 2);
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM action_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .apply_selected_deletion(OWNER, PEER, &["own".into(), "incoming".into()], &[])
            .unwrap();
        assert!(store.history(OWNER, PEER, None, 100).is_empty());
        assert!(store.action_base(OWNER, PEER, "own").is_err());
        assert!(store.pending_local_retractions(OWNER).unwrap().is_empty());
    }
    #[test]
    fn own_pq_room_identity_is_eligible_but_incoming_room_author_is_not() {
        let (store, mut state) = setup();
        let mut own = message("room-own", true);
        own.encryption_mode = Some(Mode::Pq);
        own.encrypted = true;
        own.fingerprint = "sender-device".into();
        own.sender = format!("{PEER}/Owner");
        own.legacy_room_sender = Some(crate::rooms::members::RoomSender {
            room: PEER.into(),
            occupant: own.sender.clone(),
            author: OWNER.into(),
        });
        let mut incoming = own.clone();
        incoming.id = "room-incoming".into();
        incoming.wire_id = "incoming-wire".into();
        incoming.aliases = vec!["wire:incoming-wire".into()];
        incoming.own = false;
        incoming.sender = format!("{PEER}/Other");
        incoming.legacy_room_sender.as_mut().unwrap().author = "other@fixture.invalid".into();
        incoming.legacy_room_sender.as_mut().unwrap().occupant = incoming.sender.clone();
        state.chat_for(PEER).group = true;
        state.chat_for(PEER).messages = vec![own, incoming];
        store.save(&state).unwrap();
        assert!(store.eligible_retraction(OWNER, PEER, "room-own").is_some());
        assert!(
            store
                .eligible_retraction(OWNER, PEER, "room-incoming")
                .is_none()
        );
    }
    #[test]
    fn merged_legacy_caption_uses_stable_ids_and_is_deleted_with_parent() {
        let (store, mut state) = setup();
        let mut parent = message("parent-stable", true);
        parent.encrypted = true;
        parent.encryption_mode = Some(Mode::Legacy);
        parent.fingerprint = "device".into();
        parent.attachment = Some(crate::file_share::Attachment {
            wire_url: "https://fixture.invalid/file".into(),
            name: "file.png".into(),
            mime: "image/png".into(),
            size: Some(1),
            width: None,
            height: None,
            hashes: Default::default(),
            manual: false,
            sticker: false,
        });
        let mut caption = message("caption-stable", true);
        caption.encrypted = true;
        caption.encryption_mode = Some(Mode::Legacy);
        caption.fingerprint = parent.fingerprint.clone();
        caption.upload_caption_for = parent.wire_id.clone();
        caption.timestamp = "2026-11-13T10:00:00Z".into();
        state.chat_for(PEER).messages = vec![parent.clone(), caption.clone()];
        store.save(&state).unwrap();
        let rows = store
            .deletion_rows(OWNER, PEER, &[parent.id.clone()])
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|m| m.id == caption.id));
        store
            .apply_selected_deletion(
                OWNER,
                PEER,
                &[parent.id.clone(), caption.id.clone()],
                &[
                    (parent.id.clone(), request(&parent)),
                    (caption.id.clone(), request(&caption)),
                ],
            )
            .unwrap();
        assert_eq!(store.pending_local_retractions(OWNER).unwrap().len(), 2);
        assert!(store.history(OWNER, PEER, None, 100).is_empty());
    }
    #[test]
    fn local_day_dst_scope_and_unloaded_ids() {
        if std::env::var_os("MONOGRAM_DST_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "selection_actions::tests::local_day_dst_scope_and_unloaded_ids",
                    "--exact",
                    "--nocapture",
                ])
                .env("TZ", "Europe/Warsaw")
                .env("MONOGRAM_DST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            return;
        }
        let (store, mut state) = setup();
        for (id, stamp) in [
            ("before", "2026-03-28T22:59:59Z"),
            ("start", "2026-03-28T23:00:00Z"),
            ("anchor", "2026-03-29T01:30:00Z"),
            ("late", "2026-03-29T21:59:59Z"),
            ("after", "2026-03-29T22:00:00Z"),
        ] {
            let mut m = message(id, false);
            m.timestamp = stamp.into();
            state.chat_for(PEER).messages.push(m);
        }
        // Different RFC3339 offsets represent the same local day.
        let mut offset = message("offset", false);
        offset.timestamp = "2026-03-30T00:30:00+03:00".into();
        state.chat_for(PEER).messages.push(offset);
        let mut hidden = message("hidden", false);
        hidden.timestamp = "2026-03-29T14:00:00Z".into();
        hidden.retracted = true;
        state.chat_for(PEER).messages.push(hidden);
        let mut other = message("other-peer", false);
        other.timestamp = "2026-03-29T12:00:00Z".into();
        state.chat_for("other@fixture.invalid").messages.push(other);
        store.save(&state).unwrap();
        let ids: HashSet<_> = store
            .selection_day_ids(OWNER, PEER, "anchor")
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            ids,
            HashSet::from([
                "start".into(),
                "anchor".into(),
                "late".into(),
                "offset".into()
            ])
        );
        assert!(
            store
                .selection_day_ids("other-owner", PEER, "anchor")
                .is_err()
        );
        // The autumn day is 25 hours long; both repeated local 02:30 instances belong.
        state.chat_for(PEER).messages.clear();
        for (id, stamp) in [
            ("start", "2026-10-24T22:00:00Z"),
            ("first", "2026-10-25T00:30:00Z"),
            ("second", "2026-10-25T01:30:00Z"),
            ("late", "2026-10-25T22:59:59Z"),
            ("after", "2026-10-25T23:00:00Z"),
        ] {
            let mut m = message(&format!("fall-{id}"), false);
            m.timestamp = stamp.into();
            state.chat_for(PEER).messages.push(m);
        }
        store.save(&state).unwrap();
        let ids = store.selection_day_ids(OWNER, PEER, "fall-first").unwrap();
        assert_eq!(ids.len(), 4);
        assert!(!ids.contains(&"fall-after".into()));
    }
}

#[cfg(test)]
#[path = "../tests/selection_actions_native.rs"]
mod native;

fn local_day(timestamp: &str) -> Option<chrono::NaiveDate> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|date| date.with_timezone(&chrono::Local).date_naive())
}

impl Store {
    pub(crate) fn selection_day_ids(
        &self,
        owner: &str,
        peer: &str,
        anchor: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let stamp: String = self.db.query_row("SELECT timestamp FROM messages WHERE account=? AND peer=? AND id=? AND COALESCE(json_extract(body,'$.retracted'),0)=0 AND COALESCE(json_type(body,'$.action'),'null')='null'", params![owner,peer,anchor], |r| r.get(0))?;
        let day = local_day(&stamp).ok_or(rusqlite::Error::InvalidQuery)?;
        // Stored RFC3339 timestamps can have different offsets. Comparing date prefixes or
        // adding 86400 seconds is incorrect at local midnight and DST transitions.
        let mut query = self.db.prepare("SELECT id,timestamp FROM messages WHERE account=? AND peer=? AND COALESCE(json_extract(body,'$.retracted'),0)=0 AND COALESCE(json_type(body,'$.action'),'null')='null' ORDER BY timestamp,id")?;
        Ok(query
            .query_map(params![owner, peer], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, stamp)| local_day(stamp) == Some(day))
            .map(|(id, _)| id)
            .collect())
    }

    fn deletion_rows(
        &self,
        owner: &str,
        peer: &str,
        ids: &[String],
    ) -> rusqlite::Result<Vec<Message>> {
        let mut ids = self.deletion_targets(owner, peer, ids)?;
        // Pending legacy captions have no sent wire reference yet but still belong to
        // their parent upload and must be cancelled with it.
        for parent in ids.clone() {
            let mut q = self.db.prepare("SELECT c.id FROM upload_captions u CROSS JOIN messages c ON c.account=u.account AND c.peer=u.peer AND c.id=u.id WHERE u.account=? AND u.peer=? AND u.parent=? AND json_extract(c.body,'$.own')=1 AND json_extract(c.body,'$.delivery')='waiting-upload' AND json_extract(c.body,'$.encryption_mode')='Legacy'")?;
            ids.extend(
                q.query_map(params![owner, peer, parent], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        let mut seen = HashSet::new();
        Ok(ids
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .filter_map(|id| self.message(owner, peer, &id))
            .filter(|m| !m.retracted && m.action.is_none())
            .collect())
    }

    fn eligible_retraction(&self, owner: &str, peer: &str, id: &str) -> Option<Message> {
        let current = self.action_target(owner, peer, id, "retract").ok()?;
        let base = self.action_base(owner, peer, id).ok()?;
        let own_sender = base.sender.split('/').next() == Some(owner)
            || base.legacy_room_sender.as_ref().is_some_and(|sender| {
                sender.author == owner
                    && sender.room == peer
                    && sender.occupant == base.sender
                    && base.mode() == Mode::Pq
            });
        (base.own
            && current.own
            && own_sender
            && current.upload.is_none()
            && !current.outbox_pending
            && matches!(current.delivery.as_str(), "sent" | "delivered" | "read"))
        .then_some(base)
    }

    /// Atomically queue retractions and scrub their originals. Ordinary local removals
    /// use hard deletion; queued requests retain their immutable reference and action.
    fn apply_selected_deletion(
        &self,
        owner: &str,
        peer: &str,
        ids: &[String],
        requests: &[(String, Message)],
    ) -> rusqlite::Result<Vec<Message>> {
        let tx = self.db.unchecked_transaction()?;
        let rows = self.deletion_rows(owner, peer, ids)?;
        let mut queued = HashMap::new();
        for (id, request) in requests {
            let base = self
                .eligible_retraction(owner, peer, id)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            if !ids.contains(id)
                || !request.own
                || request.sender != owner
                || request.mode() != base.mode()
                || !matches!(&request.action,Some(Action::Retract{target,..}) if Some(target.clone())==actions::original_wire(&base))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            self.record_action_transaction(&tx, owner, peer, request, "sending")?;
            tx.execute(
                "INSERT INTO local_retractions(account,peer,wire) VALUES(?,?,?)",
                params![owner, peer, request.wire_id],
            )?;
            queued.insert(id.as_str(), request);
        }
        for row in &rows {
            if queued.contains_key(row.id.as_str()) {
                let mut tombstone = self.action_base(owner, peer, &row.id)?;
                actions::scrub(&mut tombstone);
                tombstone.upload = None;
                tombstone.outbox_pending = false;
                let body =
                    serde_json::to_string(&tombstone).map_err(|_| rusqlite::Error::InvalidQuery)?;
                for table in ["messages", "message_bases"] {
                    tx.execute(
                        &format!("UPDATE {table} SET body=? WHERE account=? AND peer=? AND id=?"),
                        params![body, owner, peer, row.id],
                    )?;
                }
                // Corrections can contain older private content, which is no longer
                // needed after local deletion. Keep their identity, not their payload.
                let mut q=tx.prepare("SELECT rowid,body FROM action_events WHERE account=? AND peer=? AND resolved=?")?;
                let events = q
                    .query_map(params![owner, peer, row.id], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(q);
                for (key, body) in events {
                    if let Ok(mut event) = Message::decode(&body) {
                        if matches!(event.action, Some(Action::Correct { .. })) {
                            actions::scrub(&mut event);
                            tx.execute(
                                "UPDATE action_events SET body=? WHERE rowid=?",
                                params![serde_json::to_string(&event).unwrap(), key],
                            )?;
                        }
                    }
                }
            } else {
                for table in ["messages", "message_bases"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE account=? AND peer=? AND id=?"),
                        params![owner, peer, row.id],
                    )?;
                }
                tx.execute(
                    "DELETE FROM action_refs WHERE account=? AND peer=? AND message=?",
                    params![owner, peer, row.id],
                )?;
                tx.execute(
                    "DELETE FROM action_events WHERE account=? AND peer=? AND resolved=?",
                    params![owner, peer, row.id],
                )?;
            }
        }
        tx.execute("UPDATE chats SET body=json_set(body,'$.unread',(SELECT count(*) FROM messages WHERE account=?1 AND peer=?2 AND json_extract(body,'$.unread')=1 AND COALESCE(json_extract(body,'$.retracted'),0)=0)) WHERE account=?1 AND peer=?2",params![owner,peer])?;
        tx.commit()?;
        Ok(rows)
    }

    pub(crate) fn reset_local_retractions(&self, owner: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE local_retractions SET attempted=0 WHERE account=? AND EXISTS(SELECT 1 FROM action_events e WHERE e.account=local_retractions.account AND e.peer=local_retractions.peer AND e.wire=local_retractions.wire AND e.own=1 AND e.status IN('failed','uncertain'))",
            [owner],
        )?;
        Ok(())
    }
    fn pending_local_retractions(&self, owner: &str) -> rusqlite::Result<Vec<(String, Message)>> {
        let mut q=self.db.prepare("SELECT r.peer,e.body FROM local_retractions r JOIN action_events e ON e.account=r.account AND e.peer=r.peer AND e.wire=r.wire AND e.own=1 WHERE r.account=? AND r.attempted=0 AND e.status NOT IN('sent','delivered','read') ORDER BY e.rowid")?;
        q.query_map([owner], |r| {
            let body: String = r.get(1)?;
            Ok((
                r.get(0)?,
                Message::decode(&body).map_err(|_| rusqlite::Error::InvalidQuery)?,
            ))
        })?
        .collect()
    }
    fn claim_local_retraction(
        &self,
        owner: &str,
        peer: &str,
        wire: &str,
    ) -> rusqlite::Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let claimed=tx.execute("UPDATE local_retractions SET attempted=1 WHERE account=?1 AND peer=?2 AND wire=?3 AND attempted=0 AND EXISTS(SELECT 1 FROM action_events e WHERE e.account=?1 AND e.peer=?2 AND e.wire=?3 AND e.own=1 AND e.status NOT IN('sent','delivered','read'))",params![owner,peer,wire])?==1;
        if claimed {
            tx.execute("UPDATE action_events SET status='sending' WHERE account=? AND peer=? AND own=1 AND wire=? AND status NOT IN('sent','delivered','read')",params![owner,peer,wire])?;
        }
        tx.commit()?;
        Ok(claimed)
    }
}

impl Ui {
    pub(crate) fn select_message_day(self: &Rc<Self>) {
        let selected = self.selected.borrow().clone();
        if selected.is_empty() {
            return;
        }
        let anchor = self.anchor.borrow().clone();
        let state = self.state.borrow();
        let (owner, peer) = (state.account.clone(), state.current().jid.clone());
        let id = anchor
            .as_ref()
            .filter(|id| selected.contains(*id))
            .cloned()
            .unwrap_or_else(|| selected.iter().min().unwrap().clone());
        if owner.is_empty() {
            let day = state
                .current()
                .messages
                .iter()
                .find(|m| m.id == id)
                .and_then(|m| local_day(&m.timestamp));
            if let Some(day) = day {
                self.selected.replace(
                    state
                        .current()
                        .messages
                        .iter()
                        .filter(|m| local_day(&m.timestamp) == Some(day))
                        .map(|m| m.id.clone())
                        .collect(),
                );
            }
            drop(state);
            self.update_selection();
            return;
        }
        drop(state);
        let generation = self.generation.get();
        self.notice("Selecting this day…");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let (a, p) = (owner.clone(), peer.clone());
            let fallback = selected.clone();
            let rows = ui
                .store
                .read_snapshot(move |store| {
                    let mut candidates: Vec<_> = fallback.into_iter().collect();
                    candidates.sort();
                    let valid = |id: &str| {
                        store.message(&a, &p, id).is_some_and(|m| {
                            !m.retracted && m.action.is_none() && local_day(&m.timestamp).is_some()
                        })
                    };
                    let id = if valid(&id) {
                        Some(id)
                    } else {
                        candidates.into_iter().find(|id| valid(id))
                    }
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                    let ids = store.selection_day_ids(&a, &p, &id)?;
                    Ok(store
                        .deletion_rows(&a, &p, &ids)?
                        .into_iter()
                        .map(|m| m.id)
                        .collect::<Vec<_>>())
                })
                .await;
            let state = ui.state.borrow();
            if state.account != owner
                || state.current().jid != peer
                || ui.generation.get() != generation
                || *ui.selected.borrow() != selected
                || *ui.anchor.borrow() != anchor
            {
                return;
            }
            drop(state);
            match rows {
                Ok(ids) => {
                    ui.selected.replace(ids.into_iter().collect());
                    ui.update_selection();
                }
                Err(_) => ui.notice("This message's local day could not be selected"),
            }
        });
    }

    pub(crate) async fn prepare_deletion(
        self: &Rc<Self>,
        ids: Vec<String>,
    ) -> Result<DeletePreparation, &'static str> {
        let state = self.state.borrow();
        let (owner, peer, chat, group) = (
            state.account.clone(),
            state.current().jid.clone(),
            state.active.clone(),
            state.current().group,
        );
        let generation = self.generation.get();
        drop(state);
        let (a, p) = (owner.clone(), peer.clone());
        let (rows, mut targets) = if owner.is_empty() {
            (
                self.state
                    .borrow()
                    .current()
                    .messages
                    .iter()
                    .filter(|m| ids.contains(&m.id))
                    .cloned()
                    .collect::<Vec<_>>(),
                Vec::new(),
            )
        } else {
            self.store
                .read_snapshot(move |store| {
                    let rows = store.deletion_rows(&a, &p, &ids)?;
                    let targets = rows
                        .iter()
                        .filter_map(|m| store.eligible_retraction(&a, &p, &m.id))
                        .collect::<Vec<_>>();
                    Ok((rows, targets))
                })
                .await
                .map_err(|_| "Message deletion could not be prepared")?
        };
        targets.retain(|m| !group || m.mode() == Mode::Pq);
        let own_count = rows.iter().filter(|m| m.own).count();
        Ok(DeletePreparation {
            owner,
            peer,
            chat,
            generation,
            local_count: rows.len(),
            own_count,
            eligible_own_count: targets.len(),
            ineligible_own_count: own_count - targets.len(),
            ids: rows.into_iter().map(|m| m.id).collect(),
            targets,
        })
    }

    pub(crate) fn apply_deletion(self: &Rc<Self>, prep: DeletePreparation, request_own: bool) {
        let state = self.state.borrow();
        if state.account != prep.owner
            || state.current().jid != prep.peer
            || state.active != prep.chat
            || self.generation.get() != prep.generation
        {
            return;
        }
        drop(state);
        if prep.owner.is_empty() {
            self.delete_messages(&prep.ids);
            return;
        }
        let mut requests = Vec::new();
        if request_own {
            for base in &prep.targets {
                let Some(fingerprint) = self.fingerprint_for_send(base.mode()) else {
                    self.notice("The original message's encryption is not ready. Nothing was deleted; reconnect and try again.");
                    return;
                };
                let Some(target) = actions::original_wire(base) else {
                    return;
                };
                let wire = new_id();
                requests.push((
                    base.id.clone(),
                    Message {
                        id: wire.clone(),
                        wire_id: wire.clone(),
                        aliases: vec![format!("wire:{wire}"), format!("origin:{wire}")],
                        sender: prep.owner.clone(),
                        author: "You".into(),
                        own: true,
                        encrypted: base.mode().encrypted(),
                        encryption_mode: Some(base.mode()),
                        fingerprint,
                        action: Some(Action::Retract {
                            target,
                            legacy: false,
                        }),
                        delivery: "sending".into(),
                        ..Default::default()
                    },
                ));
            }
        }
        self.notice("Deleting selected messages…");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui)=weak.upgrade() else{return};
            let (a,p,ids)=(prep.owner.clone(),prep.peer.clone(),prep.ids.clone());
            let result=ui.store.write_snapshot(move |store|store.apply_selected_deletion(&a,&p,&ids,&requests)).await;
            let rows=match result {Ok(rows)=>rows,Err(_)=>{ui.notice("Deletion could not be completed. Reopen the dialog and try again.");return;}};
            for row in &rows {
                ui.uploads.borrow_mut().cancel(&row.id);
                if let Some(file)=&row.attachment {ui.downloads.borrow_mut().cancel(&prep.owner,file);ui.images.borrow_mut().invalidate(file);}
                if row.upload.is_some() {
                    let (a,id)=(prep.owner.clone(),row.id.clone());
                    gtk::gio::spawn_blocking(move||{let _guard=crate::downloads::CACHE_IO.lock().unwrap();if let Ok(path)=crate::uploads::snapshot_path(&a,&id){let _=std::fs::remove_file(path);}});
                }
            }
            if ui.state.borrow().account==prep.owner {
                let removed:HashSet<_>=rows.iter().map(|m|m.id.as_str()).collect();
                let mut state=ui.state.borrow_mut();
                let exists=if let Some(chat)=state.chats.iter_mut().find(|chat|chat.jid==prep.peer) {
                    chat.messages.retain(|m|!removed.contains(m.id.as_str()));true
                } else {false};
                drop(state);
                if !exists {ui.flush_local_retractions();return;}
                ui.sync_latest_preview(&prep.owner,&prep.peer);
                if ui.state.borrow().current().jid==prep.peer {
                    ui.history_window.content_changed();ui.clear_selection();ui.render_messages(false);
                }
                ui.refresh_chats(None);
                if request_own && prep.ineligible_own_count>0 {ui.notice("Deleted locally. Only eligible sent messages have removal requests; unsent or unsupported messages were removed locally.");}
                ui.flush_local_retractions();
            }
        });
    }

    pub(crate) fn reconnect_local_retractions(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let account = owner.clone();
            if ui
                .store
                .write_snapshot(move |store| store.reset_local_retractions(&account))
                .await
                .is_ok()
                && ui.online.get()
                && ui.generation.get() == generation
                && ui.state.borrow().account == owner
            {
                ui.flush_local_retractions();
            }
        });
    }

    pub(crate) fn flush_local_retractions(self: &Rc<Self>) {
        if !self.online.get() {
            return;
        }
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(ui)=weak.upgrade() else{return};
            let a=owner.clone();
            let Ok(pending)=ui.store.read_snapshot(move|store|store.pending_local_retractions(&a)).await else{return};
            for (peer,message) in pending {
                if !ui.online.get() || ui.state.borrow().account!=owner || ui.generation.get()!=generation {return;}
                if !matches!(message.action,Some(Action::Retract{..})) || !message.own || message.sender!=owner || ui.fingerprint_for_send(message.mode()).as_deref()!=Some(message.fingerprint.as_str()) {continue;}
                if ui.state.borrow().chats.iter().find(|c|c.jid==peer).is_some_and(|c|c.group && (!c.room_can_send || !c.room_joined || message.mode()!=Mode::Pq)) {continue;}
                let (a,p,wire)=(owner.clone(),peer.clone(),message.wire_id.clone());
                if !ui.store.write_snapshot(move|store|store.claim_local_retraction(&a,&p,&wire)).await.unwrap_or(false){continue;}
                if !ui.online.get() || ui.state.borrow().account!=owner || ui.generation.get()!=generation {return;}
                if !ui.command(Command::Send{peer:peer.clone(),message:message.clone(),quote:String::new()}) {
                    let _=ui.store.action_delivery(&owner,&peer,&message.wire_id,"failed");
                    ui.notice("Messages were deleted locally. Removal requests are saved and will retry after reconnecting.");
                }
            }
        });
    }
}
