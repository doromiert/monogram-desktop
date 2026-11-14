//! Platform adapter: only portable user settings cross the authenticated LAN mesh.
use crate::{
    model::Account,
    packs::{Job, Library},
    storage::Store,
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use gtk::gio;
use monogram_sync::{Mesh, Meta};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use zeroize::Zeroizing;
static START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static EPOCH: AtomicU64 = AtomicU64::new(0);
static ACTIVE: LazyLock<Mutex<Option<Arc<Mesh>>>> = LazyLock::new(|| Mutex::new(None));
pub static REVISION: AtomicU64 = AtomicU64::new(0);
static ADAPTER_STATUS: Mutex<String> = Mutex::new(String::new());
const KEY_ACCOUNT: &str = "monogram-lan-sync@localhost";
type Result<T> = std::result::Result<T, String>;
fn error(_: impl std::fmt::Debug) -> String {
    "Could not read or save synchronized settings.".into()
}
fn root() -> PathBuf {
    crate::crypto::lifecycle::root().join("device-sync")
}
pub fn current() -> Option<Arc<Mesh>> {
    ACTIVE.lock().unwrap().clone()
}
pub async fn start() -> Result<Arc<Mesh>> {
    let _start = START.lock().await;
    let epoch = EPOCH.load(Ordering::Acquire);
    let _initializing = gio::spawn_blocking(|| {
        std::fs::create_dir_all(root()).map_err(error)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root().join("initialize.lock"))
            .map_err(error)?;
        file.try_lock()
            .map_err(|_| "Device sync is starting in another process.".to_string())?;
        Ok::<_, String>(file)
    })
    .await
    .map_err(error)??;
    if let Some(mesh) = current() {
        return Ok(mesh);
    }
    let saved = crate::credentials::load_service(KEY_ACCOUNT, "lan-sync-master")
        .await
        .map_err(error)?;
    let key = if let Some(saved) = saved {
        B64.decode(saved.as_bytes())
            .map_err(error)?
            .try_into()
            .map_err(error)?
    } else {
        let mut key = [0; 32];
        openssl::rand::rand_bytes(&mut key).map_err(error)?;
        crate::credentials::save_service(
            KEY_ACCOUNT,
            "lan-sync-master",
            Zeroizing::new(B64.encode(key)),
        )
        .await
        .map_err(error)?;
        key
    };
    let mesh = gio::spawn_blocking(move || {
        let mut sync=monogram_sync::Store::open(root(),key,gtk::glib::host_name().to_string())?;
        let native=Store::open(false).map_err(error)?;
        let mut query=native.db.prepare("SELECT substr(key,19) FROM preferences WHERE key LIKE 'lan-sync-excluded:%' AND value='true'").map_err(error)?;
        let owners=query.query_map([],|r|r.get::<_,String>(0)).map_err(error)?;
        for owner in owners{sync.exclude(&owner.map_err(error)?)?;}
        Mesh::start(sync)
    })
    .await
    .map_err(error)??;
    if EPOCH.load(Ordering::Acquire) != epoch {
        mesh.stop();
        return Err("Device sync stopped.".into());
    }
    let mut active = ACTIVE.lock().unwrap();
    if let Some(existing) = active.as_ref() {
        mesh.stop();
        return Ok(existing.clone());
    }
    *active = Some(mesh.clone());
    drop(active);
    let node = mesh.clone();
    std::thread::Builder::new().name("monogram-lan-settings".into()).spawn(move||{
  let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
  runtime.block_on(async move{
   let Ok(store)=Store::open(false)else{return;};
   while !node.is_stopped(){
    let result=cycle(&node,&store).await;*ADAPTER_STATUS.lock().unwrap()=result.err().unwrap_or_default();
    for _ in 0..20{if node.is_stopped(){return;}tokio::time::sleep(Duration::from_millis(500)).await;}
   }
  });
 }).map_err(error)?;
    Ok(mesh)
}
pub fn exclude(owner: &str) {
    if let Some(mesh) = current() {
        let owner = owner.to_owned();
        gio::spawn_blocking(move || mesh.exclude(&owner));
    }
}
pub fn stop() {
    EPOCH.fetch_add(1, Ordering::AcqRel);
    if let Some(mesh) = ACTIVE.lock().unwrap().take() {
        mesh.stop();
    }
}
pub fn key(kind: &str, owner: &str, id: &str) -> String {
    serde_json::to_string(&(kind, owner, id)).unwrap()
}
fn digest(value: &[u8]) -> String {
    crate::crypto::lifecycle::hex(&openssl::sha::sha256(value))
}
fn publish_json(mesh: &Mesh, kind: &str, owner: &str, id: &str, value: Value) -> Result<()> {
    let bytes = Zeroizing::new(serde_json::to_vec(&value).map_err(error)?);
    let sig = digest(&bytes);
    let key = key(kind, owner, id);
    let mut store = mesh.store.lock().unwrap();
    let old = store.signature(&key);
    if old.as_deref() == Some(&sig) {
        return Ok(());
    }
    if old.is_none() && store.pending().iter().any(|m| m.key() == key) {
        return Ok(());
    }
    store.put_json(kind, owner, id, std::str::from_utf8(&bytes).map_err(error)?)?;
    store.set_signature(&key, sig)?;
    Ok(())
}
async fn cycle(mesh: &Arc<Mesh>, store: &Store) -> Result<()> {
    // Existing local edits publish first; fresh defaults never overwrite a pending import.
    for account in store.accounts() {
        let owner = &account.jid;
        if store.pref(&format!("lan-sync-excluded:{owner}"), "") == "true" {
            mesh.exclude(owner)?;
            continue;
        }
        mesh.store.lock().unwrap().allow(owner)?;
        let password = match crate::credentials::load(owner).await {
            Ok(p) => p,
            Err(crate::credentials::Error::Missing) => Zeroizing::new(String::new()),
            Err(_) => return Err("Unlock the system keyring to sync account credentials.".into()),
        };
        publish_json(
            mesh,
            "account",
            owner,
            "login",
            json!({"jid":owner,"host":account.host,"port":account.port,"direct_tls":account.direct_tls,"password":password.as_str()}),
        )?;
        for service in ["giphy", "telegram-packs", "translation", "monero-daemon"] {
            let value = crate::credentials::load_service(owner, service)
                .await
                .map_err(error)?
                .unwrap_or_default();
            if !value.is_empty()
                || mesh
                    .store
                    .lock()
                    .unwrap()
                    .signature(&key("token", owner, service))
                    .is_some()
            {
                publish_json(
                    mesh,
                    "token",
                    owner,
                    service,
                    json!({"value":value.as_str()}),
                )?;
            }
        }
        let settings = store.pref(&format!("monero-settings:{owner}"), "");
        if !settings.is_empty() {
            publish_json(
                mesh,
                "monero-settings",
                owner,
                "settings",
                serde_json::from_str(&settings).map_err(error)?,
            )?;
        }
        let folders = store.pref(&format!("folders:{owner}"), "");
        if !folders.is_empty() {
            publish_json(
                mesh,
                "folders",
                owner,
                "folders",
                serde_json::from_str(&folders).map_err(error)?,
            )?;
        }
        export_fingerprints(mesh, owner)?;
        let library = Library::open(owner).await.map_err(error)?;
        for pack in library.snapshot().await.map_err(error)?.packs {
            if pack.items.is_empty() || pack.source.starts_with("local:replaced:") {
                continue;
            }
            let pack = library.sync_tag(pack.id).await.map_err(error)?;
            let mut value = serde_json::to_value(&pack).map_err(error)?;
            for field in ["last_sync", "last_attempt", "sync_error"] {
                value.as_object_mut().unwrap().remove(field);
            }
            let sig = digest(&serde_json::to_vec(&value).map_err(error)?);
            let k = key("pack", owner, &pack.sync_id);
            let old = mesh.store.lock().unwrap().signature(&k);
            if old.as_deref() == Some(&sig) {
                continue;
            }
            if old.is_none()
                && mesh
                    .store
                    .lock()
                    .unwrap()
                    .pending()
                    .iter()
                    .any(|m| m.key() == k)
            {
                continue;
            }
            *ADAPTER_STATUS.lock().unwrap() = "Preparing sticker and emoji packs…".into();
            let path = library
                .export_sync_xmsp(pack.id, Job::new())
                .await
                .map_err(error)?;
            let result = mesh
                .store
                .lock()
                .unwrap()
                .put("pack", owner, &pack.sync_id, &path);
            let _ = std::fs::remove_file(path);
            result?;
            mesh.store.lock().unwrap().set_signature(&k, sig)?;
        }
    }
    let pending = mesh.store.lock().unwrap().pending();
    for meta in pending {
        if store.pref(&format!("lan-sync-excluded:{}", meta.owner), "") == "true" {
            mesh.exclude(&meta.owner)?;
            continue;
        }
        if mesh.is_stopped() {
            return Ok(());
        }
        let path = mesh.store.lock().unwrap().stage_export(&meta.key())?;
        let applied = apply(store, &meta, &path).await;
        let _ = std::fs::remove_file(path);
        match applied {
            Ok(signature) => {
                let mut sync = mesh.store.lock().unwrap();
                sync.mark_applied(&meta.key(), &meta.digest)?;
                sync.set_signature(&meta.key(), signature)?;
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
            Err(reason) => return Err(reason),
        }
    }
    Ok(())
}
async fn apply(store: &Store, meta: &Meta, path: &std::path::Path) -> Result<String> {
    if meta.kind == "pack" {
        if !store.accounts().iter().any(|a| a.jid == meta.owner) {
            return Err("Waiting for the pack’s account to sync.".into());
        }
        let library = Library::open(&meta.owner).await.map_err(error)?;
        let pack = library
            .import_sync_xmsp(path.to_owned(), Job::new(), meta.id.clone())
            .await
            .map_err(error)?;
        let mut value = serde_json::to_value(pack).map_err(error)?;
        for field in ["last_sync", "last_attempt", "sync_error"] {
            value.as_object_mut().unwrap().remove(field);
        }
        return Ok(digest(&serde_json::to_vec(&value).map_err(error)?));
    }
    let data = Zeroizing::new(std::fs::read(path).map_err(error)?);
    let value: Value = serde_json::from_slice(&data).map_err(error)?;
    monogram_sync::profile::validate(&meta.kind, &meta.owner, &meta.id, &value)?;
    if crate::protocol::bare(&meta.owner).ok().as_deref() != Some(&meta.owner) {
        return Err("Invalid synchronized account.".into());
    }
    match meta.kind.as_str() {
        "account" => {
            let account = Account {
                jid: meta.owner.clone(),
                host: value["host"].as_str().unwrap().into(),
                port: value["port"].as_u64().unwrap() as u16,
                direct_tls: value["direct_tls"].as_bool().unwrap(),
            };
            let password = value["password"].as_str().unwrap();
            if !password.is_empty() {
                crate::credentials::store(&meta.owner, Zeroizing::new(password.into()))
                    .await
                    .map_err(error)?;
            }
            if let Err(failure) = store.import_synced_account(&account) {
                if !store.accounts().iter().any(|a| a.jid == meta.owner) {
                    let _ = crate::credentials::remove(&meta.owner).await;
                }
                return Err(error(failure));
            }
        }
        "token" => {
            let secret = value["value"].as_str().unwrap();
            if secret.is_empty() {
                crate::credentials::remove_service(&meta.owner, &meta.id)
                    .await
                    .map_err(error)?;
            } else {
                crate::credentials::save_service(
                    &meta.owner,
                    &meta.id,
                    Zeroizing::new(secret.into()),
                )
                .await
                .map_err(error)?;
            }
        }
        "monero-settings" => {
            let settings: crate::monero_settings_ui::Settings =
                serde_json::from_value(value.clone()).map_err(error)?;
            crate::monero_settings_ui::validate(&settings).map_err(str::to_owned)?;
            store
                .set_pref(
                    &format!("monero-settings:{}", meta.owner),
                    &value.to_string(),
                )
                .map_err(error)?;
        }
        "folders" => {
            store
                .set_pref(&format!("folders:{}", meta.owner), &value.to_string())
                .map_err(error)?;
        }
        "fingerprint" => {
            store
                .set_pref(
                    &format!("synced-fingerprint:{}:{}", meta.owner, meta.id),
                    &value.to_string(),
                )
                .map_err(error)?;
        }
        "wallet" => {
            // The encrypted portable capsule remains in the sync vault; never expose keys in preferences.
            let public = json!({"name":value["name"],"network":value["network"],"address":value["address"],"node":value["node"],"height":value["height"]});
            store
                .set_pref(
                    &format!("synced-wallet:{}:{}", meta.owner, meta.id),
                    &public.to_string(),
                )
                .map_err(error)?;
        }
        _ => return Err("Unsupported synchronized setting.".into()),
    }
    Ok(digest(&serde_json::to_vec(&value).map_err(error)?))
}
fn export_fingerprints(mesh: &Mesh, owner: &str) -> Result<()> {
    let root = crate::crypto::lifecycle::root()
        .join("crypto")
        .join(digest(owner.as_bytes()));
    for (mode, directory) in [
        ("classic", root.clone()),
        ("legacy", root.join("legacy")),
        ("pq", root.join("pq")),
    ] {
        let path = directory.join("device.json");
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(error)?;
        if bytes.len() > 8192 {
            return Err("Invalid local device profile.".into());
        }
        let profile: Value = serde_json::from_slice(&bytes).map_err(error)?;
        if profile["owner"] != owner {
            continue;
        }
        let fingerprint = if mode == "pq" {
            profile["fingerprint"].as_str().map(str::to_owned)
        } else {
            profile["identity"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .map(|v| v.as_u64().filter(|n| *n < 256).map(|n| n as u8))
                        .collect::<Option<Vec<_>>>()
                })
                .map(|b| crate::crypto::lifecycle::hex(&b))
        };
        if let Some(fingerprint) = fingerprint {
            let value = json!({"address":owner,"mode":mode,"device":profile["device"],"fingerprint":fingerprint,"trust":"verified"});
            let id = digest(format!("{mode}:{}:{fingerprint}", profile["device"]).as_bytes());
            publish_json(mesh, "fingerprint", owner, &id, value)?;
        }
    }
    Ok(())
}
pub async fn summary(mesh: Arc<Mesh>) -> Value {
    gio::spawn_blocking(move||{let store=mesh.store.lock().unwrap();let peers:Vec<_>=store.state.peers.values().map(|p|json!({"id":p.id,"name":p.name})).collect();let status=ADAPTER_STATUS.lock().unwrap().clone();json!({"status":if status.is_empty(){mesh.status.lock().unwrap().clone()}else{status},"peers":peers,"records":store.state.objects.len(),"pending":store.pending().len()})}).await.unwrap_or_else(|_|json!({"status":"Device sync unavailable","peers":[]}))
}
pub fn public_records(store: &Store, prefix: &str) -> Vec<Value> {
    let mut query = match store
        .db
        .prepare("SELECT value FROM preferences WHERE substr(key,1,?)=? ORDER BY key LIMIT 4096")
    {
        Ok(q) => q,
        Err(_) => return vec![],
    };
    query
        .query_map(rusqlite::params![prefix.len() as i64, prefix], |r| {
            r.get::<_, String>(0)
        })
        .map(|rows| {
            rows.flatten()
                .filter_map(|v| serde_json::from_str(&v).ok())
                .collect()
        })
        .unwrap_or_default()
}
#[cfg(test)]
mod native_tests {
    use super::*;
    #[test]
    #[ignore = "requires isolated Secret Service"]
    fn native_stores_and_restart_keep_credentials_and_folder_updates() {
        assert!(std::env::var_os("MONOGRAM_KEYRING_FIXTURE").is_some());
        gtk::glib::MainContext::default().block_on(async {
            let store=Store::open(true).unwrap();let owner="sync-fixture@example.invalid";
            let path=std::env::temp_dir().join(format!("monogram-native-import-{}",uuid::Uuid::new_v4()));
            let mut meta=Meta{kind:"account".into(),owner:owner.into(),id:"login".into(),stamp:1,origin:uuid::Uuid::new_v4().to_string(),digest:String::new(),size:0};
            let account=json!({"jid":owner,"host":"example.invalid","port":5222,"direct_tls":false,"password":"fictional-sync-password"});
            std::fs::write(&path,account.to_string()).unwrap();apply(&store,&meta,&path).await.unwrap();assert_eq!(crate::credentials::load(owner).await.unwrap().as_str(),"fictional-sync-password");assert_eq!(store.accounts()[0].jid,owner);
            meta.kind="token".into();meta.id="giphy".into();std::fs::write(&path,json!({"value":"fictional-api-key"}).to_string()).unwrap();apply(&store,&meta,&path).await.unwrap();assert_eq!(crate::credentials::load_service(owner,"giphy").await.unwrap().unwrap().as_str(),"fictional-api-key");
            let old=store.load_windowed(owner);let folders=json!({"rules":{"Friends":{"types":["contacts"],"exclude_muted":false,"exclude_read":false,"exclude_archived":true,"include":[],"exclude":[],"icon":"cat","pinned":[]}},"order":["Friends","All"],"all_hidden":true});
            meta.kind="folders".into();meta.id="folders".into();std::fs::write(&path,folders.to_string()).unwrap();apply(&store,&meta,&path).await.unwrap();store.save_chat_state(&old).unwrap();let reloaded=store.load_windowed(owner);assert!(reloaded.all_folder_hidden);assert_eq!(reloaded.folders["Friends"].icon,"cat");
            store.forget(owner).unwrap();meta.kind="account".into();meta.id="login".into();std::fs::write(&path,account.to_string()).unwrap();assert!(apply(&store,&meta,&path).await.is_err());assert!(store.accounts().is_empty());
            let first=start().await.unwrap();let id=first.store.lock().unwrap().state.id.clone();stop();drop(first);gtk::glib::timeout_future(Duration::from_millis(800)).await;
            let again=start().await.unwrap();assert_eq!(again.store.lock().unwrap().state.id,id);stop();drop(again);
            let _=crate::credentials::remove(owner).await;let _=crate::credentials::remove_service(owner,"giphy").await;let _=std::fs::remove_file(path);
            println!("PASS native keyring imports, folder persistence, removed-account fencing and encrypted-store restart identity");
        });
    }
}
