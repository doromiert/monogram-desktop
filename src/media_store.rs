//! Local decoded-media observations never rewrite the immutable received SFS payload.
use crate::{file_share::Attachment, model::Message, storage::Store};
use rusqlite::{OptionalExtension, params};
const FILTER: &str = "m.account=?1 AND m.peer=?2 AND COALESCE(json_extract(m.body,'$.retracted'),0)=0 AND json_type(m.body,'$.attachment')='object' AND (json_extract(m.body,'$.attachment.mime') LIKE 'image/%' OR json_extract(m.body,'$.attachment.mime') LIKE 'video/%' OR lower(json_extract(m.body,'$.attachment.name')) LIKE '%.mp4' OR lower(json_extract(m.body,'$.attachment.name')) LIKE '%.webm' OR lower(json_extract(m.body,'$.attachment.name')) LIKE '%.mov' OR lower(json_extract(m.body,'$.attachment.name')) LIKE '%.mkv' OR EXISTS(SELECT 1 FROM image_metadata i WHERE i.account=m.account AND i.peer=m.peer AND i.id=m.id))";
#[derive(Default)]
pub struct MediaWindow {
    pub messages: Vec<Message>,
    pub index: usize,
}
pub struct MediaPage {
    pub messages: Vec<Message>,
    pub next: Option<(String, String)>,
}
impl Store {
    pub async fn read_snapshot<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let Some(path) = self
            .db
            .path()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
        else {
            return read(self);
        };
        gtk::gio::spawn_blocking(move || {
            let db = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            db.busy_timeout(std::time::Duration::from_millis(250))?;
            let reader = Store { db };
            let snapshot = reader.db.unchecked_transaction()?;
            let result = read(&reader)?;
            snapshot.commit()?;
            Ok(result)
        })
        .await
        .unwrap_or(Err(rusqlite::Error::InvalidQuery))
    }
    pub fn init_media(&self) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS image_metadata(account TEXT NOT NULL,peer TEXT NOT NULL,id TEXT NOT NULL,file TEXT NOT NULL,mime TEXT NOT NULL,width INTEGER NOT NULL,height INTEGER NOT NULL,animated INTEGER NOT NULL,PRIMARY KEY(account,peer,id),FOREIGN KEY(account,peer,id) REFERENCES messages(account,peer,id) ON DELETE CASCADE);
            DROP TRIGGER IF EXISTS image_metadata_invalidated;
            CREATE TRIGGER image_metadata_invalidated AFTER UPDATE OF body ON messages
            WHEN EXISTS(SELECT 1 FROM image_metadata i WHERE i.account=OLD.account AND i.peer=OLD.peer AND i.id=OLD.id)
                AND (COALESCE(json_extract(NEW.body,'$.retracted'),0)=1
                    OR json_extract(NEW.body,'$.attachment.wire_url') IS NOT json_extract(OLD.body,'$.attachment.wire_url')
                    OR json_extract(NEW.body,'$.attachment.size') IS NOT json_extract(OLD.body,'$.attachment.size')
                    OR json_extract(NEW.body,'$.attachment.hashes') IS NOT json_extract(OLD.body,'$.attachment.hashes'))
            BEGIN DELETE FROM image_metadata WHERE account=OLD.account AND peer=OLD.peer AND id=OLD.id; END;")?;
        tx.commit()
    }
    pub fn media_dimensions(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        file: &Attachment,
    ) -> Option<(u32, u32)> {
        self.db.query_row(
            "SELECT width,height FROM image_metadata WHERE account=? AND peer=? AND id=? AND file=?",
            params![owner, peer, id, file.key()], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
        ).ok().or_else(|| file.width.zip(file.height))
            .filter(|(width, height)| *width > 0 && *height > 0 && u64::from(*width) * u64::from(*height) <= 64_000_000)
    }
    pub fn remember_image(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        file: &Attachment,
        mime: &str,
        width: u32,
        height: u32,
        animated: bool,
    ) -> rusqlite::Result<bool> {
        let Some(message) = self.message(owner, peer, id).filter(|m| {
            !m.retracted
                && m.attachment
                    .as_ref()
                    .is_some_and(|current| current.key() == file.key())
        }) else {
            return Ok(false);
        };
        if message.upload.is_some()
            || !(mime.starts_with("image/") || mime.starts_with("video/"))
            || width == 0
            || height == 0
        {
            return Ok(false);
        }
        self.db.execute("INSERT INTO image_metadata VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(account,peer,id) DO UPDATE SET file=excluded.file,mime=excluded.mime,width=excluded.width,height=excluded.height,animated=excluded.animated WHERE image_metadata.file!=excluded.file OR image_metadata.mime!=excluded.mime OR image_metadata.width!=excluded.width OR image_metadata.height!=excluded.height OR image_metadata.animated!=excluded.animated",params![owner,peer,id,file.key(),mime,width,height,animated])?;
        Ok(true)
    }
    pub fn media_page(
        &self,
        owner: &str,
        peer: &str,
        before: Option<(&str, &str)>,
    ) -> rusqlite::Result<MediaPage> {
        const PAGE_SIZE: usize = 9;
        let (time, id) = before.unwrap_or(("~", "~"));
        let mut query = self.db.prepare(&format!(
            "SELECT m.timestamp,m.id,m.body FROM messages m WHERE {FILTER} AND (m.timestamp,m.id)<(?3,?4) ORDER BY m.timestamp DESC,m.id DESC LIMIT ?5"
        ))?;
        let mut rows = query
            .query_map(
                params![owner, peer, time, id, PAGE_SIZE as i64 + 1],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = if rows.len() > PAGE_SIZE {
            rows.pop();
            rows.last().map(|(time, id, _)| (time.clone(), id.clone()))
        } else {
            None
        };
        let messages = rows
            .into_iter()
            .filter_map(|(_, _, body)| Message::decode(&body).ok())
            .collect();
        Ok(MediaPage { messages, next })
    }
    pub fn media_window(&self, owner: &str, peer: &str, id: &str) -> rusqlite::Result<MediaWindow> {
        let anchor = self
            .db
            .query_row(
                &format!("SELECT m.timestamp,m.id FROM messages m WHERE {FILTER} AND m.id=?3"),
                params![owner, peer, id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((time, id)) = anchor else {
            return Ok(MediaWindow::default());
        };
        let side = |operator: &str, order: &str| -> rusqlite::Result<Vec<Message>> {
            let mut q=self.db.prepare(&format!("SELECT m.body FROM messages m WHERE {FILTER} AND (m.timestamp,m.id){operator}(?3,?4) ORDER BY m.timestamp {order},m.id {order} LIMIT 10"))?;
            Ok(
                q.query_map(params![owner, peer, time, id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .into_iter()
                    .filter_map(|s| Message::decode(&s).ok())
                    .collect(),
            )
        };
        let Some(selected) = self.message(owner, peer, &id) else {
            return Ok(MediaWindow::default());
        };
        let mut messages = side("<", "DESC")?;
        messages.reverse();
        let index = messages.len();
        messages.push(selected);
        messages.extend(side(">", "ASC")?);
        Ok(MediaWindow { messages, index })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    #[test]
    fn ten_neighbours_per_side_and_verified_observations() {
        let db = Store::open(true).unwrap();
        let owner = "alice@example.org";
        let peer = "bob@example.org";
        db.save_account(&Account {
            jid: owner.into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        })
        .unwrap();
        let mut state = State::live(owner, vec![]);
        let chat = state.chat_for(peer);
        for i in 0..23 {
            let mut file = Attachment::bare(&format!("https://fixture.invalid/photo/{i}")).unwrap();
            file.mime = "image/png".into();
            chat.messages.push(Message {
                id: format!("image-{i:03}"),
                kind: "file".into(),
                timestamp: format!("{i:06}"),
                attachment: Some(file),
                ..Default::default()
            });
        }
        db.save(&state).unwrap();
        let w = db.media_window(owner, peer, "image-011").unwrap();
        assert_eq!((w.messages.len(), w.index), (21, 10));
        assert_eq!(w.messages[0].id, "image-001");
        assert_eq!(w.messages[20].id, "image-021");
        let first = db.media_window(owner, peer, "image-000").unwrap();
        assert_eq!((first.messages.len(), first.index), (11, 0));
        let end = db.media_window(owner, peer, "image-022").unwrap();
        assert_eq!((end.messages.len(), end.index), (11, 10));
        assert!(
            db.media_window("other@example.org", peer, "image-011")
                .unwrap()
                .messages
                .is_empty()
        );
        let file = Attachment::bare("https://fixture.invalid/opaque").unwrap();
        state.chat_for(peer).messages.push(Message {
            id: "observed".into(),
            timestamp: "000025".into(),
            kind: "file".into(),
            attachment: Some(file.clone()),
            ..Default::default()
        });
        db.save(&state).unwrap();
        assert!(
            db.media_window(owner, peer, "observed")
                .unwrap()
                .messages
                .is_empty()
        );
        assert!(
            db.remember_image(owner, peer, "observed", &file, "image/png", 800, 600, false)
                .unwrap()
        );
        assert_eq!(db.media_window(owner, peer, "observed").unwrap().index, 10);
        assert_eq!(
            db.message(owner, peer, "observed")
                .unwrap()
                .attachment
                .unwrap()
                .mime,
            "application/octet-stream"
        );
        let observed = state.chat_for(peer).messages.last_mut().unwrap();
        observed.retracted = true;
        observed.attachment = None;
        db.save(&state).unwrap();
        assert!(
            db.media_window(owner, peer, "observed")
                .unwrap()
                .messages
                .is_empty()
        );
        assert_eq!(
            db.db
                .query_row("SELECT count(*) FROM image_metadata", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
