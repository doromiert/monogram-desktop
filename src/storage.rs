use crate::model::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
pub struct Store {
    pub db: Connection,
}
impl Store {
    pub fn open(memory: bool) -> rusqlite::Result<Self> {
        let db = if memory {
            Connection::open_in_memory()?
        } else {
            let root = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
                })
                .join("monogram-desktop");
            fs::create_dir_all(&root).map_err(|_| rusqlite::Error::InvalidPath(root.clone()))?;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
            let path = root.join("history.sqlite3");
            let db = Connection::open(&path)?;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            // Media history readers must not block foreground message writes.
            db.pragma_update(None, "journal_mode", "WAL")?;
            db
        };
        db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE IF NOT EXISTS accounts(jid TEXT PRIMARY KEY,config TEXT NOT NULL);CREATE TABLE IF NOT EXISTS chats(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,peer));CREATE TABLE IF NOT EXISTS messages(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,timestamp TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(account,peer,id),FOREIGN KEY(account,peer) REFERENCES chats(account,peer) ON DELETE CASCADE);CREATE INDEX IF NOT EXISTS message_order ON messages(account,peer,timestamp,id);CREATE TABLE IF NOT EXISTS aliases(account TEXT NOT NULL,peer TEXT NOT NULL,own INTEGER NOT NULL,alias TEXT NOT NULL,message TEXT NOT NULL,PRIMARY KEY(account,peer,own,alias));CREATE TABLE IF NOT EXISTS preferences(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
        let store = Self { db };
        store.init_actions()?;
        store.init_media()?;
        Ok(store)
    }
    pub fn accounts(&self) -> Vec<Account> {
        let mut stmt = self
            .db
            .prepare("SELECT config FROM accounts ORDER BY jid")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok().and_then(|v| serde_json::from_str(&v).ok()))
            .collect()
    }
    pub fn save_account(&self, a: &Account) -> rusqlite::Result<()> {
        self.db.execute("INSERT INTO accounts VALUES (?,?) ON CONFLICT(jid) DO UPDATE SET config=excluded.config",params![a.jid,serde_json::to_string(a).unwrap()])?;
        Ok(())
    }
    pub fn pref(&self, key: &str, default: &str) -> String {
        self.db
            .query_row("SELECT value FROM preferences WHERE key=?", [key], |r| {
                r.get(0)
            })
            .unwrap_or_else(|_| default.into())
    }
    pub fn set_pref(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO preferences VALUES (?,?)",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn blind_trust(&self, owner: &str, peer: &str) -> rusqlite::Result<bool> {
        if owner == peer {
            return Ok(false);
        }
        self.db
            .query_row(
                "SELECT value='true' FROM preferences WHERE key=?",
                [format!("blind-trust:{owner}\n{peer}")],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
    }
    pub fn set_blind_trust(&self, owner: &str, peer: &str, enabled: bool) -> rusqlite::Result<()> {
        if owner == peer || crate::protocol::bare(peer).ok().as_deref() != Some(peer) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.set_pref(
            &format!("blind-trust:{owner}\n{peer}"),
            if enabled { "true" } else { "false" },
        )
    }
    pub fn save(&self, s: &State) -> rusqlite::Result<()> {
        if s.account.is_empty() {
            return Ok(());
        }
        let tx = self.db.unchecked_transaction()?;
        for c in &s.chats {
            let mut body = serde_json::to_value(c).unwrap();
            body.as_object_mut().unwrap().remove("messages");
            tx.execute("INSERT INTO chats VALUES (?,?,?) ON CONFLICT(account,peer) DO UPDATE SET body=excluded.body",params![s.account,c.jid,body.to_string()])?;
            for m in &c.messages {
                self.upsert_message(&s.account, &c.jid, m)?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO preferences VALUES (?,?)",
            params![format!("active-chat:{}", s.account), s.active],
        )?;
        let folders = serde_json::json!({"rules":s.folders,"order":s.folder_order});
        tx.execute(
            "INSERT OR REPLACE INTO preferences VALUES (?,?)",
            params![format!("folders:{}", s.account), folders.to_string()],
        )?;
        tx.commit()
    }
    pub fn save_chat_message(
        &self,
        account: &str,
        chat: &Chat,
        message: Option<&Message>,
    ) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let mut metadata = serde_json::to_value(chat).unwrap();
        metadata.as_object_mut().unwrap().remove("messages");
        tx.execute("INSERT INTO chats VALUES (?,?,?) ON CONFLICT(account,peer) DO UPDATE SET body=excluded.body",
            params![account, chat.jid, metadata.to_string()])?;
        if let Some(message) = message {
            self.upsert_message(account, &chat.jid, message)?;
        }
        tx.commit()
    }
    fn upsert_message(&self, account: &str, peer: &str, m: &Message) -> rusqlite::Result<()> {
        if m.action.is_some() {
            return Ok(());
        }
        self.save_base(account, peer, m)?;
        self.db.execute("INSERT INTO messages VALUES (?,?,?,?,COALESCE((SELECT CASE WHEN json_extract(body,'$.retracted')=1 THEN body END FROM message_bases WHERE account=? AND peer=? AND id=?),?)) ON CONFLICT(account,peer,id) DO UPDATE SET body=excluded.body,timestamp=excluded.timestamp",params![account,peer,m.id,m.timestamp,account,peer,m.id,serde_json::to_string(m).unwrap()])?;
        for alias in &m.aliases {
            self.db.execute(
                "INSERT OR IGNORE INTO aliases VALUES (?,?,?,?,?)",
                params![account, peer, m.own, alias, m.id],
            )?;
        }
        Ok(())
    }
    pub fn restore_legacy_sticker(
        &self,
        account: &str,
        peer: &str,
        id: &str,
    ) -> rusqlite::Result<()> {
        if !self.message(account, peer, id).is_some_and(|message| {
            message.mode() == crate::encryption_policy::Mode::Legacy
                && !message.retracted
                && message.attachment.is_some_and(|file| {
                    !file.sticker && file.link().is_some_and(|link| link.encrypted())
                })
        }) {
            return Ok(());
        }
        let tx = self.db.unchecked_transaction()?;
        for table in ["messages", "message_bases"] {
            tx.execute(
                &format!("UPDATE {table} SET body=json_set(body,'$.attachment.sticker',json('true')) WHERE account=? AND peer=? AND id=? AND json_type(body,'$.attachment')='object'"),
                params![account, peer, id],
            )?;
        }
        tx.commit()
    }
    pub fn restore_legacy_reply(
        &self,
        account: &str,
        peer: &str,
        id: &str,
        reply: crate::crypto::legacy::service::LegacyReply,
    ) -> rusqlite::Result<()> {
        let original = if reply.quote.is_empty() {
            reply.text.clone()
        } else {
            format!("> {}\n{}", reply.quote.replace('\n', "\n> "), reply.text)
        };
        if !self.message(account, peer, id).is_some_and(|m| {
            m.mode() == crate::encryption_policy::Mode::Legacy
                && !m.retracted
                && !m.unavailable
                && !m.edited
                && m.reply_id.is_empty()
                && m.text == original
        }) {
            return Ok(());
        }
        let quote = self
            .resolve_reply(account, peer, &reply.to, &reply.id)
            .filter(|target| !target.retracted)
            .map(|target| {
                format!(
                    "{} · {}",
                    target.author,
                    target.preview().chars().take(80).collect::<String>()
                )
            })
            .unwrap_or(reply.quote);
        let tx = self.db.unchecked_transaction()?;
        for table in ["messages", "message_bases"] {
            tx.execute(
                &format!("UPDATE {table} SET body=json_set(body,'$.reply_id',?1,'$.reply_to',?2,'$.reply',?3,'$.text',?4) WHERE account=?5 AND peer=?6 AND id=?7 AND json_extract(body,'$.text')=?8 AND COALESCE(json_extract(body,'$.retracted'),0)=0"),
                params![reply.id, reply.to, quote, reply.text, account, peer, id, original],
            )?;
        }
        tx.commit()
    }
    pub fn update_delivery(
        &self,
        account: &str,
        peer: &str,
        wire: &str,
        status: &str,
    ) -> rusqlite::Result<Option<Message>> {
        let Some(id) = self.known(
            account,
            peer,
            true,
            &[format!("wire:{wire}"), format!("origin:{wire}")],
        ) else {
            return Ok(None);
        };
        let Some(mut message) = self
            .message(account, peer, &id)
            .filter(|m| m.own && m.wire_id == wire && !m.retracted)
        else {
            return Ok(None);
        };
        if message.delivery != "delivered" || status == "delivered" {
            message.delivery = status.into();
            let tx = self.db.unchecked_transaction()?;
            self.upsert_message(account, peer, &message)?;
            tx.commit()?;
        }
        Ok(Some(message))
    }
    pub fn interrupt_messages(&self, owner: &str) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for table in ["messages", "message_bases"] {
            tx.execute(&format!("UPDATE {table} SET body=json_set(body,'$.delivery','uncertain') WHERE account=? AND json_extract(body,'$.delivery')='sending' AND COALESCE(json_type(body,'$.upload'),'null')='null'"), [owner])?;
        }
        tx.commit()
    }
    pub fn load(&self, a: &str) -> State {
        self.load_state(a, false)
    }
    pub fn load_windowed(&self, a: &str) -> State {
        self.load_state(a, true)
    }
    fn load_state(&self, a: &str, windowed: bool) -> State {
        let _ = self.interrupt_uploads(a);
        let _ = self.interrupt_messages(a);
        let _ = self.interrupt_actions(a);
        let peers: Vec<String> = {
            let mut q = self
                .db
                .prepare("SELECT DISTINCT peer FROM action_events WHERE account=?")
                .unwrap();
            q.query_map([a], |r| r.get(0)).unwrap().flatten().collect()
        };
        for peer in peers {
            let _ = self.reconcile_actions(a, &peer);
        }
        let mut stmt = self
            .db
            .prepare("SELECT peer,body FROM chats WHERE account=? ORDER BY rowid")
            .unwrap();
        let rows = stmt
            .query_map([a], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap();
        let mut chats = vec![];
        for (peer, body) in rows.flatten() {
            if let Ok(mut c) = serde_json::from_str::<Chat>(&body) {
                if !windowed {
                    c.messages = self.history(a, &peer, None, 200);
                } else {
                    c.latest = self
                        .history(a, &peer, None, 1)
                        .first()
                        .map(MessagePreview::from);
                }
                chats.push(c)
            }
        }
        let mut s = State::live(a, chats);
        let active = self.pref(&format!("active-chat:{a}"), "");
        if s.chats.iter().any(|chat| chat.id == active) {
            s.active = active;
        }
        if windowed {
            let chat = s.current_mut();
            chat.messages = self.history(a, &chat.jid, None, crate::history_window::PAGE);
            chat.remember_latest();
        }
        if let Ok(data) =
            serde_json::from_str::<serde_json::Value>(&self.pref(&format!("folders:{a}"), ""))
        {
            if let (Ok(f), Ok(o)) = (
                serde_json::from_value(data["rules"].clone()),
                serde_json::from_value::<Vec<String>>(data["order"].clone()),
            ) {
                if !o.is_empty() {
                    s.folders = f;
                    s.folder_order = o;
                    s.folder = s.folder_order[0].clone()
                }
            }
        }
        s
    }
    pub fn history(
        &self,
        a: &str,
        peer: &str,
        before: Option<(&str, &str)>,
        limit: u32,
    ) -> Vec<Message> {
        self.try_history(a, peer, before, limit).unwrap_or_default()
    }
    pub fn try_history(
        &self,
        a: &str,
        peer: &str,
        before: Option<(&str, &str)>,
        limit: u32,
    ) -> rusqlite::Result<Vec<Message>> {
        let (time, id) = before.unwrap_or(("~", "~"));
        let mut stmt=self.db.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND (timestamp,id)<(?,?) AND COALESCE(json_extract(body,'$.retracted'),0)=0 ORDER BY timestamp DESC,id DESC LIMIT ?")?;
        let mut rows: Vec<_> = stmt
            .query_map(params![a, peer, time, id, limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|body| Message::decode(&body).ok())
            .filter(|m| !m.retracted)
            .collect();
        rows.reverse();
        Ok(rows)
    }
    pub fn known(&self, a: &str, peer: &str, own: bool, aliases: &[String]) -> Option<String> {
        aliases.iter().find_map(|alias| {
            self.db
                .query_row(
                    "SELECT message FROM aliases WHERE account=? AND peer=? AND own=? AND alias=?",
                    params![a, peer, own, alias],
                    |r| r.get(0),
                )
                .ok()
        })
    }
    // A client stanza ID is not unique across the author's resources.
    pub fn known_message(&self, a: &str, peer: &str, message: &Message) -> Option<String> {
        if let Some(alias) = message
            .aliases
            .iter()
            .find(|alias| alias.starts_with("room-archive:"))
        {
            let aliases = std::slice::from_ref(alias);
            // A room-assigned ID survives an archive that withholds the sender's real JID.
            if let Some(id) = self
                .known(a, peer, message.own, aliases)
                .or_else(|| self.known(a, peer, !message.own, aliases))
            {
                return Some(id);
            }
        }
        let mut aliases = message.aliases.clone();
        aliases.sort_by_key(|v| {
            !v.starts_with("omemo2:")
                && !v.starts_with("legacy-omemo:")
                && !v.starts_with("server:")
                && !v.starts_with("archive:")
        });
        for alias in aliases {
            let Some(id) = self.known(a, peer, message.own, &[alias.clone()]) else {
                continue;
            };
            let Some(old) = self
                .action_base(a, peer, &id)
                .ok()
                .or_else(|| self.message(a, peer, &id))
            else {
                return Some(id);
            };
            if alias.starts_with("omemo2:")
                || alias.starts_with("legacy-omemo:")
                || alias.starts_with("server:")
                || alias.starts_with("archive:")
                || same_source(&old, message)
            {
                return Some(id);
            }
        }
        if message.wire_id.is_empty() {
            return None;
        }
        let mut q = self.db.prepare("SELECT body FROM messages WHERE account=? AND peer=? AND json_extract(body,'$.own')=? AND json_extract(body,'$.wire_id')=?").ok()?;
        q.query_map(params![a, peer, message.own, message.wire_id], |r| {
            r.get::<_, String>(0)
        })
        .ok()?
        .flatten()
        .filter_map(|body| Message::decode(&body).ok())
        .find(|m| same_source(m, message))
        .map(|m| m.id)
    }
    pub fn delete(&self, a: &str, peer: &str, id: &str) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for table in ["messages", "message_bases"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE account=? AND peer=? AND id=?"),
                params![a, peer, id],
            )?;
        }
        tx.execute(
            "DELETE FROM action_refs WHERE account=? AND peer=? AND message=?",
            params![a, peer, id],
        )?;
        tx.execute(
            "DELETE FROM action_events WHERE account=? AND peer=? AND resolved=?",
            params![a, peer, id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn message(&self, a: &str, peer: &str, id: &str) -> Option<Message> {
        self.db
            .query_row(
                "SELECT body FROM messages WHERE account=? AND peer=? AND id=?",
                params![a, peer, id],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|body| Message::decode(&body).ok())
    }
    pub fn merge_aliases(
        &self,
        a: &str,
        peer: &str,
        own: bool,
        id: &str,
        aliases: &[String],
    ) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for alias in aliases {
            tx.execute(
                "INSERT OR IGNORE INTO aliases VALUES(?,?,?,?,?)",
                params![a, peer, own, alias, id],
            )?;
        }
        if let Some(mut m) = self.message(a, peer, id) {
            for alias in aliases {
                if !m.aliases.contains(alias) {
                    m.aliases.push(alias.clone());
                }
            }
            self.save_base(a, peer, &m)?;
            tx.execute(
                "UPDATE messages SET body=? WHERE account=? AND peer=? AND id=?",
                params![serde_json::to_string(&m).unwrap(), a, peer, id],
            )?;
        } else if let Some(local) = id.strip_prefix("event:") {
            let body:Option<String>=tx.query_row("SELECT body FROM action_events WHERE account=? AND peer=? AND own=? AND local=?",params![a,peer,own,local],|r|r.get(0)).optional()?;
            if let Some(mut m) = body.and_then(|body| Message::decode(&body).ok()) {
                for alias in aliases {
                    if !m.aliases.contains(alias) {
                        m.aliases.push(alias.clone());
                    }
                }
                tx.execute("UPDATE action_events SET body=? WHERE account=? AND peer=? AND own=? AND local=?",params![serde_json::to_string(&m).unwrap(),a,peer,own,local])?;
            }
        }
        tx.commit()
    }
    pub fn forget(&self, a: &str) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute("DELETE FROM accounts WHERE jid=?", [a])?;
        tx.execute("DELETE FROM aliases WHERE account=?", [a])?;
        tx.execute(
            "DELETE FROM preferences WHERE key=?",
            [format!("folders:{a}")],
        )?;
        let trust_prefix = format!("blind-trust:{a}\n");
        tx.execute(
            "DELETE FROM preferences WHERE substr(key,1,length(?1))=?1",
            [trust_prefix],
        )?;
        tx.commit()
    }
}
pub fn same_source(old: &Message, new: &Message) -> bool {
    if let (Some(old), Some(new)) = (&old.legacy_room_sender, &new.legacy_room_sender)
        && (old.room != new.room || old.author != new.author)
    {
        return false;
    }
    let room_id = |message: &Message| {
        message
            .aliases
            .iter()
            .find(|alias| alias.starts_with("room-archive:"))
            .cloned()
    };
    if let (Some(old), Some(new)) = (room_id(old), room_id(new))
        && old != new
    {
        return false;
    }
    old.own == new.own
        && (old.sender == new.sender
            || (!old.sender.contains('/') || !new.sender.contains('/'))
                && (old.unavailable
                    || new.unavailable
                    || old.text == new.text && old.poll == new.poll))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_migrate_and_isolate() {
        let db = Store::open(true).unwrap();
        let a = Account {
            jid: "a@example.org".into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        };
        db.save_account(&a).unwrap();
        let mut s = State::live(&a.jid, vec![]);
        s.current_mut().draft = "draft 🌿".into();
        s.send("hello", "text");
        db.save(&s).unwrap();
        db.save_account(&a).unwrap();
        let restored = db.load(&a.jid);
        assert_eq!(restored.current().draft, "draft 🌿");
        assert_eq!(restored.current().messages.len(), 1);
        assert!(db.load("other@example.org").current().messages.is_empty());
        let m = &restored.current().messages[0];
        assert_eq!(
            db.known(&a.jid, &a.jid, true, &m.aliases),
            Some(m.id.clone())
        );
        db.delete(&a.jid, &a.jid, &m.id).unwrap();
        assert!(db.load(&a.jid).current().messages.is_empty());
        assert!(db.known(&a.jid, &a.jid, true, &m.aliases).is_some());
    }
}

impl Store {
    // Pending placeholders must never become the immutable wire payload used by corrections.
    pub fn update_upload(
        &self,
        owner: &str,
        peer: &str,
        message: &Message,
    ) -> rusqlite::Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE account=? AND peer=? AND id=? AND json_type(body,'$.upload')='object')",rusqlite::params![owner,peer,message.id],|r|r.get(0))?;
        if !exists {
            return Ok(false);
        }
        if message.upload.is_none() {
            self.save_base(owner, peer, message)?;
        }
        tx.execute(
            "UPDATE messages SET body=? WHERE account=? AND peer=? AND id=?",
            rusqlite::params![
                serde_json::to_string(message).unwrap(),
                owner,
                peer,
                message.id
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn interrupt_uploads(&self, owner: &str) -> rusqlite::Result<usize> {
        self.db.execute("UPDATE messages SET body=json_set(body,'$.delivery','failed','$.upload.error','Upload interrupted. Retry to send.') WHERE account=? AND json_type(body,'$.upload')='object' AND COALESCE(json_extract(body,'$.delivery'),'')='sending'",[owner])
    }
}
