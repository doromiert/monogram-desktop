//! Public room choices are durable; passwords belong only to Secret Service.
use super::*;
use crate::rooms::{Bookmark, BookmarkInfo, LocalBookmark};

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalBookmark> {
    Ok(LocalBookmark {
        info: BookmarkInfo {
            jid: row.get(0)?,
            name: row.get(1)?,
            nick: row.get(2)?,
            autojoin: row.get(3)?,
        },
        revision: row.get::<_, i64>(4)? as u64,
        dirty: row.get(5)?,
    })
}
impl Store {
    pub(super) fn init_rooms(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS room_choices(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,name TEXT NOT NULL,nick TEXT,autojoin INTEGER NOT NULL,revision INTEGER NOT NULL,dirty INTEGER NOT NULL,error TEXT,PRIMARY KEY(account,peer));CREATE INDEX IF NOT EXISTS dirty_room_choices ON room_choices(account,dirty,peer);")
    }
    pub fn room_choices(&self, owner: &str) -> rusqlite::Result<Vec<LocalBookmark>> {
        let mut query = self.db.prepare("SELECT peer,name,nick,autojoin,revision,dirty FROM room_choices WHERE account=? ORDER BY peer LIMIT 1024")?;
        query.query_map([owner], row)?.collect()
    }
    pub fn dirty_room_choices(&self, owner: &str) -> rusqlite::Result<Vec<LocalBookmark>> {
        let mut query = self.db.prepare("SELECT peer,name,nick,autojoin,revision,dirty FROM room_choices WHERE account=? AND dirty=1 ORDER BY peer LIMIT 1024")?;
        query.query_map([owner], row)?.collect()
    }
    pub fn save_room_choice(
        &self,
        owner: &str,
        info: &BookmarkInfo,
    ) -> rusqlite::Result<LocalBookmark> {
        Bookmark::new(info.clone(), None)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let tx = self.db.unchecked_transaction()?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM room_choices WHERE account=? AND peer=?",
                params![owner, info.jid],
                |row| row.get(0),
            )
            .optional()?;
        if revision.is_none()
            && tx.query_row(
                "SELECT COUNT(*) FROM room_choices WHERE account=?",
                [owner],
                |row| row.get::<_, i64>(0),
            )? >= 1024
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let revision = revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        tx.execute("INSERT INTO room_choices VALUES(?,?,?,?,?,?,1,NULL) ON CONFLICT(account,peer) DO UPDATE SET name=excluded.name,nick=excluded.nick,autojoin=excluded.autojoin,revision=excluded.revision,dirty=1,error=NULL",params![owner,info.jid,info.name,info.nick,info.autojoin,revision])?;
        tx.commit()?;
        Ok(LocalBookmark {
            info: info.clone(),
            revision: revision as u64,
            dirty: true,
        })
    }
    pub fn acknowledge_room_choice(
        &self,
        owner: &str,
        peer: &str,
        revision: u64,
        error: Option<&str>,
    ) -> rusqlite::Result<bool> {
        Ok(self.db.execute(
            "UPDATE room_choices SET dirty=?,error=? WHERE account=? AND peer=? AND revision=?",
            params![error.is_some(), error, owner, peer, revision as i64],
        )? != 0)
    }
    pub fn room_choice_error(&self, owner: &str, peer: &str) -> Option<String> {
        self.db
            .query_row(
                "SELECT error FROM room_choices WHERE account=? AND peer=?",
                params![owner, peer],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }
    pub fn reconcile_room_choices(
        &self,
        owner: &str,
        server: &[BookmarkInfo],
    ) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let clean: Vec<_> = self
            .room_choices(owner)?
            .into_iter()
            .filter(|choice| !choice.dirty)
            .collect();
        for choice in clean {
            let current = server.iter().find(|entry| entry.jid == choice.info.jid);
            if let Some(current) = current {
                tx.execute("UPDATE room_choices SET name=?,nick=?,autojoin=? WHERE account=? AND peer=? AND dirty=0",params![current.name,current.nick,current.autojoin,owner,current.jid])?;
            } else {
                tx.execute(
                    "UPDATE room_choices SET autojoin=0 WHERE account=? AND peer=? AND dirty=0",
                    params![owner, choice.info.jid],
                )?;
            }
        }
        for info in server {
            tx.execute("INSERT OR IGNORE INTO room_choices SELECT ?,?,?,?,?,0,0,NULL WHERE (SELECT COUNT(*) FROM room_choices WHERE account=?)<1024",params![owner,info.jid,info.name,info.nick,info.autojoin,owner])?;
        }
        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_choices_keep_newer_edits_and_missing_clean_bookmarks_stop_autojoin() {
        let store = Store::open(true).unwrap();
        let owner = "own@example.org";
        store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut info = BookmarkInfo {
            jid: "room@conference.example.org".into(),
            name: "Group".into(),
            nick: Some("own".into()),
            autojoin: true,
        };
        let first = store.save_room_choice(owner, &info).unwrap();
        info.autojoin = false;
        let second = store.save_room_choice(owner, &info).unwrap();
        assert!(
            !store
                .acknowledge_room_choice(owner, &info.jid, first.revision, None)
                .unwrap()
        );
        assert_eq!(
            store.dirty_room_choices(owner).unwrap()[0].revision,
            second.revision
        );
        assert!(
            store
                .acknowledge_room_choice(owner, &info.jid, second.revision, None)
                .unwrap()
        );
        info.autojoin = true;
        let third = store.save_room_choice(owner, &info).unwrap();
        store.reconcile_room_choices(owner, &[]).unwrap();
        assert!(
            store.room_choices(owner).unwrap()[0].info.autojoin,
            "pending local choice wins"
        );
        store
            .acknowledge_room_choice(owner, &info.jid, third.revision, None)
            .unwrap();
        store.reconcile_room_choices(owner, &[]).unwrap();
        assert!(!store.room_choices(owner).unwrap()[0].info.autojoin);
        assert!(store.room_choices("other@example.org").unwrap().is_empty());
        store.forget(owner).unwrap();
        assert!(store.room_choices(owner).unwrap().is_empty());
    }
}
