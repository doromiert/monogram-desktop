//! Private, portable contact overrides. These never update roster/vCard data.
use crate::storage::Store;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct LocalContact {
    pub peer: String,
    pub nickname: String,
    pub note: String,
    pub avatar: Option<PathBuf>,
    pub revision: u64,
}
fn invalid() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}
pub fn id(peer: &str) -> String {
    crate::crypto::lifecycle::hex(&openssl::sha::sha256(peer.as_bytes()))
}
fn valid_peer(owner: &str, peer: &str) -> bool {
    owner != peer
        && crate::protocol::bare(owner).ok().as_deref() == Some(owner)
        && crate::protocol::bare(peer).ok().as_deref() == Some(peer)
        && peer
            .parse::<tokio_xmpp::jid::Jid>()
            .ok()
            .is_some_and(|j| j.node().is_some())
}
fn valid_text(name: &str, note: &str) -> bool {
    name.chars().count() <= 128
        && name.encode_utf16().count() <= 256
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        && note.encode_utf16().count() <= 8192
        && !note.contains('\0')
}
pub fn valid_png(bytes: &[u8]) -> bool {
    if bytes.len() < 33
        || bytes.len() > 1024 * 1024
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || &bytes[12..16] != b"IHDR"
    {
        return false;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    width > 0
        && height > 0
        && width <= 4096
        && height <= 4096
        && u64::from(width) * u64::from(height) <= 16 * 1024 * 1024
}
fn cache_avatar(owner: &str, bytes: &[u8]) -> rusqlite::Result<PathBuf> {
    if !valid_png(bytes) {
        return Err(invalid());
    }
    let root = crate::crypto::lifecycle::root()
        .join("contact-overrides")
        .join(id(owner));
    std::fs::create_dir_all(&root).map_err(|_| invalid())?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| invalid())?;
    let path = root.join(format!(
        "{}.png",
        crate::crypto::lifecycle::hex(&openssl::sha::sha256(bytes))
    ));
    if !path.is_file() {
        let temporary = root.join(format!(".avatar-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            out.write_all(bytes)?;
            out.sync_all()?;
            std::fs::rename(&temporary, &path)
        })();
        let _ = std::fs::remove_file(temporary);
        result.map_err(|_| invalid())?;
    }
    Ok(path)
}
impl Store {
    pub(crate) fn init_contact_details(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS contact_details(account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,peer TEXT NOT NULL,nickname TEXT NOT NULL,note TEXT NOT NULL,avatar TEXT,revision INTEGER NOT NULL,PRIMARY KEY(account,peer));")
    }
    pub fn contact_details(&self, owner: &str, peer: &str) -> rusqlite::Result<LocalContact> {
        self.db.query_row("SELECT nickname,note,avatar,revision FROM contact_details WHERE account=? AND peer=?",params![owner,peer],|r|Ok(LocalContact{peer:peer.into(),nickname:r.get(0)?,note:r.get(1)?,avatar:r.get::<_,Option<String>>(2)?.map(PathBuf::from),revision:r.get::<_,i64>(3)?.max(0) as u64})).optional().map(|value|value.unwrap_or_else(||LocalContact{peer:peer.into(),..Default::default()}))
    }
    pub fn load_all_contact_overrides(
        &self,
        owner: &str,
    ) -> rusqlite::Result<HashMap<String, LocalContact>> {
        let mut query = self.db.prepare(
            "SELECT peer,nickname,note,avatar,revision FROM contact_details WHERE account=?",
        )?;
        query
            .query_map([owner], |r| {
                let peer: String = r.get(0)?;
                Ok((
                    peer.clone(),
                    LocalContact {
                        peer,
                        nickname: r.get(1)?,
                        note: r.get(2)?,
                        avatar: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                        revision: r.get::<_, i64>(4)?.max(0) as u64,
                    },
                ))
            })?
            .collect()
    }
    /// Worker-only: None clears the override; pass the loaded path to retain it.
    pub fn set_contact_details(
        &self,
        owner: &str,
        peer: &str,
        nickname: &str,
        note: &str,
        avatar: Option<&Path>,
    ) -> rusqlite::Result<()> {
        if !valid_peer(owner, peer) || !valid_text(nickname.trim(), note) {
            return Err(invalid());
        }
        let avatar = avatar
            .map(|source| {
                let mut bytes = vec![];
                std::fs::File::open(source)
                    .map_err(|_| invalid())?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| invalid())?;
                cache_avatar(owner, &bytes)
            })
            .transpose()?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let old = self.contact_details(owner, peer)?;
        let revision = old
            .revision
            .saturating_add(1)
            .max(chrono::Utc::now().timestamp_millis().max(1) as u64);
        tx.execute("INSERT INTO contact_details VALUES(?,?,?,?,?,?) ON CONFLICT(account,peer) DO UPDATE SET nickname=excluded.nickname,note=excluded.note,avatar=excluded.avatar,revision=excluded.revision",params![owner,peer,nickname.trim(),note,avatar.as_ref().map(|p|p.to_string_lossy().to_string()),revision as i64])?;
        tx.commit()
    }
    pub(crate) fn contact_sync_value(
        &self,
        owner: &str,
        peer: &str,
    ) -> rusqlite::Result<(Value, u64)> {
        let value = self.contact_details(owner, peer)?;
        let avatar = value
            .avatar
            .map(|p| {
                std::fs::read(p).map_err(|_| invalid()).and_then(|bytes| {
                    if valid_png(&bytes) {
                        Ok(B64.encode(bytes))
                    } else {
                        Err(invalid())
                    }
                })
            })
            .transpose()?;
        Ok((
            json!({"peer":peer,"nickname":value.nickname,"note":value.note,"avatar":avatar}),
            value.revision,
        ))
    }
    pub(crate) fn apply_contact_sync(
        &self,
        owner: &str,
        value: &Value,
        stamp: u64,
    ) -> rusqlite::Result<bool> {
        let peer = value["peer"].as_str().ok_or_else(invalid)?;
        let nickname = value["nickname"].as_str().ok_or_else(invalid)?;
        let note = value["note"].as_str().ok_or_else(invalid)?;
        if !valid_peer(owner, peer)
            || !valid_text(nickname, note)
            || stamp == 0
            || stamp > i64::MAX as u64
        {
            return Err(invalid());
        }
        let avatar = if value["avatar"].is_null() {
            None
        } else {
            let bytes = B64
                .decode(value["avatar"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            Some(cache_avatar(owner, &bytes)?)
        };
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if self.contact_details(owner, peer)?.revision > stamp {
            return Ok(false);
        }
        tx.execute("INSERT INTO contact_details VALUES(?,?,?,?,?,?) ON CONFLICT(account,peer) DO UPDATE SET nickname=excluded.nickname,note=excluded.note,avatar=excluded.avatar,revision=excluded.revision",params![owner,peer,nickname,note,avatar.as_ref().map(|p|p.to_string_lossy().to_string()),stamp as i64])?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overrides_are_private_durable_revisioned_and_clearable() {
        let store = Store::open(true).unwrap();
        let owner = "alice@example.invalid";
        let peer = "bob@example.invalid";
        let other = "other@example.invalid";
        for jid in [owner, other] {
            store
                .save_account(&crate::model::Account {
                    jid: jid.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
        }
        store
            .set_contact_details(owner, peer, "Local friend", "A private note", None)
            .unwrap();
        let first = store.contact_details(owner, peer).unwrap();
        assert_eq!(first.nickname, "Local friend");
        assert!(first.revision > 0);
        assert!(
            store
                .contact_details(other, peer)
                .unwrap()
                .nickname
                .is_empty()
        );
        let stale = json!({"peer":peer,"nickname":"Old remote","note":"","avatar":null});
        assert!(
            !store
                .apply_contact_sync(owner, &stale, first.revision - 1)
                .unwrap()
        );
        let newer = first.revision + 10;
        assert!(store.apply_contact_sync(owner, &stale, newer).unwrap());
        assert_eq!(store.contact_sync_value(owner, peer).unwrap().1, newer);
        store
            .set_contact_details(owner, peer, "", "", None)
            .unwrap();
        assert!(store.contact_details(owner, peer).unwrap().revision > newer);
        assert_eq!(store.load_all_contact_overrides(owner).unwrap().len(), 1);
        assert!(
            store
                .set_contact_details(owner, peer, "bad\nname", "", None)
                .is_err()
        );
        assert!(
            store
                .set_contact_details(owner, owner, "self", "", None)
                .is_err()
        );
    }
}
