//! Account-local initialization is serialized; a missing store never rotates a pinned identity.
use super::{
    engine::Engine,
    records::{IDENTITY, Store},
    *,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
#[derive(Serialize, Deserialize)]
struct Profile {
    version: u32,
    owner: String,
    device: u32,
    identity: Option<[u8; 32]>,
}
pub struct Local {
    pub engine: Engine,
    _lock: File,
}
pub fn root() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("monogram-desktop")
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn directory(root: &Path, owner: &str) -> PathBuf {
    root.join("crypto")
        .join(hex(&openssl::sha::sha256(owner.as_bytes())))
}
pub fn exists(root: &Path, owner: &str) -> bool {
    directory(root, owner).join("device.json").exists()
}
fn private(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(|_| Error::Incomplete)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Incomplete)
}
fn save(path: &Path, profile: &Profile) -> Result<()> {
    let temporary = path.with_file_name(format!("device-{}.tmp", crate::model::new_id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| Error::Incomplete)?;
    let bytes = serde_json::to_vec(profile).map_err(|_| Error::Incomplete)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Incomplete)?;
    fs::rename(&temporary, path).map_err(|_| Error::Incomplete)?;
    File::open(path.parent().unwrap())
        .and_then(|dir| dir.sync_all())
        .map_err(|_| Error::Incomplete)
}
impl Local {
    pub async fn open(root: &Path, owner: &str, create: bool) -> Result<Self> {
        address(owner, 1)?;
        private(root)?;
        private(&root.join("crypto"))?;
        let dir = directory(root, owner);
        private(&dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lock"))
            .map_err(|_| Error::Incomplete)?;
        lock.try_lock().map_err(|_| {
            Error::Unavailable(
                "Another Monogram session is using this identity; disconnect it before retrying",
            )
        })?;
        let path = dir.join("device.json");
        let mut profile = if path.exists() {
            let mut bytes = Vec::new();
            File::open(&path)
                .map_err(|_| Error::Incomplete)?
                .take(8193)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::Incomplete)?;
            if bytes.len() > 8192 {
                return Err(Error::Incomplete);
            }
            let profile: Profile = serde_json::from_slice(&bytes).map_err(|_| Error::Incomplete)?;
            address(&profile.owner, profile.device)?;
            if profile.version != 1 || profile.owner != owner {
                return Err(Error::Incomplete);
            }
            profile
        } else {
            if !create
                || dir.join("omemo2.sqlite3").exists()
                || !vault::existing_devices(owner).await?.is_empty()
            {
                return Err(Error::Incomplete);
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
                identity: None,
            };
            save(&path, &profile)?;
            profile
        };
        let database = dir.join("omemo2.sqlite3");
        // A ready profile must never recreate a master key or database after deletion.
        if profile.identity.is_some() && !database.exists() {
            return Err(Error::Incomplete);
        }
        let master = vault::master_key(
            owner,
            profile.device,
            profile.identity.is_none() && !database.exists(),
        )
        .await?;
        let pin = vault::identity_pin(owner, profile.device, None).await?;
        let mut store = Store::open(Some(&database), owner, profile.device, master)?;
        let public = store.transaction(|tx| tx.load(IDENTITY, "", 1))?;
        if let Some(expected) = pin {
            if public.as_ref().is_none_or(|key| key.as_slice() != expected)
                || profile.identity.is_some_and(|key| key != expected)
            {
                return Err(Error::Incomplete);
            }
        } else if profile.identity.is_some() {
            return Err(Error::Incomplete);
        }
        let now = chrono::Utc::now().timestamp_millis();
        if now < 0 {
            return Err(Error::Incomplete);
        }
        let engine = Engine::new(store, now as u64)?;
        let mut local = Self {
            engine,
            _lock: lock,
        };
        let public = local.fingerprint()?;
        if vault::identity_pin(owner, profile.device, Some(&public)).await? != Some(public) {
            return Err(Error::Incomplete);
        }
        if profile.identity != Some(public) {
            profile.identity = Some(public);
            save(&path, &profile)?;
        }
        Ok(local)
    }
    pub fn fingerprint(&mut self) -> Result<[u8; 32]> {
        self.engine.store.transaction(|tx| {
            tx.load(IDENTITY, "", 1)?
                .ok_or(Error::Incomplete)?
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires disposable Secret Service and private D-Bus"]
    fn isolated_identity_lifecycle() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let root =
            std::env::temp_dir().join(format!("monogram-identity-{}", crate::model::new_id()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let owner = "lifecycle@example.invalid";
            assert!(Local::open(&root, owner, false).await.is_err());
            let mut local = Local::open(&root, owner, true).await.unwrap();
            let id = local.engine.store.device;
            let identity = local.fingerprint().unwrap();
            assert!(
                Local::open(&root, owner, true).await.is_err(),
                "A second session acquired the identity"
            );
            drop(local);
            let mut local = Local::open(&root, owner, false).await.unwrap();
            assert_eq!(local.engine.store.device, id);
            assert_eq!(local.fingerprint().unwrap(), identity);
            drop(local);
            let dir = directory(&root, owner);
            let db = dir.join("omemo2.sqlite3");
            let backup = dir.join("saved.sqlite3");
            fs::rename(&db, &backup).unwrap();
            assert!(Local::open(&root, owner, true).await.is_err());
            fs::rename(&backup, &db).unwrap();
            let profile_path = dir.join("device.json");
            let saved = fs::read(&profile_path).unwrap();
            fs::write(&profile_path, vec![b' '; 8193]).unwrap();
            assert!(Local::open(&root, owner, true).await.is_err());
            fs::write(&profile_path, &saved).unwrap();
            fs::remove_file(&profile_path).unwrap();
            assert!(Local::open(&root, owner, true).await.is_err());
            fs::write(&profile_path, &saved).unwrap();
            // A crash after keyring binding but before committing the ready manifest recovers exactly.
            let mut profile: Profile = serde_json::from_slice(&saved).unwrap();
            profile.identity = None;
            save(&profile_path, &profile).unwrap();
            let mut local = Local::open(&root, owner, false).await.unwrap();
            assert_eq!(local.fingerprint().unwrap(), identity);
            drop(local);
            // Deleting all encrypted records cannot regenerate a key while the keyring pin remains.
            let database = rusqlite::Connection::open(&db).unwrap();
            database
                .execute_batch(
                    "DELETE FROM omemo2_records; UPDATE omemo2_scopes SET initialized=0;",
                )
                .unwrap();
            drop(database);
            assert!(Local::open(&root, owner, true).await.is_err());
        });
    }
}
