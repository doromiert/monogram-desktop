//! Original payloads and control events persist separately so replay can be reconciled safely.
use crate::{
    message_actions::{self as actions, Action, Event},
    model::*,
    storage::Store,
};
use rusqlite::{OptionalExtension, params};
impl Store {
    pub fn init_actions(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS message_bases(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,peer,id));CREATE TABLE IF NOT EXISTS action_refs(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,own INTEGER NOT NULL,kind TEXT NOT NULL,value TEXT NOT NULL,message TEXT NOT NULL,PRIMARY KEY(account,peer,own,kind,value,message));CREATE INDEX IF NOT EXISTS action_reference ON action_refs(account,peer,value);CREATE TABLE IF NOT EXISTS action_events(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,own INTEGER NOT NULL,identity TEXT NOT NULL,target TEXT NOT NULL,local TEXT NOT NULL,wire TEXT NOT NULL,status TEXT NOT NULL,resolved TEXT,body TEXT NOT NULL,PRIMARY KEY(account,peer,own,identity));CREATE INDEX IF NOT EXISTS action_target ON action_events(account,peer,target);CREATE INDEX IF NOT EXISTS action_local ON action_events(account,peer,local);")?;
        // Upgrade cached unread counts once, before serde adds defaults to old rows.
        self.db.execute_batch("WITH ranked AS(SELECT account,peer,id,row_number() OVER(PARTITION BY account,peer ORDER BY timestamp DESC,id DESC) AS position FROM messages WHERE json_valid(body) AND json_extract(body,'$.own')=0) UPDATE messages SET body=json_set(body,'$.unread',json(CASE WHEN COALESCE((SELECT position FROM ranked r WHERE r.account=messages.account AND r.peer=messages.peer AND r.id=messages.id),2147483647)<=COALESCE((SELECT json_extract(c.body,'$.unread') FROM chats c WHERE c.account=messages.account AND c.peer=messages.peer),0) THEN 'true' ELSE 'false' END)) WHERE json_valid(body) AND json_type(body,'$.unread') IS NULL;INSERT OR IGNORE INTO message_bases SELECT account,peer,id,body FROM messages WHERE json_valid(body) AND COALESCE(json_type(body,'$.action'),'null')='null';")?;
        let rows = {
            let mut query=self.db.prepare("SELECT account,peer,body FROM message_bases WHERE NOT EXISTS(SELECT 1 FROM action_refs r WHERE r.account=message_bases.account AND r.peer=message_bases.peer AND r.message=message_bases.id)")?;
            query
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (a, peer, body) in rows {
            if let Ok(m) = serde_json::from_str::<Message>(&body) {
                self.add_refs(&a, &peer, &m)?;
            }
        }
        Ok(())
    }
    pub fn add_refs(&self, a: &str, peer: &str, m: &Message) -> rusqlite::Result<()> {
        for alias in &m.aliases {
            if let Some((kind, value)) = alias.split_once(':') {
                if matches!(kind, "wire" | "origin") && actions::valid_id(value) {
                    self.db.execute(
                        "INSERT OR IGNORE INTO action_refs VALUES(?,?,?,?,?,?)",
                        params![a, peer, m.own, kind, value, m.id],
                    )?;
                }
            }
        }
        if actions::valid_id(&m.wire_id) {
            self.db.execute(
                "INSERT OR IGNORE INTO action_refs VALUES(?,?,?,?,?,?)",
                params![a, peer, m.own, "wire", m.wire_id, m.id],
            )?;
        }
        Ok(())
    }
    pub fn save_base(&self, a: &str, peer: &str, m: &Message) -> rusqlite::Result<()> {
        if m.action.is_some() {
            return Ok(());
        }
        let body = serde_json::to_string(m).unwrap();
        self.db.execute(
            "INSERT OR IGNORE INTO message_bases VALUES(?,?,?,?)",
            params![a, peer, m.id, body],
        )?;
        self.db.execute("UPDATE message_bases SET body=CASE WHEN json_extract(body,'$.unavailable')=1 AND ?=0 THEN ? ELSE json_set(body,'$.pinned',json(?),'$.unread',json(?),'$.delivery',?,'$.aliases',json(?),'$.sender',CASE WHEN COALESCE(json_extract(body,'$.sender'),'')='' THEN ? ELSE json_extract(body,'$.sender') END,'$.fingerprint',CASE WHEN json_extract(body,'$.encrypted')=1 AND COALESCE(json_extract(body,'$.fingerprint'),'')='' THEN ? ELSE COALESCE(json_extract(body,'$.fingerprint'),'') END) END WHERE account=? AND peer=? AND id=? AND COALESCE(json_extract(body,'$.retracted'),0)=0",params![m.unavailable,body,if m.pinned{"true"}else{"false"},if m.unread{"true"}else{"false"},m.delivery,serde_json::to_string(&m.aliases).unwrap(),m.sender,m.fingerprint,a,peer,m.id])?;
        self.add_refs(a, peer, m)
    }
    pub fn record_action(
        &self,
        a: &str,
        peer: &str,
        m: &Message,
        status: &str,
    ) -> rusqlite::Result<()> {
        let Some(action) = &m.action else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        if m.sender.split('/').next() != Some(if m.own { a } else { peer })
            || !actions::valid_id(action.target())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if matches!(action, Action::React { .. }) {
            let stamp = chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                .map_err(|_| rusqlite::Error::InvalidQuery)?
                .timestamp_millis();
            if stamp < 0 || stamp > chrono::Utc::now().timestamp_millis() + 300000 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let tx = self.db.unchecked_transaction()?;
        let identity = format!("{}:{}", m.encrypted, actions::source_key(m));
        let chat = Chat {
            jid: peer.into(),
            name: peer.into(),
            ..Default::default()
        };
        tx.execute(
            "INSERT OR IGNORE INTO chats VALUES(?,?,?)",
            params![a, peer, serde_json::to_string(&chat).unwrap()],
        )?;
        let known: Option<String> = m
            .aliases
            .iter()
            .filter(|v| v.starts_with("omemo2:"))
            .find_map(|alias| {
                tx.query_row(
                    "SELECT message FROM aliases WHERE account=? AND peer=? AND own=? AND alias=?",
                    params![a, peer, m.own, alias],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| v.strip_prefix("event:").map(str::to_string))
            });
        if known.is_none() {
            tx.execute(
                "INSERT OR IGNORE INTO action_events VALUES(?,?,?,?,?,?,?,?,NULL,?)",
                params![
                    a,
                    peer,
                    m.own,
                    identity,
                    action.target(),
                    m.id,
                    m.wire_id,
                    status,
                    serde_json::to_string(m).unwrap()
                ],
            )?;
        }
        let local = if let Some(known) = known {
            known
        } else {
            tx.query_row("SELECT local FROM action_events WHERE account=? AND peer=? AND own=? AND identity=?",params![a,peer,m.own,identity],|r|r.get(0))?
        };
        for alias in &m.aliases {
            let old: Option<String> = tx
                .query_row(
                    "SELECT message FROM aliases WHERE account=? AND peer=? AND own=? AND alias=?",
                    params![a, peer, m.own, alias],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old {
                let unavailable: Option<String> = tx
                    .query_row(
                        "SELECT body FROM messages WHERE account=? AND peer=? AND id=?",
                        params![a, peer, old],
                        |r| r.get(0),
                    )
                    .optional()?;
                if unavailable.is_some_and(|body| {
                    serde_json::from_str::<Message>(&body).is_ok_and(|m| m.unavailable)
                }) {
                    for table in ["messages", "message_bases"] {
                        tx.execute(
                            &format!("DELETE FROM {table} WHERE account=? AND peer=? AND id=?"),
                            params![a, peer, old],
                        )?;
                    }
                    tx.execute(
                        "DELETE FROM action_refs WHERE account=? AND peer=? AND message=?",
                        params![a, peer, old],
                    )?;
                    tx.execute("UPDATE aliases SET message=? WHERE account=? AND peer=? AND own=? AND message=?",params![format!("event:{local}"),a,peer,m.own,old])?;
                }
            }
            tx.execute(
                "INSERT OR IGNORE INTO aliases VALUES(?,?,?,?,?)",
                params![a, peer, m.own, alias, format!("event:{local}")],
            )?;
        }
        tx.commit()
    }
    pub fn reconcile_actions(&self, a: &str, peer: &str) -> rusqlite::Result<bool> {
        let mut events = {
            let mut query=self.db.prepare("SELECT rowid,body,status FROM action_events WHERE account=? AND peer=? ORDER BY rowid")?;
            query
                .query_map(params![a, peer], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if events.is_empty() {
            return Ok(false);
        }
        let events: Vec<Event> = events
            .drain(..)
            .filter_map(|(row, body, status)| {
                serde_json::from_str::<Message>(&body)
                    .ok()
                    .map(|mut message| {
                        message.delivery = status.clone();
                        Event {
                            row,
                            failed: matches!(status.as_str(), "failed" | "uncertain")
                                || status == "sending"
                                    && matches!(message.action, Some(Action::Retract { .. })),
                            message,
                            resolved: None,
                        }
                    })
            })
            .collect();
        let bases = {
            let mut query=self.db.prepare("SELECT b.body FROM message_bases b WHERE b.account=? AND b.peer=? AND(b.id IN(SELECT r.message FROM action_refs r WHERE r.account=b.account AND r.peer=b.peer AND r.value IN(SELECT target FROM action_events e WHERE e.account=b.account AND e.peer=b.peer)) OR b.id IN(SELECT resolved FROM action_events e WHERE e.account=b.account AND e.peer=b.peer))")?;
            query
                .query_map(params![a, peer], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|body| serde_json::from_str::<Message>(&body).ok())
                .collect()
        };
        let projection = actions::project(bases, events, a, peer);
        let tx = self.db.unchecked_transaction()?;
        for event in &projection.events {
            tx.execute(
                "UPDATE action_events SET resolved=? WHERE rowid=?",
                params![event.resolved, event.row],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE account=? AND peer=? AND id=?",
                params![a, peer, event.message.id],
            )?;
        }
        let deleted: std::collections::BTreeSet<_> = projection
            .messages
            .iter()
            .filter(|m| m.retracted)
            .map(|m| m.id.clone())
            .collect();
        for mut m in projection.messages {
            if m.retracted {
                actions::scrub(&mut m);
                tx.execute(
                    "UPDATE message_bases SET body=? WHERE account=? AND peer=? AND id=?",
                    params![serde_json::to_string(&m).unwrap(), a, peer, m.id],
                )?;
            }
            tx.execute("INSERT INTO messages VALUES(?,?,?,?,?) ON CONFLICT(account,peer,id) DO UPDATE SET body=excluded.body",params![a,peer,m.id,m.timestamp,serde_json::to_string(&m).unwrap()])?;
        }
        for event in projection.events {
            if event
                .resolved
                .as_ref()
                .is_some_and(|id| deleted.contains(id))
                && matches!(event.message.action, Some(Action::Correct { .. }))
            {
                let mut m = event.message;
                actions::scrub(&mut m);
                tx.execute(
                    "UPDATE action_events SET body=? WHERE rowid=?",
                    params![serde_json::to_string(&m).unwrap(), event.row],
                )?;
            }
        }
        tx.execute("UPDATE chats SET body=json_set(body,'$.unread',(SELECT count(*) FROM messages m WHERE m.account=chats.account AND m.peer=chats.peer AND json_extract(m.body,'$.unread')=1 AND COALESCE(json_extract(m.body,'$.retracted'),0)=0)) WHERE account=? AND peer=?",params![a,peer])?;
        tx.commit()?;
        Ok(true)
    }
    pub fn action_delivery(
        &self,
        a: &str,
        peer: &str,
        wire: &str,
        status: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.db.execute(
            "UPDATE action_events SET status=CASE WHEN status='delivered' THEN status ELSE ? END WHERE account=? AND peer=? AND own=1 AND wire=?",
            params![status, a, peer, wire],
        )?;
        if changed > 0 {
            self.reconcile_actions(a, peer)?;
        }
        Ok(changed > 0)
    }
    pub fn interrupt_actions(&self, a: &str) -> rusqlite::Result<()> {
        self.db.execute("UPDATE action_events SET status='uncertain' WHERE account=? AND own=1 AND status='sending'",[a])?;
        Ok(())
    }
    pub fn failed_edit(&self, a: &str, peer: &str, target: &str) -> Option<String> {
        let mut query=self.db.prepare("SELECT body,status FROM action_events WHERE account=? AND peer=? AND own=1 AND target=? ORDER BY rowid DESC").ok()?;
        let latest = query
            .query_map(params![a, peer, target], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok()?
            .flatten()
            .filter_map(|(body, status)| {
                serde_json::from_str::<Message>(&body)
                    .ok()
                    .map(|m| (m, status))
            })
            .find(|(m, _)| matches!(m.action, Some(Action::Correct { .. })))?;
        matches!(latest.1.as_str(), "failed" | "uncertain").then_some(latest.0.text)
    }
    pub fn mark_read(&self, a: &str, peer: &str) -> rusqlite::Result<()> {
        for table in ["messages", "message_bases", "action_events"] {
            self.db.execute(&format!("UPDATE {table} SET body=json_set(body,'$.unread',json('false')) WHERE account=? AND peer=?"),params![a,peer])?;
        }
        Ok(())
    }
    pub fn unread_count(&self, a: &str, peer: &str) -> u32 {
        self.db.query_row("SELECT count(*) FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.unread')=1 AND COALESCE(json_extract(body,'$.retracted'),0)=0",params![a,peer],|r|r.get(0)).unwrap_or(0)
    }
    pub fn action_protected(&self, a: &str, peer: &str, id: &str) -> bool {
        self.db.query_row("SELECT EXISTS(SELECT 1 FROM action_events e WHERE account=? AND peer=? AND own=1 AND status!='failed' AND json_extract(body,'$.encrypted')=1 AND json_type(body,'$.action.React') IS NOT NULL AND(resolved=? OR target IN(SELECT value FROM action_refs WHERE account=e.account AND peer=e.peer AND message=?)))",params![a,peer,id,id],|r|r.get(0)).unwrap_or(true)
    }
    pub fn action_base(&self, a: &str, peer: &str, id: &str) -> rusqlite::Result<Message> {
        let body: String = self.db.query_row(
            "SELECT body FROM message_bases WHERE account=? AND peer=? AND id=?",
            params![a, peer, id],
            |r| r.get(0),
        )?;
        serde_json::from_str(&body).map_err(|_| rusqlite::Error::InvalidQuery)
    }
    pub fn action_target(
        &self,
        a: &str,
        peer: &str,
        id: &str,
        kind: &str,
    ) -> rusqlite::Result<Message> {
        let base = self.action_base(a, peer, id)?;
        let current = self
            .message(a, peer, id)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        if current.retracted
            || current.unavailable
            || base.unavailable
            || base.action.is_some()
            || kind != "react" && !base.own
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let reference = if kind == "react" {
            actions::reaction_reference(&base)
        } else {
            actions::original_wire(&base)
        }
        .ok_or(rusqlite::Error::InvalidQuery)?;
        let count: u32 = if kind == "react" {
            self.db.query_row("SELECT count(DISTINCT message) FROM action_refs WHERE account=? AND peer=? AND value=?",params![a,peer,reference],|r|r.get(0))?
        } else {
            self.db.query_row("SELECT count(DISTINCT message) FROM action_refs WHERE account=? AND peer=? AND own=1 AND kind='wire' AND value=?",params![a,peer,reference],|r|r.get(0))?
        };
        if count != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWNER: &str = "a@example.org";
    const PEER: &str = "b@example.org";
    fn setup() -> (Store, State) {
        let db = Store::open(true).unwrap();
        db.save_account(&Account {
            jid: OWNER.into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        })
        .unwrap();
        let mut s = State::live(OWNER, vec![]);
        s.chat_for(PEER);
        (db, s)
    }
    fn base(id: &str, wire: &str, own: bool) -> Message {
        Message {
            id: id.into(),
            wire_id: wire.into(),
            aliases: vec![format!("wire:{wire}"), format!("origin:o-{wire}")],
            sender: format!("{}/one", if own { OWNER } else { PEER }),
            own,
            text: "original".into(),
            timestamp: "2026-11-10T10:00:00.000000+00:00".into(),
            ..Default::default()
        }
    }
    fn event(id: &str, action: Action, own: bool, second: u32) -> Message {
        let mut m = base(id, id, own);
        m.action = Some(action);
        m.text = "corrected".into();
        m.timestamp = format!("2026-11-10T10:00:{second:02}.000000+00:00");
        m
    }
    fn save(db: &Store, s: &mut State, m: Message) {
        s.chat_for(PEER).messages.push(m);
        db.save(s).unwrap();
    }
    fn record(db: &Store, m: &Message, status: &str) {
        db.record_action(OWNER, PEER, m, status).unwrap();
        db.reconcile_actions(OWNER, PEER).unwrap();
    }
    #[test]
    fn delayed_edit_replay_and_collision_restore_immutable_original() {
        let (db, mut s) = setup();
        let edit = event(
            "edit",
            Action::Correct {
                target: "same".into(),
            },
            false,
            1,
        );
        record(&db, &edit, "received");
        assert_eq!(db.history(OWNER, PEER, None, 200).len(), 1);
        save(&db, &mut s, base("first", "same", false));
        db.reconcile_actions(OWNER, PEER).unwrap();
        let m = db.message(OWNER, PEER, "first").unwrap();
        assert!(m.edited);
        assert_eq!(m.text, "corrected");
        assert_eq!(m.edit_history, vec!["original", "corrected"]);
        record(&db, &edit, "received");
        assert_eq!(
            db.message(OWNER, PEER, "first").unwrap().edit_history.len(),
            2
        );
        let mut second = base("second", "same", false);
        second.sender = format!("{PEER}/two");
        second.text = "other original".into();
        assert!(db.known_message(OWNER, PEER, &second).is_none());
        save(&db, &mut s, second);
        db.reconcile_actions(OWNER, PEER).unwrap();
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().text, "original");
        assert!(!db.message(OWNER, PEER, "first").unwrap().edited);
        assert_eq!(db.history(OWNER, PEER, None, 200).len(), 3);
    }
    #[test]
    fn sender_scope_encryption_floor_and_exact_retraction_fingerprint() {
        let (db, mut s) = setup();
        let mut original = base("first", "wire", false);
        original.encrypted = true;
        original.fingerprint = "device-one".into();
        save(&db, &mut s, original.clone());
        let plain = event(
            "plain",
            Action::Correct {
                target: "wire".into(),
            },
            false,
            1,
        );
        record(&db, &plain, "received");
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().text, "original");
        let mut secure = event(
            "secure",
            Action::Correct {
                target: "wire".into(),
            },
            false,
            2,
        );
        secure.encrypted = true;
        secure.fingerprint = "device-two".into();
        record(&db, &secure, "received");
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().text, "corrected");
        let mut deletion = event(
            "delete",
            Action::Retract {
                target: "wire".into(),
                legacy: false,
            },
            false,
            3,
        );
        deletion.encrypted = true;
        deletion.fingerprint = "device-two".into();
        record(&db, &deletion, "received");
        assert!(!db.message(OWNER, PEER, "first").unwrap().retracted);
        deletion.id = "delete-correct-key".into();
        deletion.wire_id = deletion.id.clone();
        deletion.aliases = vec![format!("wire:{}", deletion.id)];
        deletion.fingerprint = "device-one".into();
        record(&db, &deletion, "received");
        let wiped = db.message(OWNER, PEER, "first").unwrap();
        assert!(wiped.retracted);
        assert!(wiped.text.is_empty());
        assert!(wiped.edit_history.is_empty());
        db.save(&s).unwrap();
        assert!(db.message(OWNER, PEER, "first").unwrap().text.is_empty());
        assert!(
            db.history(OWNER, PEER, None, 200)
                .iter()
                .all(|m| m.id != "first")
        );
        assert!(
            db.action_base(OWNER, PEER, "first")
                .unwrap()
                .text
                .is_empty()
        );
        let mut spoof = secure;
        spoof.sender = "attacker@example.org".into();
        assert!(db.record_action(OWNER, PEER, &spoof, "received").is_err());
    }
    #[test]
    fn reaction_replacement_empty_downgrade_and_corrected_alias() {
        let (db, mut s) = setup();
        save(&db, &mut s, base("first", "wire", true));
        let edit = event(
            "edit",
            Action::Correct {
                target: "wire".into(),
            },
            true,
            1,
        );
        record(&db, &edit, "sent");
        let mut reaction = event(
            "r1",
            Action::React {
                target: "edit".into(),
                emojis: ["❤".into(), "👍".into(), "invalid".into()].into(),
            },
            false,
            2,
        );
        reaction.encrypted = true;
        record(&db, &reaction, "received");
        assert_eq!(
            db.message(OWNER, PEER, "first").unwrap().reactions,
            vec!["❤️ 1", "👍 1"]
        );
        let downgrade = event(
            "r2",
            Action::React {
                target: "o-wire".into(),
                emojis: Default::default(),
            },
            false,
            3,
        );
        record(&db, &downgrade, "received");
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().reactions.len(), 2);
        let own = event(
            "r3",
            Action::React {
                target: "wire".into(),
                emojis: ["❤️".into()].into(),
            },
            true,
            4,
        );
        record(&db, &own, "sending");
        assert_eq!(
            db.message(OWNER, PEER, "first").unwrap().reactions,
            vec!["❤️ 2", "👍 1"]
        );
        db.action_delivery(OWNER, PEER, "r3", "failed").unwrap();
        assert!(
            db.message(OWNER, PEER, "first")
                .unwrap()
                .my_reactions
                .is_empty()
        );
        reaction.id = "r4".into();
        reaction.wire_id = "r4".into();
        reaction.aliases = vec!["wire:r4".into()];
        reaction.timestamp = "2026-11-10T10:00:05.000000+00:00".into();
        reaction.action = Some(Action::React {
            target: "wire".into(),
            emojis: Default::default(),
        });
        record(&db, &reaction, "received");
        assert!(
            db.message(OWNER, PEER, "first")
                .unwrap()
                .reactions
                .is_empty()
        );
    }
    fn poll() -> crate::polls::Poll {
        crate::polls::Poll {
            question: "Where?".into(),
            description: "Tonight".into(),
            options: vec![
                crate::polls::OptionValue {
                    emoji: "🍕".into(),
                    label: "Pizza".into(),
                },
                crate::polls::OptionValue {
                    emoji: "🍜".into(),
                    label: "Noodles".into(),
                },
            ],
            multiple: false,
            closed: false,
        }
    }
    fn ballot(id: &str, options: &[&str], second: u32, secure: bool) -> Message {
        let mut m = event(
            id,
            Action::React {
                target: "wire".into(),
                emojis: options.iter().map(|s| s.to_string()).collect(),
            },
            false,
            second,
        );
        m.encrypted = secure;
        m
    }
    #[test]
    fn structured_ballots_replay_revote_withdraw_and_freeze_at_authenticated_closure() {
        let (db, mut s) = setup();
        let mut original = base("poll", "wire", true);
        original.poll = Some(poll());
        original.encrypted = true;
        original.fingerprint = "own-key".into();
        record(&db, &ballot("early", &["🍜"], 1, true), "received");
        save(&db, &mut s, original);
        db.reconcile_actions(OWNER, PEER).unwrap();
        let tally = || db.message(OWNER, PEER, "poll").unwrap().poll_tally.unwrap();
        assert_eq!(tally().counts, vec![0, 1]);
        record(&db, &ballot("plain", &["🍕"], 2, false), "received");
        assert_eq!(tally().counts, vec![0, 1]);
        record(
            &db,
            &ballot("invalid-single", &["🍕", "🍜"], 3, true),
            "received",
        );
        assert_eq!(tally().ballots.len(), 0);
        record(&db, &ballot("revote", &["🍕", "❤️"], 4, true), "received");
        assert_eq!(tally().counts, vec![1, 0]);
        record(&db, &ballot("withdraw", &["❤️"], 5, true), "received");
        assert_eq!(tally().ballots.len(), 0);
        record(&db, &ballot("vote", &["🍕"], 6, true), "received");
        let mut closed = poll();
        closed.closed = true;
        let mut close = event(
            "close",
            Action::Correct {
                target: "wire".into(),
            },
            true,
            7,
        );
        close.encrypted = true;
        close.poll = Some(closed);
        record(&db, &close, "sent");
        record(&db, &ballot("too-late", &["🍜"], 8, true), "received");
        assert_eq!(tally().counts, vec![1, 0]);
        close.id = "close-again".into();
        close.wire_id = close.id.clone();
        close.aliases = vec!["wire:close-again".into()];
        close.timestamp = "2026-11-10T10:00:09.000000+00:00".into();
        record(&db, &close, "sent");
        assert_eq!(tally().counts, vec![1, 0]);
        db.load(OWNER);
        assert_eq!(tally().counts, vec![1, 0]);
        assert!(
            db.message(OWNER, PEER, "poll")
                .unwrap()
                .poll
                .unwrap()
                .closed
        );
    }
    #[test]
    fn changed_definition_and_foreign_closure_cannot_mutate_a_poll() {
        let (db, mut s) = setup();
        let mut original = base("poll", "wire", true);
        original.poll = Some(poll());
        save(&db, &mut s, original);
        let mut close = event(
            "bad-close",
            Action::Correct {
                target: "wire".into(),
            },
            true,
            1,
        );
        let mut definition = poll();
        definition.closed = true;
        definition.options[0].label = "Replaced".into();
        close.poll = Some(definition);
        record(&db, &close, "sent");
        assert!(
            !db.message(OWNER, PEER, "poll")
                .unwrap()
                .poll
                .unwrap()
                .closed
        );
        assert!(db.message(OWNER, PEER, "bad-close").unwrap().poll.is_none());
        close.id = "foreign".into();
        close.wire_id = close.id.clone();
        close.aliases = vec!["wire:foreign".into()];
        close.own = false;
        close.sender = PEER.into();
        close.poll.as_mut().unwrap().options[0].label = "Pizza".into();
        record(&db, &close, "received");
        assert!(
            !db.message(OWNER, PEER, "poll")
                .unwrap()
                .poll
                .unwrap()
                .closed
        );
    }
    #[test]
    fn bodyless_poll_id_collisions_are_not_deduplicated_by_empty_body() {
        let (db, mut s) = setup();
        let mut first = base("first", "same", true);
        first.poll = Some(poll());
        first.text.clear();
        first.sender = OWNER.into();
        save(&db, &mut s, first.clone());
        let mut different = first;
        different.id = "different".into();
        different.sender = format!("{OWNER}/other-resource");
        different.poll.as_mut().unwrap().question = "Different question".into();
        assert!(db.known_message(OWNER, PEER, &different).is_none());
        save(&db, &mut s, different);
        assert!(db.action_target(OWNER, PEER, "first", "correct").is_err());
    }
    #[test]
    fn own_encrypted_reaction_keeps_future_sends_protected_on_plain_original() {
        let (db, mut s) = setup();
        save(&db, &mut s, base("first", "wire", false));
        let mut event = ballot("own-protected", &["❤️"], 1, true);
        event.own = true;
        event.sender = OWNER.into();
        record(&db, &event, "sent");
        assert!(db.action_protected(OWNER, PEER, "first"));
        db.action_delivery(OWNER, PEER, "own-protected", "uncertain")
            .unwrap();
        assert!(db.action_protected(OWNER, PEER, "first"));
        db.action_delivery(OWNER, PEER, "own-protected", "failed")
            .unwrap();
        assert!(!db.action_protected(OWNER, PEER, "first"));
    }
    #[test]
    fn aliases_read_flags_and_pending_recovery_survive_reload() {
        let (db, mut s) = setup();
        let mut original = base("first", "wire", true);
        original.sender = OWNER.into();
        save(&db, &mut s, original.clone());
        let edit = event(
            "edit",
            Action::Correct {
                target: "late-alias".into(),
            },
            true,
            1,
        );
        record(&db, &edit, "sent");
        db.merge_aliases(OWNER, PEER, true, "first", &["wire:late-alias".into()])
            .unwrap();
        db.reconcile_actions(OWNER, PEER).unwrap();
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().text, "corrected");
        let mut echo = original.clone();
        echo.sender = format!("{OWNER}/device");
        assert_eq!(db.known_message(OWNER, PEER, &echo), Some("first".into()));
        let pending = event(
            "pending",
            Action::Correct {
                target: "wire".into(),
            },
            true,
            2,
        );
        record(&db, &pending, "sending");
        db.load(OWNER);
        assert_eq!(
            db.failed_edit(OWNER, PEER, "wire"),
            Some("corrected".into())
        );
        assert_eq!(
            db.db
                .query_row(
                    "SELECT status FROM action_events WHERE local='pending'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "uncertain"
        );
        let mut incoming = base("unread", "incoming", false);
        incoming.unread = true;
        save(&db, &mut s, incoming);
        db.mark_read(OWNER, PEER).unwrap();
        db.reconcile_actions(OWNER, PEER).unwrap();
        assert_eq!(db.unread_count(OWNER, PEER), 0);
    }
    #[test]
    fn failed_actions_and_pending_deletion_do_not_destroy_content() {
        let (db, mut s) = setup();
        save(&db, &mut s, base("first", "wire", true));
        let edit = event(
            "edit",
            Action::Correct {
                target: "wire".into(),
            },
            true,
            1,
        );
        record(&db, &edit, "sending");
        assert!(db.message(OWNER, PEER, "first").unwrap().edited);
        db.action_delivery(OWNER, PEER, "edit", "failed").unwrap();
        assert_eq!(db.message(OWNER, PEER, "first").unwrap().text, "original");
        assert_eq!(
            db.failed_edit(OWNER, PEER, "wire"),
            Some("corrected".into())
        );
        let delete = event(
            "delete",
            Action::Retract {
                target: "wire".into(),
                legacy: false,
            },
            true,
            2,
        );
        record(&db, &delete, "sending");
        assert!(!db.message(OWNER, PEER, "first").unwrap().retracted);
        db.action_delivery(OWNER, PEER, "delete", "sent").unwrap();
        assert!(db.message(OWNER, PEER, "first").unwrap().retracted);
        db.reconcile_actions(OWNER, PEER).unwrap();
        assert!(db.message(OWNER, PEER, "first").unwrap().text.is_empty());
        db.forget(OWNER).unwrap();
        assert_eq!(
            db.db
                .query_row("SELECT count(*) FROM action_events", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            0
        );
    }
}
