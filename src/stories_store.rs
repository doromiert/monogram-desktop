//! Account-scoped story metadata and local read state. Media remains in its own cache.
use crate::{
    storage::Store,
    stories::{Entry, LIFETIME},
};
use rusqlite::{OptionalExtension, params};
impl Store {
    pub(crate) fn init_stories(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS stories(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,owner TEXT NOT NULL,id TEXT NOT NULL,published INTEGER NOT NULL,body TEXT NOT NULL,seen INTEGER NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(account,owner,id));CREATE INDEX IF NOT EXISTS story_expiry ON stories(account,published);")?;
        let mut info = self.db.prepare("PRAGMA table_info(stories)")?;
        let active = info
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "active");
        if !active {
            self.db.execute_batch(
                "ALTER TABLE stories ADD COLUMN active INTEGER NOT NULL DEFAULT 1",
            )?;
        }
        Ok(())
    }
    pub fn stories_cache(&self, account: &str, now: i64) -> rusqlite::Result<Vec<Entry>> {
        let mut query=self.db.prepare("SELECT owner,body FROM stories WHERE account=? AND active=1 AND published>? ORDER BY published,id LIMIT 5000")?;
        let rows = query.query_map(params![account, now.saturating_sub(LIFETIME)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|(owner, body)| {
                serde_json::from_str::<Entry>(&body)
                    .ok()
                    .filter(|e| e.owner == owner && e.validate(now).is_ok() && !e.expired(now))
            })
            .collect())
    }
    pub fn stories_history(&self, account: &str) -> rusqlite::Result<Vec<Entry>> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut query=self.db.prepare("SELECT body FROM stories WHERE account=? AND owner=? ORDER BY published DESC,id DESC LIMIT 1000")?;
        let rows = query.query_map(params![account, account], |r| r.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|body| {
                serde_json::from_str::<Entry>(&body)
                    .ok()
                    .filter(|e| e.owner == account && e.validate(now).is_ok())
            })
            .collect())
    }
    pub fn stories_replace(
        &self,
        account: &str,
        peer: &str,
        entries: &[Entry],
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut ids = std::collections::BTreeSet::new();
        if entries.len() > 100
            || entries.iter().any(|e| {
                e.owner != peer || e.validate(now).is_err() || e.expired(now) || !ids.insert(&e.id)
            })
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.db.unchecked_transaction()?;
        // Keep own expired posts as local history. Missing active items were retracted.
        let mut old=tx.prepare("SELECT id FROM stories WHERE account=? AND owner=? AND (owner<>account OR published>?)")?;
        let removed = old
            .query_map(params![account, peer, now.saturating_sub(LIFETIME)], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(old);
        for id in removed {
            if !ids.contains(&id) {
                if peer == account {
                    tx.execute(
                        "UPDATE stories SET active=0 WHERE account=? AND owner=? AND id=?",
                        params![account, peer, id],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM stories WHERE account=? AND owner=? AND id=?",
                        params![account, peer, id],
                    )?;
                }
            }
        }
        for entry in entries {
            tx.execute("INSERT INTO stories(account,owner,id,published,body) VALUES (?,?,?,?,?) ON CONFLICT(account,owner,id) DO UPDATE SET published=excluded.published,body=excluded.body,active=1",params![account,peer,entry.id,entry.published,serde_json::to_string(entry).map_err(|_|rusqlite::Error::InvalidQuery)?])?;
        }
        tx.execute(
            "DELETE FROM stories WHERE account=? AND owner<>account AND published<=?",
            params![account, now.saturating_sub(LIFETIME)],
        )?;
        tx.execute("DELETE FROM stories WHERE account=? AND owner=account AND id NOT IN(SELECT id FROM stories WHERE account=? AND owner=account ORDER BY published DESC,id DESC LIMIT 1000)",params![account,account])?;
        tx.commit()
    }
    pub fn stories_seen(&self, account: &str, peer: &str, id: &str) -> rusqlite::Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT seen<>0 FROM stories WHERE account=? AND owner=? AND id=?",
                params![account, peer, id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }
    pub fn stories_mark_seen(&self, account: &str, peer: &str, id: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE stories SET seen=1 WHERE account=? AND owner=? AND id=?",
            params![account, peer, id],
        )?;
        Ok(())
    }
    pub fn stories_remove(&self, account: &str, peer: &str, id: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "DELETE FROM stories WHERE account=? AND owner=? AND id=?",
            params![account, peer, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_is_account_scoped_expiring_and_preserves_seen_and_own_history() {
        let store = Store::open(true).unwrap();
        let own = "alice@example.invalid";
        let other = "other@example.invalid";
        let peer = "bob@example.invalid";
        let now = chrono::Utc::now().timestamp_millis();
        for jid in [own, other] {
            store
                .save_account(&crate::model::Account {
                    jid: jid.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
        }
        let make = |owner: &str, id: &str, published| Entry {
            id: id.into(),
            owner: owner.into(),
            url: "https://example.invalid/story.jpg".into(),
            mime: "image/jpeg".into(),
            caption: "Caption".into(),
            published,
        };
        let entry = make(peer, "one", now);
        store
            .stories_replace(own, peer, &[entry.clone()], now)
            .unwrap();
        store.stories_mark_seen(own, peer, "one").unwrap();
        store.stories_replace(own, peer, &[entry], now).unwrap();
        assert!(store.stories_seen(own, peer, "one").unwrap());
        assert!(store.stories_cache(other, now).unwrap().is_empty());
        store
            .stories_replace(own, own, &[make(own, "own", now)], now)
            .unwrap();
        store
            .stories_replace(own, own, &[], now + LIFETIME + 1)
            .unwrap();
        assert!(
            store
                .stories_cache(own, now + LIFETIME + 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.stories_history(own).unwrap().len(), 1);
        store.stories_remove(own, own, "own").unwrap();
        assert!(store.stories_history(own).unwrap().is_empty());
        assert!(
            store
                .stories_replace(own, peer, &[make(other, "forged", now)], now)
                .is_err()
        );
    }
}
