//! Authenticated encrypted records and atomic ratchet transactions.
use super::*;
use openssl::symm::{Cipher, decrypt_aead, encrypt_aead};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
pub const IDENTITY: i32 = 0;
pub const PRE_KEY: i32 = 1;
pub const SIGNED_PRE_KEY: i32 = 2;
pub const SESSION: i32 = 3;
pub const SESSION_IDENTITY: i32 = 4;
pub const TRUSTED: i32 = 5;
pub const META: i32 = 6;
pub const PRE_PUBLIC: i32 = 7;
pub const SIGNED_PUBLIC: i32 = 8;
pub const SEEN: i32 = 9;
pub const DEVICES: i32 = 10;
pub const REJECTED: i32 = 11;
pub const INCOMING: i32 = 12;
const MAX: usize = 2 * 1024 * 1024;
pub struct Store {
    db: Connection,
    key: MasterKey,
    pub owner: String,
    pub device: u32,
}
pub struct Tx<'a> {
    sql: Transaction<'a>,
    key: &'a [u8; 32],
    owner: &'a str,
    device: u32,
}
impl Store {
    pub fn open(path: Option<&Path>, owner: &str, device: u32, key: MasterKey) -> Result<Self> {
        address(owner, device)?;
        let db = if let Some(path) = path {
            let parent = path.parent().ok_or(Error::Invalid("database path"))?;
            fs::create_dir_all(parent).map_err(|_| Error::Invalid("private directory"))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error::Invalid("private directory"))?;
            let db = Connection::open(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|_| Error::Invalid("private database"))?;
            db
        } else {
            Connection::open_in_memory()?
        };
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS omemo2_scopes(owner TEXT NOT NULL,device INTEGER NOT NULL,initialized INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(owner,device)); CREATE TABLE IF NOT EXISTS omemo2_records(owner TEXT NOT NULL,device INTEGER NOT NULL,kind INTEGER NOT NULL,peer TEXT NOT NULL,id INTEGER NOT NULL,body BLOB NOT NULL,PRIMARY KEY(owner,device,kind,peer,id),FOREIGN KEY(owner,device) REFERENCES omemo2_scopes(owner,device) ON DELETE CASCADE);")?;
        db.execute(
            "INSERT OR IGNORE INTO omemo2_scopes(owner,device) VALUES(?,?)",
            params![owner, device],
        )?;
        Ok(Self {
            db,
            key,
            owner: owner.into(),
            device,
        })
    }
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let tx = Tx {
            sql: self.db.transaction()?,
            key: &self.key,
            owner: &self.owner,
            device: self.device,
        };
        let result = f(&tx)?;
        tx.sql.commit()?;
        Ok(result)
    }
}
impl Tx<'_> {
    fn aad(&self, kind: i32, peer: &str, id: u32) -> Vec<u8> {
        serde_json::to_vec(&(
            "monogram/omemo2/records/1",
            self.owner,
            self.device,
            kind,
            peer,
            id,
        ))
        .expect("record scope")
    }
    fn valid(&self, kind: i32, peer: &str) -> Result<()> {
        if !(IDENTITY..=INCOMING).contains(&kind)
            || peer.len() > 4096
            || (!peer.is_empty() && crate::protocol::bare(peer).ok().as_deref() != Some(peer))
        {
            return Err(Error::Invalid("record address"));
        }
        Ok(())
    }
    pub fn load(&self, kind: i32, peer: &str, id: u32) -> Result<Option<Secret>> {
        self.valid(kind, peer)?;
        let bytes:Option<Vec<u8>>=self.sql.query_row("SELECT body FROM omemo2_records WHERE owner=? AND device=? AND kind=? AND peer=? AND id=?",params![self.owner,self.device,kind,peer,id],|row|{
            let v=row.get_ref(0)?.as_blob()?;
            if v.len()>MAX+29 { return Err(rusqlite::Error::InvalidQuery); }
            Ok(v.to_vec())
        }).optional()?;
        let Some(bytes) = bytes else { return Ok(None) };
        if bytes.len() < 29 || bytes[0] != 1 {
            return Err(Error::Incomplete);
        }
        let end = bytes.len() - 16;
        let result = decrypt_aead(
            Cipher::aes_256_gcm(),
            self.key,
            Some(&bytes[1..13]),
            &self.aad(kind, peer, id),
            &bytes[13..end],
            &bytes[end..],
        )?;
        Ok(Some(Secret::new(result)))
    }
    pub fn save(&self, kind: i32, peer: &str, id: u32, value: Option<&[u8]>) -> Result<()> {
        self.valid(kind, peer)?;
        if value.is_some_and(|v| v.is_empty() || v.len() > MAX) {
            return Err(Error::Invalid("record size"));
        }
        if kind == TRUSTED {
            let old = self.load(kind, peer, id)?;
            if old.as_deref().map(|v| v.as_slice()) != value {
                self.sql.execute("DELETE FROM omemo2_records WHERE owner=? AND device=? AND peer=? AND id=? AND kind IN (?,?)",params![self.owner,self.device,peer,id,SESSION,SESSION_IDENTITY])?;
            }
        }
        if let Some(value) = value {
            let nonce = random::<12>()?;
            let mut tag = [0; 16];
            let ciphertext = encrypt_aead(
                Cipher::aes_256_gcm(),
                self.key,
                Some(&nonce),
                &self.aad(kind, peer, id),
                value,
                &mut tag,
            )?;
            let mut body = Vec::with_capacity(29 + ciphertext.len());
            body.push(1);
            body.extend_from_slice(&nonce);
            body.extend(ciphertext);
            body.extend(tag);
            self.sql.execute("INSERT INTO omemo2_records VALUES(?,?,?,?,?,?) ON CONFLICT(owner,device,kind,peer,id) DO UPDATE SET body=excluded.body",params![self.owner,self.device,kind,peer,id,body])?;
        } else {
            self.sql.execute("DELETE FROM omemo2_records WHERE owner=? AND device=? AND kind=? AND peer=? AND id=?",params![self.owner,self.device,kind,peer,id])?;
        }
        Ok(())
    }
    pub fn ids(&self, kind: i32) -> Result<Vec<u32>> {
        self.valid(kind, "")?;
        let mut stmt=self.sql.prepare("SELECT id FROM omemo2_records WHERE owner=? AND device=? AND kind=? AND peer='' ORDER BY id")?;
        Ok(stmt
            .query_map(params![self.owner, self.device, kind], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn initialized(&self) -> Result<bool> {
        Ok(self.sql.query_row(
            "SELECT initialized FROM omemo2_scopes WHERE owner=? AND device=?",
            params![self.owner, self.device],
            |r| r.get(0),
        )?)
    }
    pub fn mark_initialized(&self) -> Result<()> {
        self.sql.execute(
            "UPDATE omemo2_scopes SET initialized=1 WHERE owner=? AND device=?",
            params![self.owner, self.device],
        )?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_records_and_rollback() {
        let mut db = Store::open(None, "a@example.org", 101, MasterKey::new([1; 32])).unwrap();
        db.transaction(|t| t.save(IDENTITY, "", 0, Some(b"private")))
            .unwrap();
        let body: Vec<u8> = db
            .db
            .query_row("SELECT body FROM omemo2_records", [], |r| r.get(0))
            .unwrap();
        assert!(!body.windows(7).any(|v| v == b"private"));
        let failed: Result<()> = db.transaction(|t| {
            t.save(IDENTITY, "", 0, Some(b"changed"))?;
            Err(Error::Incomplete)
        });
        assert!(failed.is_err());
        db.transaction(|t| {
            assert_eq!(&**t.load(IDENTITY, "", 0)?.as_ref().unwrap(), b"private");
            Ok(())
        })
        .unwrap();
        db.db
            .execute("UPDATE omemo2_records SET kind=1", [])
            .unwrap();
        assert!(db.transaction(|t| t.load(PRE_KEY, "", 0)).is_err());
    }
    #[test]
    fn trust_changes_invalidate_sessions_only_when_needed() {
        let mut db = Store::open(None, "a@example.org", 101, MasterKey::new([1; 32])).unwrap();
        db.transaction(|t| {
            t.save(TRUSTED, "b@example.org", 202, Some(&[2; 32]))?;
            t.save(SESSION, "b@example.org", 202, Some(b"ratchet"))?;
            t.save(TRUSTED, "b@example.org", 202, Some(&[2; 32]))?;
            assert!(t.load(SESSION, "b@example.org", 202)?.is_some());
            t.save(TRUSTED, "b@example.org", 202, Some(&[3; 32]))?;
            assert!(t.load(SESSION, "b@example.org", 202)?.is_none());
            Ok(())
        })
        .unwrap();
    }
}
#[cfg(test)]
impl Store {
    pub fn fixture_fail_writes(&self, fail: bool) {
        if fail {
            self.db.execute_batch("CREATE TEMP TRIGGER fixture_insert BEFORE INSERT ON omemo2_records BEGIN SELECT RAISE(ABORT,'fixture persistence failure');END; CREATE TEMP TRIGGER fixture_update BEFORE UPDATE ON omemo2_records BEGIN SELECT RAISE(ABORT,'fixture persistence failure');END;").unwrap();
        } else {
            self.db
                .execute_batch("DROP TRIGGER fixture_insert; DROP TRIGGER fixture_update;")
                .unwrap();
        }
    }
}
