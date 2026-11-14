use crate::model::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
#[path = "request_store.rs"]
mod request_store;
#[path = "rooms/store.rs"]
mod room_store;
pub use request_store::{ContactRequest, PendingRequests, RoomInvitation};
pub struct Store {
    pub db: Connection,
}
const READY_UPLOAD_CAPTIONS: &str = "SELECT dependency.peer,caption.body
    FROM upload_captions AS dependency
    CROSS JOIN messages AS caption CROSS JOIN messages AS parent
    WHERE dependency.account=?1
      AND caption.account=dependency.account AND caption.peer=dependency.peer AND caption.id=dependency.id
      AND parent.account=dependency.account AND parent.peer=dependency.peer AND parent.id=dependency.parent
      AND json_extract(caption.body,'$.own')=1
      AND json_extract(caption.body,'$.encrypted')=1 AND json_extract(caption.body,'$.encryption_mode')='Legacy'
      AND json_extract(caption.body,'$.delivery')='waiting-upload'
      AND json_extract(caption.body,'$.upload_caption_for')=dependency.parent
      AND COALESCE(json_extract(caption.body,'$.retracted'),0)=0
      AND COALESCE(json_type(caption.body,'$.upload'),'null')='null'
      AND COALESCE(json_type(caption.body,'$.action'),'null')='null'
      AND json_extract(parent.body,'$.own')=1
      AND json_extract(parent.body,'$.wire_id')=dependency.parent
      AND json_extract(parent.body,'$.encrypted')=1 AND json_extract(parent.body,'$.encryption_mode')='Legacy'
      AND json_extract(parent.body,'$.delivery') IN ('sent','delivered','read')
      AND COALESCE(json_extract(parent.body,'$.retracted'),0)=0
      AND COALESCE(json_type(parent.body,'$.upload'),'null')='null'
      AND json_type(parent.body,'$.attachment')='object'";
impl Store {
    pub(crate) fn init_selection_actions(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS local_retractions(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,wire TEXT NOT NULL,attempted INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(account,peer,wire));")
    }

    pub async fn write_snapshot<T: Send + 'static>(
        &self,
        write: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let Some(path) = self
            .db
            .path()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
        else {
            return write(self);
        };
        gtk::gio::spawn_blocking(move || {
            let db = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            db.busy_timeout(std::time::Duration::from_millis(250))?;
            db.pragma_update(None, "foreign_keys", "ON")?;
            write(&Store { db })
        })
        .await
        .map_err(|_| rusqlite::Error::InvalidQuery)?
    }
    pub fn init_link_previews(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS link_previews(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,url TEXT NOT NULL,expires INTEGER NOT NULL,body TEXT,PRIMARY KEY(account,url));CREATE INDEX IF NOT EXISTS link_previews_expiry ON link_previews(account,expires);")
    }
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
        db.busy_timeout(std::time::Duration::from_millis(250))?;
        let store = Self { db };
        store.init_actions()?;
        store.init_selection_actions()?;
        store.init_media()?;
        store.init_rooms()?;
        store.init_requests()?;
        store.init_read_markers()?;
        store.init_link_previews()?;
        store.db.execute_batch("CREATE INDEX IF NOT EXISTS queued_outbox ON messages(account,timestamp,id) WHERE json_extract(body,'$.outbox_pending')=1;")?;
        store.db.execute_batch("CREATE TABLE IF NOT EXISTS upload_captions(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,parent TEXT NOT NULL,PRIMARY KEY(account,peer,id),FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);CREATE INDEX IF NOT EXISTS upload_caption_parent ON upload_captions(account,peer,parent);")?;
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
    pub fn import_synced_account(&self,a:&Account)->rusqlite::Result<()>{
        let tx=rusqlite::Transaction::new_unchecked(&self.db,rusqlite::TransactionBehavior::Immediate)?;
        let excluded:String=tx.query_row("SELECT COALESCE((SELECT value FROM preferences WHERE key=?),'')",[format!("lan-sync-excluded:{}",a.jid)],|r|r.get(0))?;
        if excluded=="true"{return Err(rusqlite::Error::InvalidQuery);}
        tx.execute("INSERT INTO accounts VALUES (?,?) ON CONFLICT(jid) DO UPDATE SET config=excluded.config",rusqlite::params![a.jid,serde_json::to_string(a).unwrap()])?;tx.commit()
    }
    pub fn save_account(&self, a: &Account) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM preferences WHERE key=?",[format!("lan-sync-excluded:{}",a.jid)])?;
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
    pub(crate) fn blind_trust_for(
        &self,
        owner: &str,
        peer: &str,
        conversation: Option<&str>,
    ) -> rusqlite::Result<bool> {
        Ok(self.blind_trust(owner, peer)?
            || conversation
                .map(|scope| self.blind_trust(owner, scope))
                .transpose()?
                .unwrap_or(false))
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
    pub fn save(&self,s:&State)->rusqlite::Result<()>{self.save_state(s,true)}
    pub(crate)fn save_chat_state(&self,s:&State)->rusqlite::Result<()>{self.save_state(s,false)}
    fn save_state(&self, s: &State,folders_changed:bool) -> rusqlite::Result<()> {
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
        if folders_changed {
        let previous:serde_json::Value=serde_json::from_str(&self.pref(&format!("folders:{}",s.account),"{}")).unwrap_or_default();
        let mut rooms=std::collections::BTreeSet::<String>::new();for value in previous["rooms"].as_array().into_iter().flatten(){if let Some(jid)=value.as_str(){rooms.insert(jid.into());}}
        rooms.extend(s.chats.iter().filter(|c|c.group).map(|c|c.id.clone()));
        let folders = serde_json::json!({"rules":s.folders,"order":s.folder_order,"all_hidden":s.all_folder_hidden,"rooms":rooms});
        tx.execute(
            "INSERT OR REPLACE INTO preferences VALUES (?,?)",
            params![format!("folders:{}", s.account), folders.to_string()],
        )?;
        }
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
    pub(crate) fn save_outbox_row(
        &self,
        account: &str,
        peer: &str,
        message: &Message,
    ) -> rusqlite::Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.upsert_message(account, peer, message)?;
        transaction.commit()
    }
    pub(crate) fn save_outbox_pair(
        &self,
        account: &str,
        peer: &str,
        media: &Message,
        caption: &Message,
    ) -> rusqlite::Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.upsert_message(account, peer, media)?;
        self.upsert_message(account, peer, caption)?;
        transaction.commit()
    }
    fn upsert_message(&self, account: &str, peer: &str, m: &Message) -> rusqlite::Result<()> {
        let confirmed;
        let m = if m.own && m.delivery != "read" && self.confirmed_read(account, peer, &m.id) {
            confirmed = {
                let mut message = m.clone();
                message.delivery = "read".into();
                message
            };
            &confirmed
        } else {
            m
        };
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
        if m.own
            && m.mode() == crate::encryption_policy::Mode::Legacy
            && !m.upload_caption_for.is_empty()
            && m.delivery == "waiting-upload"
            && !m.retracted
        {
            self.db.execute("INSERT INTO upload_captions VALUES(?,?,?,?) ON CONFLICT(account,peer,id) DO UPDATE SET parent=excluded.parent",
                params![account,peer,m.id,m.upload_caption_for])?;
        } else {
            self.db.execute(
                "DELETE FROM upload_captions WHERE account=? AND peer=? AND id=?",
                params![account, peer, m.id],
            )?;
        }
        Ok(())
    }
    pub fn ready_upload_captions(&self, owner: &str) -> rusqlite::Result<Vec<(String, Message)>> {
        let mut query = self.db.prepare(&format!(
            "{READY_UPLOAD_CAPTIONS} ORDER BY caption.timestamp,caption.id LIMIT 64"
        ))?;
        query.query_map([owner], upload_caption_row)?.collect()
    }
    pub fn claim_upload_caption(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
    ) -> rusqlite::Result<Option<Message>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let caption = tx
            .query_row(
                &format!("{READY_UPLOAD_CAPTIONS} AND dependency.peer=?2 AND dependency.id=?3"),
                params![owner, peer, id],
                upload_caption_row,
            )
            .optional()?;
        let Some((_, mut message)) = caption else {
            tx.commit()?;
            return Ok(None);
        };
        message.delivery = "sending".into();
        self.upsert_message(owner, peer, &message)?;
        tx.commit()?;
        Ok(Some(message))
    }
    pub fn restore_preview_spoilers(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        context: &crate::spoilers::Context,
    ) -> rusqlite::Result<bool> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut changed = false;
        for table in ["messages", "message_bases"] {
            let body: Option<String> = transaction
                .query_row(
                    &format!("SELECT body FROM {table} WHERE account=? AND peer=? AND id=?"),
                    params![owner, peer, id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(mut message) = body.and_then(|body| Message::decode(&body).ok()) else {
                continue;
            };
            if message.unavailable
                || message.preview_spoilers.is_some()
                || !context.matches(&message.text)
            {
                continue;
            }
            message.preview_spoilers = Some(context.clone());
            transaction.execute(
                &format!("UPDATE {table} SET body=? WHERE account=? AND peer=? AND id=?"),
                params![
                    serde_json::to_string(&message).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    owner,
                    peer,
                    id
                ],
            )?;
            changed = true;
        }
        transaction.commit()?;
        Ok(changed)
    }
    pub fn restore_inline_emoji(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        emoji: &crate::packs::InlineEmoji,
    ) -> rusqlite::Result<bool> {
        let Ok(validated) =
            crate::packs::InlineEmoji::new(emoji.body.clone(), emoji.ranges.clone())
        else {
            return Ok(false);
        };
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut changed = false;
        for table in ["messages", "message_bases"] {
            let body: Option<String> = transaction
                .query_row(
                    &format!("SELECT body FROM {table} WHERE account=? AND peer=? AND id=?"),
                    params![owner, peer, id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(mut message) = body.and_then(|body| Message::decode(&body).ok()) else {
                continue;
            };
            if message.inline_emoji.is_some()
                || message.unavailable
                || message.retracted
                || message.mode() == crate::encryption_policy::Mode::Legacy
                || message.text != validated.body
            {
                continue;
            }
            message.inline_emoji = Some(validated.clone());
            transaction.execute(
                &format!("UPDATE {table} SET body=? WHERE account=? AND peer=? AND id=?"),
                params![
                    serde_json::to_string(&message).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    owner,
                    peer,
                    id
                ],
            )?;
            changed = true;
        }
        transaction.commit()?;
        Ok(changed)
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
            .map(|target| format!("{} · {}", target.author, target.reply_preview()))
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
        let Some(id) = self.own_wire_id(account, peer, wire)? else {
            return Ok(None);
        };
        let Some(mut message) = self
            .message(account, peer, &id)
            .filter(|m| m.own && m.wire_id == wire && !m.retracted)
        else {
            return Ok(None);
        };
        if message.delivery != "read" && (message.delivery != "delivered" || status == "read") {
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
        if let Ok(requests) = self.pending_requests(a) {
            s.requests = requests
                .contacts
                .into_iter()
                .map(|request| (request.peer, request.description))
                .collect();
        }
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
                s.folders = f;
                let mut seen = std::collections::HashSet::new();
                s.folder_order = o
                    .into_iter()
                    .filter(|name| {
                        (name == "All" || s.folders.contains_key(name)) && seen.insert(name.clone())
                    })
                    .collect();
                if !s.folder_order.iter().any(|name| name == "All") {
                    s.folder_order.push("All".into());
                }
                s.all_folder_hidden = data["all_hidden"].as_bool().unwrap_or(false);
                if s.visible_folder_names().is_empty() {
                    s.all_folder_hidden = false;
                }
                s.folder = s.visible_folder_names()[0].clone();
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
        let mut ids = vec![id.to_owned()];
        {
            let mut query = tx.prepare("SELECT pending.id FROM upload_captions pending JOIN messages caption ON caption.account=pending.account AND caption.peer=pending.peer AND caption.id=pending.id WHERE pending.account=?1 AND pending.peer=?2 AND pending.parent=?3 AND json_extract(caption.body,'$.upload_caption_for')=?3 AND json_extract(caption.body,'$.delivery')='waiting-upload' AND json_extract(caption.body,'$.own')=1 AND json_extract(caption.body,'$.encrypted')=1 AND json_extract(caption.body,'$.encryption_mode')='Legacy'")?;
            ids.extend(
                query
                    .query_map(params![a, peer, id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        for id in ids {
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
        }
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
        self.set_pref(&format!("lan-sync-excluded:{a}"),"true")?;
        crate::lan_sync::exclude(a);
        let tx = self.db.unchecked_transaction()?;
        tx.execute("DELETE FROM accounts WHERE jid=?", [a])?;
        tx.execute("DELETE FROM aliases WHERE account=?", [a])?;
        tx.execute(
            "DELETE FROM preferences WHERE key=?",
            [format!("folders:{a}")],
        )?;
        for key in [
            format!("link-previews-enabled:{a}"),
            format!("link-previews-encrypted:{a}"),
            format!("blocked-jids:{a}"),
        ] {
            tx.execute("DELETE FROM preferences WHERE key=?", [key])?;
        }
        let trust_prefix = format!("blind-trust:{a}\n");
        tx.execute(
            "DELETE FROM preferences WHERE substr(key,1,length(?1))=?1",
            [trust_prefix],
        )?;
        let reactions_prefix = format!("plain-reactions:{a}\n");
        self.db.execute(
            "DELETE FROM preferences WHERE substr(key,1,length(?1))=?1",
            [reactions_prefix],
        )?;
        tx.commit()
    }
}
fn upload_caption_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Message)> {
    let body: String = row.get(1)?;
    let message = Message::decode(&body).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok((row.get(0)?, message))
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
    fn folder_visibility_reload_is_account_scoped_and_legacy_safe() {
        let store = Store::open(true).unwrap();
        for owner in ["first@fixture.invalid", "second@fixture.invalid"] {
            store
                .save_account(&Account {
                    jid: owner.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
        }
        let mut first = State::live("first@fixture.invalid", vec![]);
        first.folder_order = vec!["Groups".into(), "All".into(), "Unread".into()];
        first.all_folder_hidden = true;
        store.save(&first).unwrap();
        let second = State::live("second@fixture.invalid", vec![]);
        store.save(&second).unwrap();
        let loaded = store.load("first@fixture.invalid");
        assert!(loaded.all_folder_hidden);
        assert_eq!(loaded.folder_order, first.folder_order);
        assert_eq!(loaded.folder, "Groups");
        assert_eq!(loaded.visible_folder_names(), ["Groups", "Unread"]);
        assert!(!store.load("second@fixture.invalid").all_folder_hidden);
        // Old configs lack visibility; unknown/deleted names cannot become active.
        let legacy = serde_json::json!({"rules":first.folders,"order":["missing","All","Groups"]});
        store
            .set_pref("folders:first@fixture.invalid", &legacy.to_string())
            .unwrap();
        let loaded = store.load("first@fixture.invalid");
        assert!(!loaded.all_folder_hidden);
        assert_eq!(loaded.folder, "All");
        assert_eq!(loaded.folder_order, ["All", "Groups"]);
        let empty = serde_json::json!({"rules":{},"order":["All"],"all_hidden":true});
        store
            .set_pref("folders:first@fixture.invalid", &empty.to_string())
            .unwrap();
        let loaded = store.load("first@fixture.invalid");
        assert!(!loaded.all_folder_hidden);
        assert_eq!(loaded.visible_folder_names(), ["All"]);
        assert_eq!(loaded.folder, "All");
    }
    #[test]
    fn automatic_trust_is_account_and_conversation_scoped() {
        let store = Store::open(true).unwrap();
        let (owner, peer, room) = (
            "me@example.org",
            "member@example.org",
            "room@conference.example.org",
        );
        assert!(!store.blind_trust_for(owner, peer, Some(room)).unwrap());
        store.set_blind_trust(owner, room, true).unwrap();
        assert!(store.blind_trust_for(owner, peer, Some(room)).unwrap());
        assert!(store.blind_trust_for(owner, owner, Some(room)).unwrap());
        assert!(!store.blind_trust_for(owner, peer, None).unwrap());
        assert!(
            !store
                .blind_trust_for(owner, peer, Some("other@conference.example.org"))
                .unwrap()
        );
        assert!(
            !store
                .blind_trust_for("other@example.org", peer, Some(room))
                .unwrap()
        );
        store.set_blind_trust(owner, room, false).unwrap();
        assert!(!store.blind_trust_for(owner, owner, Some(room)).unwrap());
        store.set_blind_trust(owner, peer, true).unwrap();
        assert!(store.blind_trust_for(owner, owner, Some(peer)).unwrap());
        assert!(store.blind_trust_for(owner, peer, Some(room)).unwrap());
    }
    #[test]
    fn upload_caption_claim_waits_for_scoped_parent_and_is_durable_once() {
        let store = Store::open(true).unwrap();
        let owner = "own@example.org";
        let other = "other@example.org";
        let peer = "peer@example.org";
        let foreign = "foreign@example.org";
        let chat = |jid: &str| Chat {
            jid: jid.into(),
            ..Default::default()
        };
        for account in [owner, other] {
            store
                .save_account(&Account {
                    jid: account.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
            for peer in [peer, foreign] {
                store.save_chat_message(account, &chat(peer), None).unwrap();
            }
        }
        let mut parent = Message {
            id: "upload".into(),
            wire_id: "upload".into(),
            own: true,
            delivery: "sent".into(),
            attachment: crate::file_share::Attachment::bare("https://files.example.org/file"),
            ..Default::default()
        };
        parent.set_encryption(crate::encryption_policy::Mode::Legacy, String::new());
        store.upsert_message(other, peer, &parent).unwrap();
        store.upsert_message(owner, foreign, &parent).unwrap();
        let mut caption = Message {
            id: "caption".into(),
            wire_id: "caption".into(),
            own: true,
            delivery: "waiting-upload".into(),
            upload_caption_for: parent.wire_id.clone(),
            text: "Companion caption".into(),
            ..Default::default()
        };
        caption.set_encryption(crate::encryption_policy::Mode::Legacy, String::new());
        store.upsert_message(owner, peer, &caption).unwrap();
        for index in 0..70 {
            let mut blocked = caption.clone();
            blocked.id = format!("blocked-{index}");
            blocked.wire_id = blocked.id.clone();
            blocked.upload_caption_for = format!("missing-parent-{index}");
            blocked.timestamp = "2000-01-01T00:00:00Z".into();
            store.upsert_message(owner, peer, &blocked).unwrap();
        }
        assert!(store.ready_upload_captions(owner).unwrap().is_empty());
        assert!(
            store
                .claim_upload_caption(owner, peer, &caption.id)
                .unwrap()
                .is_none()
        );
        parent.delivery = "sending".into();
        store.upsert_message(owner, peer, &parent).unwrap();
        assert!(store.ready_upload_captions(owner).unwrap().is_empty());
        parent.delivery = "sent".into();
        store.upsert_message(owner, peer, &parent).unwrap();
        let ready = store.ready_upload_captions(owner).unwrap();
        assert_eq!(
            ready.len(),
            1,
            "blocked earlier rows cannot starve a ready caption"
        );
        assert_eq!(ready[0].0, peer);
        assert!(
            store
                .claim_upload_caption(other, peer, &caption.id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_upload_caption(owner, foreign, &caption.id)
                .unwrap()
                .is_none()
        );
        let claimed = store
            .claim_upload_caption(owner, peer, &caption.id)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.delivery, "sending");
        assert_eq!(claimed.upload_caption_for, parent.wire_id);
        assert_eq!(
            store
                .action_base(owner, peer, &caption.id)
                .unwrap()
                .delivery,
            "sending"
        );
        assert!(
            store
                .claim_upload_caption(owner, peer, &caption.id)
                .unwrap()
                .is_none()
        );
        store.interrupt_messages(owner).unwrap();
        assert_eq!(
            store.message(owner, peer, &caption.id).unwrap().delivery,
            "uncertain"
        );
        assert!(store.ready_upload_captions(owner).unwrap().is_empty());
        for index in 0..70 {
            let mut next = caption.clone();
            next.id = format!("ready-{index}");
            next.wire_id = next.id.clone();
            store.upsert_message(owner, peer, &next).unwrap();
        }
        assert_eq!(store.ready_upload_captions(owner).unwrap().len(), 64);
        let mut plan = store.db.prepare(&format!("EXPLAIN QUERY PLAN {READY_UPLOAD_CAPTIONS} ORDER BY caption.timestamp,caption.id LIMIT 64")).unwrap();
        let steps: Vec<String> = plan
            .query_map([owner], |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for table in ["dependency", "caption", "parent"] {
            assert!(
                steps
                    .iter()
                    .any(|step| step.starts_with(&format!("SEARCH {table} "))),
                "pending captions and their parents must use scoped indexed lookups: {steps:?}"
            );
        }
        drop(plan);
        parent.delivery = "failed".into();
        store.upsert_message(owner, peer, &parent).unwrap();
        assert!(
            store
                .claim_upload_caption(owner, peer, "ready-0")
                .unwrap()
                .is_none(),
            "claim rechecks parent state after enumeration"
        );
        store.delete(owner, peer, &parent.id).unwrap();
        assert!(store.message(owner, peer, "ready-0").is_none());
        assert!(store.action_base(owner, peer, "ready-0").is_err());
        assert!(
            store.message(owner, peer, &caption.id).is_some(),
            "already claimed captions survive parent deletion"
        );
        assert!(store.message(other, peer, &parent.id).is_some());
        assert!(store.message(owner, foreign, &parent.id).is_some());
    }
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
