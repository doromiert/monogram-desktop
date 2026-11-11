// SPDX-License-Identifier: AGPL-3.0-only
//! Account/device-bound encrypted KV transactions, matching mobile PqOmemoRecords.
use super::*;
use crate::crypto::{MasterKey, random};
use openssl::symm::{Cipher, decrypt_aead, encrypt_aead};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use secret_service::{EncryptionType, SecretService};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
const MAX: usize = 2 * 1024 * 1024;
const APP: &str = "com.negzero.MonogramDesktop";
const MASTER: &str = "PQ OMEMO hybrid record encryption v1";
const BINDING: &str = "PQ OMEMO local hybrid identity binding v1";

#[derive(Serialize, Deserialize)]
struct Profile {
    version: u32,
    owner: String,
    device: u32,
    fingerprint: Option<String>,
}
pub struct Store {
    db: Connection,
    key: MasterKey,
    pub owner: String,
    pub device: u32,
    profile: Option<PathBuf>,
    pub(super) allow_initialize: bool,
}
pub struct Tx<'a> {
    sql: Transaction<'a>,
    key: &'a [u8; 32],
    pub owner: &'a str,
    pub device: u32,
}
fn io(_: std::io::Error) -> Error {
    Error::Unavailable("PQ encryption storage is unavailable; existing keys were preserved")
}
fn private(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io)?;
    Ok(())
}
fn profile_save(path: &Path, profile: &Profile) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(io)?;
        let bytes = serde_json::to_vec(profile).map_err(|_| Error::Incomplete)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(io)?;
        fs::rename(&tmp, path).map_err(io)?;
        File::open(path.parent().ok_or(Error::Incomplete)?)
            .and_then(|dir| dir.sync_all())
            .map_err(io)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
async fn vault(owner: &str, device: u32, purpose: &str, create: Option<&[u8]>) -> Result<Secret> {
    let service = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|_| Error::Vault)?;
    let id = device.to_string();
    let attributes = HashMap::from([
        ("application", APP),
        ("purpose", purpose),
        ("account", owner),
        ("device", id.as_str()),
    ]);
    let found = service
        .search_items(attributes.clone())
        .await
        .map_err(|_| Error::Vault)?;
    let mut items = found.unlocked;
    items.extend(found.locked);
    if items.len() > 1 {
        return Err(Error::Vault);
    }
    if let Some(item) = items.pop() {
        item.unlock().await.map_err(|_| Error::Vault)?;
        let value = Secret::new(item.get_secret().await.map_err(|_| Error::Vault)?);
        if create.is_some_and(|expected| expected != value.as_slice()) {
            return Err(Error::Incomplete);
        }
        return Ok(value);
    }
    let value = create.ok_or(Error::Vault)?;
    let collection = service
        .get_default_collection()
        .await
        .map_err(|_| Error::Vault)?;
    collection.unlock().await.map_err(|_| Error::Vault)?;
    collection
        .create_item(
            "Monogram PQ encryption state",
            attributes,
            value,
            false,
            "application/octet-stream",
        )
        .await
        .map_err(|_| Error::Vault)?;
    Ok(Secret::new(value.to_vec()))
}

impl Store {
    pub async fn open_profile(root: &Path, owner: &str, create: bool) -> Result<(Self, File)> {
        crate::crypto::address(owner, 1)?;
        let account =
            root.join("crypto")
                .join(crate::crypto::lifecycle::hex(&openssl::sha::sha256(
                    owner.as_bytes(),
                )));
        private(&account)?;
        let dir = account.join("pq");
        private(&dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lock"))
            .map_err(io)?;
        lock.try_lock().map_err(|_| {
            Error::Unavailable("Another Monogram session is using this PQ identity")
        })?;
        let path = dir.join("device.json");
        let database = dir.join("pq.sqlite3");
        let profile: Profile = if path.exists() {
            if path.metadata().map_err(io)?.len() > 8192 {
                return Err(Error::Incomplete);
            }
            serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(|_| Error::Incomplete)?
        } else {
            if !create || database.exists() {
                return Err(Error::Incomplete);
            }
            let service = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(|_| Error::Vault)?;
            for purpose in [MASTER, BINDING] {
                let found = service
                    .search_items(HashMap::from([
                        ("application", APP),
                        ("purpose", purpose),
                        ("account", owner),
                    ]))
                    .await
                    .map_err(|_| Error::Vault)?;
                if !found.unlocked.is_empty() || !found.locked.is_empty() {
                    return Err(Error::Incomplete);
                }
            }
            let device = loop {
                let id = u32::from_ne_bytes(random::<4>()?) & i32::MAX as u32;
                if id != 0 {
                    break id;
                }
            };
            let profile = Profile {
                version: 1,
                owner: owner.into(),
                device,
                fingerprint: None,
            };
            profile_save(&path, &profile)?;
            profile
        };
        if profile.version != 1 || profile.owner != owner {
            return Err(Error::Incomplete);
        }
        crate::crypto::address(owner, profile.device)?;
        let fresh = profile.fingerprint.is_none() && !database.exists();
        if profile.fingerprint.is_some() && !database.exists() {
            return Err(Error::Incomplete);
        }
        let key = if fresh {
            // An interrupted first initialization may already have its matching master item.
            let service = SecretService::connect(EncryptionType::Dh)
                .await
                .map_err(|_| Error::Vault)?;
            let id = profile.device.to_string();
            let found = service
                .search_items(HashMap::from([
                    ("application", APP),
                    ("purpose", MASTER),
                    ("account", owner),
                    ("device", id.as_str()),
                ]))
                .await
                .map_err(|_| Error::Vault)?;
            if found.unlocked.is_empty() && found.locked.is_empty() {
                let generated = MasterKey::new(random()?);
                vault(owner, profile.device, MASTER, Some(generated.as_slice())).await?
            } else {
                vault(owner, profile.device, MASTER, None).await?
            }
        } else {
            vault(owner, profile.device, MASTER, None).await?
        };
        let master = MasterKey::new(key.as_slice().try_into().map_err(|_| Error::Vault)?);
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .map_err(|_| Error::Vault)?;
        let id = profile.device.to_string();
        let binding = service
            .search_items(HashMap::from([
                ("application", APP),
                ("purpose", BINDING),
                ("account", owner),
                ("device", id.as_str()),
            ]))
            .await
            .map_err(|_| Error::Vault)?;
        let bound = binding.unlocked.len() + binding.locked.len();
        if bound > 1 || profile.fingerprint.is_some() && bound != 1 {
            return Err(Error::Vault);
        }
        let mut store = Self::open(Some(&database), owner, profile.device, master)?;
        store.allow_initialize = profile.fingerprint.is_none() && bound == 0;
        store.profile = Some(path);
        Ok((store, lock))
    }
    pub(super) fn open(
        path: Option<&Path>,
        owner: &str,
        device: u32,
        key: MasterKey,
    ) -> Result<Self> {
        crate::crypto::address(owner, device)?;
        let db = if let Some(path) = path {
            let db = Connection::open(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io)?;
            db
        } else {
            Connection::open_in_memory()?
        };
        db.busy_timeout(Duration::from_millis(250))?;
        db.execute_batch("PRAGMA secure_delete=ON; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS pq_records(owner TEXT NOT NULL,device INTEGER NOT NULL,key TEXT NOT NULL,body BLOB NOT NULL,PRIMARY KEY(owner,device,key));")?;
        Ok(Self {
            db,
            key,
            owner: owner.into(),
            device,
            profile: None,
            allow_initialize: true,
        })
    }
    pub fn memory(owner: &str, device: u32) -> Result<Self> {
        Self::open(None, owner, device, MasterKey::new(random()?))
    }
    pub fn begin(&mut self) -> Result<Tx<'_>> {
        Ok(Tx {
            sql: self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?,
            key: &self.key,
            owner: &self.owner,
            device: self.device,
        })
    }
    pub async fn bind_identity(&self, fingerprint: &str) -> Result<()> {
        let Some(path) = self.profile.as_ref() else {
            return Ok(());
        };
        let mut profile: Profile =
            serde_json::from_slice(&fs::read(path).map_err(io)?).map_err(|_| Error::Incomplete)?;
        if fingerprint.len() != 128
            || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || profile
                .fingerprint
                .as_ref()
                .is_some_and(|saved| saved != fingerprint)
        {
            return Err(Error::Incomplete);
        }
        let bytes = fingerprint.as_bytes();
        // A ready profile must have its pre-existing binding; never recreate a deleted pin.
        let saved = vault(
            &self.owner,
            self.device,
            BINDING,
            profile.fingerprint.is_none().then_some(bytes),
        )
        .await?;
        if saved.as_slice() != bytes {
            return Err(Error::Incomplete);
        }
        if profile.fingerprint.is_none() {
            profile.fingerprint = Some(fingerprint.into());
            profile_save(path, &profile)?;
        }
        Ok(())
    }
}
impl Tx<'_> {
    fn valid(key: &str, prefix: bool) -> Result<()> {
        if key.len() > 4096
            || !prefix && key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'-'))
        {
            return Err(Error::Invalid("PQ record key"));
        }
        Ok(())
    }
    fn aad(&self, key: &str) -> Vec<u8> {
        serde_json::to_vec(&("monogram/pq/records/1", self.owner, self.device, key))
            .expect("PQ record scope")
    }
    pub fn get(&self, key: &str) -> Result<Option<Secret>> {
        Self::valid(key, false)?;
        let value: Option<Vec<u8>> = self
            .sql
            .query_row(
                "SELECT body FROM pq_records WHERE owner=? AND device=? AND key=?",
                params![self.owner, self.device, key],
                |row| {
                    let blob = row.get_ref(0)?.as_blob()?;
                    if blob.len() > MAX + 29 {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    Ok(blob.to_vec())
                },
            )
            .optional()?;
        let Some(value) = value else { return Ok(None) };
        if value.len() < 29 || value[0] != 1 {
            return Err(Error::Incomplete);
        }
        let end = value.len() - 16;
        Ok(Some(Secret::new(decrypt_aead(
            Cipher::aes_256_gcm(),
            self.key,
            Some(&value[1..13]),
            &self.aad(key),
            &value[13..end],
            &value[end..],
        )?)))
    }
    pub fn put(&self, key: &str, value: Option<&[u8]>) -> Result<()> {
        Self::valid(key, false)?;
        if let Some(value) = value {
            if value.is_empty() || value.len() > MAX {
                return Err(Error::Invalid("PQ record size"));
            }
            let iv = random::<12>()?;
            let mut tag = [0; 16];
            let body = encrypt_aead(
                Cipher::aes_256_gcm(),
                self.key,
                Some(&iv),
                &self.aad(key),
                value,
                &mut tag,
            )?;
            let mut encrypted = Vec::with_capacity(body.len() + 29);
            encrypted.push(1);
            encrypted.extend(iv);
            encrypted.extend(body);
            encrypted.extend(tag);
            self.sql.execute("INSERT INTO pq_records(owner,device,key,body) VALUES(?,?,?,?) ON CONFLICT(owner,device,key) DO UPDATE SET body=excluded.body", params![self.owner, self.device, key, encrypted])?;
        } else {
            self.sql.execute(
                "DELETE FROM pq_records WHERE owner=? AND device=? AND key=?",
                params![self.owner, self.device, key],
            )?;
        }
        Ok(())
    }
    pub fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        Self::valid(prefix, true)?;
        let mut query = self.sql.prepare("SELECT key FROM pq_records WHERE owner=? AND device=? AND substr(key,1,?)=? ORDER BY key LIMIT 16385")?;
        let keys = query
            .query_map(
                params![self.owner, self.device, prefix.len() as i64, prefix],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        if keys.len() > 16384 {
            return Err(Error::Invalid("PQ record count"));
        }
        Ok(keys)
    }
    pub fn commit(self) -> Result<()> {
        self.sql.commit()?;
        Ok(())
    }
}
