// SPDX-License-Identifier: GPL-3.0-or-later
//! Account-private GIF library and opt-in direct GIPHY, matching mobile MonogramGiphy.
//! Only provider credentials enter Secret Service; no request URL or key is logged.
use crate::{downloads, file_share::Attachment};
use gtk::{gio, glib, prelude::*};
use openssl::hash::{Hasher, MessageDigest};
use reqwest::{Url, header};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;
const JSON_LIMIT: usize = 4 * 1024 * 1024;
const PREVIEW_LIMIT: u64 = 10 * 1024 * 1024;
const CACHE_SECONDS: i64 = 300;
const SERVICE: &str = "giphy";
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    KeyRequired,
    KeyRejected,
    RateLimited,
    InvalidKey,
    InvalidQuery,
    NoMoreResults,
    InvalidAsset,
    TooLarge,
    Unavailable,
    Storage,
    KeyringLocked,
    KeyringUnavailable,
    InvalidAccount,
    LibraryFull,
    MissingFile,
    Busy,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "GIF request cancelled.",
            Self::KeyRequired => "Add a GIPHY API key to search online.",
            Self::KeyRejected => "GIPHY rejected the API key. Add a replacement key.",
            Self::RateLimited => "GIPHY's request limit was reached. Try again later.",
            Self::InvalidKey => "Enter a valid GIPHY API key.",
            Self::InvalidQuery => "GIF searches can contain up to 50 characters.",
            Self::NoMoreResults => "No more GIFs available.",
            Self::InvalidAsset => "Invalid GIF source or image.",
            Self::TooLarge => "This GIF exceeds the download or automatic preview limit.",
            Self::Unavailable => {
                "Could not load GIFs. Check your connection and saved GIPHY API key."
            }
            Self::Storage => "The GIF library could not be saved or read.",
            Self::KeyringLocked => "The system keyring is locked. Saved GIFs remain available.",
            Self::KeyringUnavailable => {
                "The system keyring could not save or load the GIPHY API key."
            }
            Self::InvalidAccount => "The account changed. Reopen GIFs on the intended account.",
            Self::LibraryFull => {
                "Saved GIF metadata exceeds 4 MiB. Remove saved GIFs before adding more."
            }
            Self::MissingFile => "This saved GIF file is no longer available.",
            Self::Busy => "The GIPHY API key is still being changed.",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GifAsset {
    pub url: Option<String>,
    pub path: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub sha256: Option<String>,
}
impl GifAsset {
    /// Verified local descriptor for the existing sandboxed texture/animation loader.
    pub fn attachment(&self) -> Option<Attachment> {
        let hash = decode_hash(self.sha256.as_deref()?)?;
        let address = self.url.clone().unwrap_or_else(|| {
            format!(
                "https://local-gifs.invalid/{}.gif",
                self.sha256.as_deref().unwrap()
            )
        });
        let mut file = Attachment::bare(&address)?;
        file.name = "animation.gif".into();
        file.mime = "image/gif".into();
        file.size = Some(self.size);
        file.width = Some(self.width);
        file.height = Some(self.height);
        file.manual = true;
        file.hashes.insert("sha-256".into(), hash);
        Some(file)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GifItem {
    pub id: String,
    pub title: String,
    pub original: GifAsset,
    pub preview: GifAsset,
    pub favorite: bool,
    pub recent: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Page {
    pub items: Vec<GifItem>,
    pub next_offset: Option<u32>,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub favorites: Vec<GifItem>,
    pub recents: Vec<GifItem>,
}
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub active: bool,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct Request {
    cancelled: Arc<AtomicBool>,
    changed: Arc<tokio::sync::watch::Sender<bool>>,
    progress: Arc<Mutex<Progress>>,
    decoder: gio::Cancellable,
}
impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}
impl Request {
    pub fn new() -> Self {
        let (changed, _) = tokio::sync::watch::channel(false);
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            changed: Arc::new(changed),
            progress: Arc::new(Mutex::new(Progress {
                stage: "Preparing GIF".into(),
                active: true,
                ..Default::default()
            })),
            decoder: gio::Cancellable::new(),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.decoder.cancel();
        self.changed.send_replace(true);
    }
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }
    fn update(&self, stage: &str, bytes: u64, total: Option<u64>) {
        *self.progress.lock().unwrap() = Progress {
            stage: stage.into(),
            bytes,
            total,
            active: true,
            error: None,
        };
    }
    fn finish<T>(&self, result: &Result<T>) {
        let mut p = self.progress.lock().unwrap();
        p.active = false;
        p.error = result.as_ref().err().map(ToString::to_string);
        if result.is_ok() {
            p.stage = "Complete".into();
        }
    }
}
struct Owner {
    account: String,
    path: PathBuf,
    cache: PathBuf,
    gate: Mutex<()>,
    valid: AtomicBool,
    key_saving: AtomicBool,
    epoch: AtomicU64,
    changed: tokio::sync::watch::Sender<u64>,
}
impl Owner {
    fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
        self.bump();
    }
    fn bump(&self) -> u64 {
        let next = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.changed.send_replace(next);
        next
    }
}
static OWNERS: LazyLock<Mutex<HashMap<PathBuf, Weak<Owner>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LIFECYCLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static KEYS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static SEARCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
#[derive(Clone)]
pub struct Library {
    owner: Arc<Owner>,
}
#[derive(Clone)]
struct Context {
    owner: Arc<Owner>,
    request: Request,
    search: Option<u64>,
}
impl Context {
    fn check(&self) -> Result<()> {
        if !self.owner.valid.load(Ordering::Acquire) {
            Err(Error::InvalidAccount)
        } else if self.request.cancelled.load(Ordering::Acquire)
            || self
                .search
                .is_some_and(|epoch| epoch != self.owner.epoch.load(Ordering::Acquire))
        {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    async fn cancellation(&self) {
        let mut owner = self.owner.changed.subscribe();
        let mut job = self.request.changed.subscribe();
        while self.check().is_ok() {
            tokio::select! {_=owner.changed()=>{},_=job.changed()=>{}}
        }
    }
}
fn executor() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("monogram-gifs")
            .enable_all()
            .build()
            .expect("GIF executor")
    })
}
async fn worker<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    executor()
        .spawn_blocking(work)
        .await
        .map_err(|_| Error::Storage)?
}
struct Abort {
    abort: tokio::task::AbortHandle,
    request: Request,
    finished: bool,
}
impl Drop for Abort {
    fn drop(&mut self) {
        if !self.finished {
            self.request.cancel();
            self.abort.abort();
        }
    }
}
impl Library {
    /// Opening a local library never reads a provider key or contacts GIPHY.
    pub async fn open(account: &str) -> Result<Self> {
        if crate::protocol::bare(account).ok().as_deref() != Some(account) {
            return Err(Error::InvalidAccount);
        }
        let _lifecycle = LIFECYCLE.lock().await;
        let account = account.to_owned();
        worker(move || {
            let (path, cache) = roots(&account)?;
            let mut owners = OWNERS.lock().unwrap();
            if let Some(owner) = owners
                .get(&path)
                .and_then(Weak::upgrade)
                .filter(|owner| owner.valid.load(Ordering::Acquire))
            {
                return Ok(Self { owner });
            }
            private_dir(&path)?;
            private_dir(&path.join("originals"))?;
            private_dir(&path.join("previews"))?;
            private_dir(&cache)?;
            let (changed, _) = tokio::sync::watch::channel(0);
            let owner = Arc::new(Owner {
                account,
                path: path.clone(),
                cache,
                gate: Mutex::new(()),
                valid: AtomicBool::new(true),
                key_saving: AtomicBool::new(false),
                epoch: AtomicU64::new(0),
                changed,
            });
            init_schema(&open_db(&owner)?)?;
            owners.insert(path, Arc::downgrade(&owner));
            Ok(Self { owner })
        })
        .await
    }
    pub fn account(&self) -> &str {
        &self.owner.account
    }
    #[cfg(test)]
    pub(crate) async fn fixture_cached_page(
        &self,
        key: &str,
        query: &str,
        offset: u32,
        page: Page,
    ) {
        let owner = self.owner.clone();
        let id = hash(format!("{key}\n{query}\n{offset}").as_bytes());
        worker(move || {
            let _gate = owner.gate.lock().unwrap();
            open_db(&owner)?
                .execute(
                    "INSERT OR REPLACE INTO pages VALUES(?,?,?)",
                    params![
                        id,
                        serde_json::to_string(&page).unwrap(),
                        seconds() + CACHE_SECONDS
                    ],
                )
                .map_err(|_| Error::Storage)?;
            Ok(())
        })
        .await
        .unwrap();
    }
    /// Account lifecycle only; close individual views with Request::cancel instead.
    pub fn cancel(&self) {
        self.owner.invalidate();
    }
    pub async fn configured(&self) -> Result<bool> {
        let owner = self.owner.clone();
        executor()
            .spawn(async move {
                check_owner(&owner)?;
                let key = crate::credentials::load_service(&owner.account, SERVICE)
                    .await
                    .map_err(key_error)?;
                check_owner(&owner)?;
                Ok(key.is_some())
            })
            .await
            .map_err(|_| Error::KeyringUnavailable)?
    }
    pub async fn save_key(&self, value: Zeroizing<String>) -> Result<()> {
        let key = Zeroizing::new(value.trim().to_owned());
        if !key.is_empty() && !valid_key(&key) {
            return Err(Error::InvalidKey);
        }
        check_owner(&self.owner)?;
        self.owner.key_saving.store(true, Ordering::Release);
        let epoch = self.owner.bump();
        let owner = self.owner.clone();
        executor()
            .spawn(async move {
                let _keys = KEYS.lock().await;
                let result = async {
                    check_owner(&owner)?;
                    if owner.epoch.load(Ordering::Acquire) != epoch {
                        return Err(Error::Cancelled);
                    }
                    if key.is_empty() {
                        crate::credentials::remove_service(&owner.account, SERVICE)
                            .await
                            .map_err(key_error)?;
                    } else {
                        crate::credentials::save_service(&owner.account, SERVICE, key)
                            .await
                            .map_err(key_error)?;
                    }
                    if !owner.valid.load(Ordering::Acquire) {
                        let _ = crate::credentials::remove_service(&owner.account, SERVICE).await;
                        return Err(Error::InvalidAccount);
                    }
                    let owner = owner.clone();
                    worker(move || {
                        let _gate = owner.gate.lock().unwrap();
                        check_owner(&owner)?;
                        open_db(&owner)?
                            .execute("DELETE FROM pages", [])
                            .map_err(|_| Error::Storage)?;
                        Ok(())
                    })
                    .await
                }
                .await;
                if owner.epoch.load(Ordering::Acquire) == epoch {
                    owner.key_saving.store(false, Ordering::Release);
                }
                result
            })
            .await
            .map_err(|_| Error::KeyringUnavailable)?
    }
    pub async fn remove_key(&self) -> Result<()> {
        self.save_key(Zeroizing::new(String::new())).await
    }
    async fn operation<
        T: Send + 'static,
        F: std::future::Future<Output = Result<T>> + Send + 'static,
    >(
        &self,
        request: Request,
        search: bool,
        stage: &str,
        work: impl FnOnce(Context) -> F + Send + 'static,
    ) -> Result<T> {
        request.update(stage, 0, None);
        let context = Context {
            owner: self.owner.clone(),
            request: request.clone(),
            search: search.then(|| self.owner.epoch.load(Ordering::Acquire)),
        };
        let cancel = context.clone();
        let task=executor().spawn(async move{context.check()?;tokio::select!{biased;_=cancel.cancellation()=>{cancel.request.cancel();Err(Error::Cancelled)},result=work(context)=>result}});
        let mut guard = Abort {
            abort: task.abort_handle(),
            request: request.clone(),
            finished: false,
        };
        let result = task.await.unwrap_or(Err(Error::Unavailable));
        guard.finished = true;
        request.finish(&result);
        result
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let owner = self.owner.clone();
        worker(move || {
            let _gate = owner.gate.lock().unwrap();
            check_owner(&owner)?;
            read_snapshot(&owner)
        })
        .await
    }
    pub async fn search(
        &self,
        query: String,
        offset: Option<u32>,
        request: Request,
    ) -> Result<Page> {
        self.operation(request, true, "Searching GIFs", move |context| async move {
            search(context, query, offset.unwrap_or(0)).await
        })
        .await
    }
    pub async fn preview(&self, item: GifItem, request: Request) -> Result<GifAsset> {
        self.operation(
            request,
            false,
            "Loading GIF preview",
            move |context| async move { preview(context, item).await },
        )
        .await
    }
    pub async fn original(&self, item: GifItem, request: Request) -> Result<GifAsset> {
        self.operation(
            request,
            false,
            "Downloading GIF",
            move |context| async move { asset(context, item.original, false).await },
        )
        .await
    }
    pub async fn remember(&self, item: GifItem, request: Request) -> Result<GifItem> {
        self.change(item, None, false, request).await
    }
    pub async fn set_favorite(
        &self,
        item: GifItem,
        favorite: bool,
        request: Request,
    ) -> Result<GifItem> {
        self.change(item, Some(favorite), false, request).await
    }
    pub async fn remove_recent(&self, item: GifItem) -> Result<GifItem> {
        self.change(item, None, true, Request::new()).await
    }
    async fn change(
        &self,
        item: GifItem,
        favorite: Option<bool>,
        remove_recent: bool,
        request: Request,
    ) -> Result<GifItem> {
        self.operation(request, false, "Saving GIF", move |context| async move {
            change_library(context, item, favorite, remove_recent).await
        })
        .await
    }
    pub async fn import_local(
        &self,
        path: PathBuf,
        title: String,
        request: Request,
    ) -> Result<GifItem> {
        self.operation(request, false, "Importing GIF", move |context| async move {
            import_local(context, path, title).await
        })
        .await
    }
}

fn key_error(error: crate::credentials::Error) -> Error {
    match error {
        crate::credentials::Error::Locked => Error::KeyringLocked,
        _ => Error::KeyringUnavailable,
    }
}
fn check_owner(owner: &Owner) -> Result<()> {
    owner
        .valid
        .load(Ordering::Acquire)
        .then_some(())
        .ok_or(Error::InvalidAccount)
}
pub fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}
pub fn valid_asset_url(address: &str) -> bool {
    asset_url(address).is_ok()
}
fn asset_url(address: &str) -> Result<Url> {
    if address.contains('#') {
        return Err(Error::InvalidAsset);
    }
    let url = crate::link_previews::safe_url(address).map_err(|_| Error::InvalidAsset)?;
    let host = url.host_str().ok_or(Error::InvalidAsset)?;
    if !(host == "giphy.com"
        || host.strip_suffix(".giphy.com").is_some_and(|prefix| {
            !prefix.is_empty() && prefix.split('.').all(|label| !label.is_empty())
        }))
        || url
            .query_pairs()
            .any(|(key, _)| key.eq_ignore_ascii_case("api_key"))
    {
        return Err(Error::InvalidAsset);
    }
    Ok(url)
}
fn hash(bytes: &[u8]) -> String {
    hex(&openssl::sha::sha256(bytes))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode_hash(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect()
}
fn roots(account: &str) -> Result<(PathBuf, PathBuf)> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".local/share")))
        .ok_or(Error::Storage)?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".cache")))
        .ok_or(Error::Storage)?;
    let id = hash(account.as_bytes());
    Ok((
        data.join("monogram-desktop/gifs").join(&id),
        cache.join("monogram-desktop/gifs").join(id),
    ))
}
fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| Error::Storage)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Storage)
}
fn open_db(owner: &Owner) -> Result<Connection> {
    check_owner(owner)?;
    let path = owner.path.join("gifs.sqlite3");
    let db = Connection::open(&path).map_err(|_| Error::Storage)?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| Error::Storage)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| Error::Storage)?;
    Ok(db)
}
fn init_schema(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS library(id TEXT PRIMARY KEY,body TEXT NOT NULL,favorite INTEGER NOT NULL,recent INTEGER NOT NULL,position INTEGER NOT NULL);CREATE TABLE IF NOT EXISTS files(id TEXT PRIMARY KEY,body TEXT NOT NULL,used INTEGER NOT NULL);CREATE TABLE IF NOT EXISTS pages(id TEXT PRIMARY KEY,body TEXT NOT NULL,expires INTEGER NOT NULL);").map_err(|_| Error::Storage)
}
fn seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
fn number(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn parse_asset(value: &serde_json::Value) -> Option<GifAsset> {
    let address = value.get("url")?.as_str()?;
    let url = asset_url(address).ok()?;
    let width = u32::try_from(number(value.get("width"))?).ok()?;
    let height = u32::try_from(number(value.get("height"))?).ok()?;
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return None;
    }
    Some(GifAsset {
        url: Some(url.to_string()),
        path: None,
        width,
        height,
        size: number(value.get("size")).unwrap_or(0),
        sha256: None,
    })
}
pub fn parse_page(bytes: &[u8], trending: bool) -> Result<Page> {
    if bytes.len() > JSON_LIMIT {
        return Err(Error::TooLarge);
    }
    let response: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)?;
    let data = response
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or(Error::Unavailable)?;
    let mut page = Page::default();
    for gif in data.iter().take(25) {
        let Some(id) = gif.get("id").and_then(|v| v.as_str()).filter(|id| {
            !id.is_empty() && id.encode_utf16().count() <= 128 && !id.chars().any(char::is_control)
        }) else {
            continue;
        };
        let Some(images) = gif.get("images") else {
            continue;
        };
        let Some(original) = images.get("original").and_then(parse_asset) else {
            continue;
        };
        let preview = images
            .get("fixed_width")
            .and_then(parse_asset)
            .unwrap_or_else(|| original.clone());
        let title = gif
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect();
        page.items.push(GifItem {
            id: format!("giphy-{id}"),
            title,
            original,
            preview,
            favorite: false,
            recent: false,
        });
    }
    if let Some(pagination) = response.get("pagination") {
        let offset = number(pagination.get("offset")).unwrap_or(0);
        let count = number(pagination.get("count")).unwrap_or(data.len() as u64);
        if let Some(next) = offset.checked_add(count) {
            let total = number(pagination.get("total_count")).unwrap_or(next);
            if !data.is_empty()
                && next > offset
                && next < total
                && next <= if trending { 499 } else { 4999 }
            {
                page.next_offset = Some(next as u32);
            }
        }
    }
    Ok(page)
}
fn library_id(item: &GifItem) -> Result<String> {
    if let Some(url) = &item.original.url {
        Ok(format!("url:{}", asset_url(url)?))
    } else if let Some(sha) = item
        .original
        .sha256
        .as_ref()
        .filter(|sha| decode_hash(sha).is_some())
    {
        Ok(format!("local:{sha}"))
    } else {
        Err(Error::InvalidAsset)
    }
}
fn gif_dimensions(header: &[u8]) -> Result<(u32, u32)> {
    if header.len() < 13 || !(header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a")) {
        return Err(Error::InvalidAsset);
    }
    let w = u16::from_le_bytes([header[6], header[7]]) as u32;
    let h = u16::from_le_bytes([header[8], header[9]]) as u32;
    if w == 0 || h == 0 || w > 16384 || h > 16384 || u64::from(w) * u64::from(h) > 64_000_000 {
        return Err(Error::TooLarge);
    }
    Ok((w, h))
}
fn owned_path(owner: &Owner, path: &Path) -> bool {
    if !fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    [&owner.path, &owner.cache]
        .into_iter()
        .any(|root| root.canonicalize().is_ok_and(|root| path.starts_with(root)))
}
fn local_asset(owner: &Owner, source: &GifAsset, limit: u64) -> Option<GifAsset> {
    let path = source.path.as_ref()?;
    if !owned_path(owner, path) {
        return None;
    }
    let mut input = fs::File::open(path).ok()?;
    let size = input.metadata().ok()?.len();
    if size == 0
        || size > limit
        || source.size != size
        || decode_hash(source.sha256.as_deref()?).is_none()
    {
        return None;
    }
    let mut header = [0; 13];
    input.read_exact(&mut header).ok()?;
    let (width, height) = gif_dimensions(&header).ok()?;
    Some(GifAsset {
        width,
        height,
        ..source.clone()
    })
}
fn read_snapshot(owner: &Owner) -> Result<Snapshot> {
    let db = open_db(owner)?;
    let mut query = db
        .prepare("SELECT body FROM library ORDER BY position DESC,id")
        .map_err(|_| Error::Storage)?;
    let rows = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| Error::Storage)?;
    let mut snapshot = Snapshot::default();
    for body in rows.flatten() {
        let Ok(mut item) = serde_json::from_str::<GifItem>(&body) else {
            continue;
        };
        if library_id(&item).is_err() {
            continue;
        }
        item.original.path =
            local_asset(owner, &item.original, crate::file_share::MAX).and_then(|asset| asset.path);
        item.preview.path =
            local_asset(owner, &item.preview, PREVIEW_LIMIT).and_then(|asset| asset.path);
        if item.preview.path.is_none() && item.original.size <= PREVIEW_LIMIT {
            item.preview = item.original.clone();
        }
        if item.favorite {
            snapshot.favorites.push(item.clone());
        }
        if item.recent {
            snapshot.recents.push(item);
        }
    }
    Ok(snapshot)
}
async fn provider_client(url: &Url, connect: u64, read: u64) -> Result<reqwest::Client> {
    let host = url.host_str().ok_or(Error::InvalidAsset)?;
    let addresses = tokio::net::lookup_host((host, 443))
        .await
        .map_err(|_| Error::Unavailable)?
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !crate::link_previews::public_address(address.ip()))
    {
        return Err(Error::InvalidAsset);
    }
    downloads::client_builder()
        .no_proxy()
        .tls_backend_native()
        .resolve(host, SocketAddr::new(addresses[0].ip(), 443))
        .connect_timeout(Duration::from_secs(connect))
        .read_timeout(Duration::from_secs(read))
        .build()
        .map_err(|_| Error::Unavailable)
}
async fn search(context: Context, query: String, offset: u32) -> Result<Page> {
    if query.chars().count() > 50 {
        return Err(Error::InvalidQuery);
    }
    if offset > if query.is_empty() { 499 } else { 4999 } {
        return Err(Error::NoMoreResults);
    }
    if context.owner.key_saving.load(Ordering::Acquire) {
        return Err(Error::Busy);
    }
    let key = crate::credentials::load_service(&context.owner.account, SERVICE)
        .await
        .map_err(key_error)?
        .ok_or(Error::KeyRequired)?;
    if !valid_key(&key) {
        return Err(Error::InvalidKey);
    }
    context.check()?;
    let material = Zeroizing::new(format!("{}\n{query}\n{offset}", key.as_str()));
    let cache_id = hash(material.as_bytes());
    let owner = context.owner.clone();
    let lookup = cache_id.clone();
    let cached = worker(move || {
        let _gate = owner.gate.lock().unwrap();
        let db = open_db(&owner)?;
        let body = db
            .query_row(
                "SELECT body FROM pages WHERE id=? AND expires>?",
                params![lookup, seconds()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        Ok(body.and_then(|body| serde_json::from_str::<Page>(&body).ok()))
    })
    .await
    .unwrap_or(None);
    if let Some(page) = cached {
        return decorate(context, page).await;
    }
    let _slot = SEARCHES.acquire().await.map_err(|_| Error::Unavailable)?;
    context.check()?;
    let endpoint = if query.is_empty() {
        "trending"
    } else {
        "search"
    };
    let mut url = Url::parse(&format!("https://api.giphy.com/v1/gifs/{endpoint}")).unwrap();
    {
        let mut values = url.query_pairs_mut();
        values
            .append_pair("api_key", &key)
            .append_pair("limit", "25")
            .append_pair("offset", &offset.to_string())
            .append_pair("rating", "r");
        if !query.is_empty() {
            values.append_pair("q", &query);
        }
    }
    let fetched = async {
        let client = provider_client(&url, 10, 15).await?;
        context.check()?;
        let mut response = client
            .get(url)
            .header(header::USER_AGENT, "Monogram-GIFs/1")
            .header(header::ACCEPT, "application/json")
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Error::KeyRejected),
            429 => return Err(Error::RateLimited),
            _ => return Err(Error::Unavailable),
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
            || response
                .content_length()
                .is_some_and(|size| size > JSON_LIMIT as u64)
        {
            return Err(Error::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            context.check()?;
            if bytes.len() + chunk.len() > JSON_LIMIT {
                return Err(Error::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
            context.request.update(
                "Searching GIFs",
                bytes.len() as u64,
                response.content_length(),
            );
        }
        parse_page(&bytes, query.is_empty())
    };
    let page = tokio::time::timeout(Duration::from_secs(30), fetched)
        .await
        .map_err(|_| Error::Unavailable)??;
    context.check()?;
    let owner = context.owner.clone();
    let stored = page.clone();
    let save = context.clone();
    let _=worker(move||{let _gate=owner.gate.lock().unwrap();save.check()?;let db=open_db(&owner)?;let body=serde_json::to_string(&stored).map_err(|_|Error::Storage)?;
        db.execute("INSERT INTO pages VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body,expires=excluded.expires",params![cache_id,body,seconds()+CACHE_SECONDS]).map_err(|_|Error::Storage)?;
        db.execute("DELETE FROM pages WHERE expires<=? OR id NOT IN(SELECT id FROM pages ORDER BY expires DESC,id LIMIT 64)",[seconds()]).map_err(|_|Error::Storage)?;Ok(())}).await;
    decorate(context, page).await
}
async fn decorate(context: Context, mut page: Page) -> Result<Page> {
    context.check()?;
    let owner = context.owner.clone();
    let fallback = page.clone();
    let fence = context.clone();
    let result = worker(move || {
        context.check()?;
        let _gate = owner.gate.lock().unwrap();
        let snapshot = read_snapshot(&owner)?;
        let saved = snapshot
            .favorites
            .into_iter()
            .chain(snapshot.recents)
            .filter_map(|item| Some((library_id(&item).ok()?, item)))
            .collect::<HashMap<_, _>>();
        for item in &mut page.items {
            if let Ok(id) = library_id(item)
                && let Some(local) = saved.get(&id)
            {
                item.favorite = local.favorite;
                item.recent = local.recent;
                if local.original.path.is_some() {
                    item.original = local.original.clone();
                }
                if local.preview.path.is_some() {
                    item.preview = local.preview.clone();
                }
            }
        }
        Ok(page)
    })
    .await;
    fence.check()?;
    Ok(result.unwrap_or(fallback))
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn temporary(dir: &Path) -> Result<(Temporary, fs::File)> {
    private_dir(dir)?;
    let path = Temporary(dir.join(format!(".{}.part", uuid::Uuid::new_v4())));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path.0)
        .map_err(|_| Error::Storage)?;
    Ok((path, file))
}
fn gif_structure(path: &Path, context: &Context) -> Result<(u32, u32)> {
    let mut file = std::io::BufReader::with_capacity(
        65536,
        fs::File::open(path).map_err(|_| Error::MissingFile)?,
    );
    let mut header = [0; 13];
    file.read_exact(&mut header)
        .map_err(|_| Error::InvalidAsset)?;
    let (width, height) = gif_dimensions(&header)?;
    let mut count = 13u64;
    let mut last = std::time::Instant::now();
    fn skip(file: &mut impl Read, length: usize, count: &mut u64) -> Result<()> {
        let mut bytes = [0; 768];
        file.read_exact(&mut bytes[..length])
            .map_err(|_| Error::InvalidAsset)?;
        *count += length as u64;
        Ok(())
    }
    fn blocks(
        file: &mut impl Read,
        context: &Context,
        count: &mut u64,
        last: &mut std::time::Instant,
    ) -> Result<()> {
        loop {
            context.check()?;
            let mut length = [0];
            file.read_exact(&mut length)
                .map_err(|_| Error::InvalidAsset)?;
            *count += 1;
            if length[0] == 0 {
                return Ok(());
            }
            skip(file, length[0] as usize, count)?;
            if last.elapsed() > Duration::from_millis(80) {
                context.request.update("Checking GIF", *count, None);
                *last = std::time::Instant::now();
            }
        }
    }
    if header[10] & 0x80 != 0 {
        skip(
            &mut file,
            3usize << (usize::from(header[10] & 7) + 1),
            &mut count,
        )?;
    }
    let mut frames = 0u64;
    loop {
        context.check()?;
        let mut tag = [0];
        file.read_exact(&mut tag).map_err(|_| Error::InvalidAsset)?;
        count += 1;
        match tag[0] {
            0x21 => {
                skip(&mut file, 1, &mut count)?;
                blocks(&mut file, context, &mut count, &mut last)?;
            }
            0x2c => {
                let mut image = [0; 9];
                file.read_exact(&mut image)
                    .map_err(|_| Error::InvalidAsset)?;
                count += 9;
                let x = u16::from_le_bytes([image[0], image[1]]) as u32;
                let y = u16::from_le_bytes([image[2], image[3]]) as u32;
                let w = u16::from_le_bytes([image[4], image[5]]) as u32;
                let h = u16::from_le_bytes([image[6], image[7]]) as u32;
                if w == 0
                    || h == 0
                    || w > 16384
                    || h > 16384
                    || u64::from(w) * u64::from(h) > 64_000_000
                    || x + w > width
                    || y + h > height
                {
                    return Err(Error::TooLarge);
                }
                if image[8] & 0x80 != 0 {
                    skip(
                        &mut file,
                        3usize << (usize::from(image[8] & 7) + 1),
                        &mut count,
                    )?;
                }
                let mut code = [0];
                file.read_exact(&mut code)
                    .map_err(|_| Error::InvalidAsset)?;
                count += 1;
                if !(2..=8).contains(&code[0]) {
                    return Err(Error::InvalidAsset);
                }
                blocks(&mut file, context, &mut count, &mut last)?;
                frames += 1;
            }
            0x3b => {
                return if frames > 0 {
                    Ok((width, height))
                } else {
                    Err(Error::InvalidAsset)
                };
            }
            _ => return Err(Error::InvalidAsset),
        }
    }
}
fn inspect(path: &Path, context: &Context, decode: bool) -> Result<(u32, u32)> {
    use std::io::{Seek, SeekFrom};
    context.check()?;
    let mut input = fs::File::open(path).map_err(|_| Error::MissingFile)?;
    let mut header = [0; 13];
    input
        .read_exact(&mut header)
        .map_err(|_| Error::InvalidAsset)?;
    let dimensions = gif_dimensions(&header)?;
    gif_structure(path, context)?;
    input
        .seek(SeekFrom::End(-1))
        .map_err(|_| Error::InvalidAsset)?;
    let mut trailer = [0];
    input
        .read_exact(&mut trailer)
        .map_err(|_| Error::InvalidAsset)?;
    if trailer[0] != 0x3b {
        return Err(Error::InvalidAsset);
    }
    if decode {
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| Error::InvalidAsset)?;
        let mut decoder = crate::image_loader::Decoder::open(
            downloads::VerifiedFile(input),
            &context.request.decoder,
        )
        .map_err(|_| Error::InvalidAsset)?;
        if decoder.mime != "image/gif" || dimensions != (decoder.width, decoder.height) {
            return Err(Error::InvalidAsset);
        }
        let frame = decoder.frame(320).map_err(|_| Error::InvalidAsset)?;
        if frame.texture.width() > 320 || frame.texture.height() > 320 {
            return Err(Error::InvalidAsset);
        }
    }
    context.check()?;
    Ok(dimensions)
}
fn digest_file(context: &Context, path: &Path, limit: u64) -> Result<String> {
    context.check()?;
    let mut file = fs::File::open(path).map_err(|_| Error::MissingFile)?;
    let size = file.metadata().map_err(|_| Error::Storage)?.len();
    if size == 0 || size > limit {
        return Err(Error::TooLarge);
    }
    let mut hasher = Hasher::new(MessageDigest::sha256()).map_err(|_| Error::Storage)?;
    let mut buffer = [0; 65536];
    let mut total = 0;
    let mut last = std::time::Instant::now();
    loop {
        context.check()?;
        let n = file.read(&mut buffer).map_err(|_| Error::Storage)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(Error::TooLarge);
        }
        hasher.update(&buffer[..n]).map_err(|_| Error::Storage)?;
        if last.elapsed() > Duration::from_millis(80) {
            context.request.update("Checking GIF", total, Some(size));
            last = std::time::Instant::now();
        }
    }
    if total != size {
        return Err(Error::InvalidAsset);
    }
    Ok(hex(&hasher.finish().map_err(|_| Error::Storage)?))
}
fn finish_file(
    context: &Context,
    part: Temporary,
    size: u64,
    sha: String,
    url: Option<String>,
    preview: bool,
    decode: bool,
) -> Result<GifAsset> {
    context.request.update("Preparing GIF", size, Some(size));
    let (width, height) = inspect(&part.0, context, decode)?;
    let dir = if preview {
        &context.owner.cache
    } else {
        &context.owner.path.join("originals")
    };
    let destination = dir.join(format!("{sha}.gif"));
    {
        let _gate = context.owner.gate.lock().unwrap();
        context.check()?;
        fs::rename(&part.0, &destination).map_err(|_| Error::Storage)?;
    }
    let asset = GifAsset {
        url,
        path: Some(destination),
        width,
        height,
        size,
        sha256: Some(sha),
    };
    context.check()?;
    Ok(asset)
}
enum Chunk {
    Data(Vec<u8>),
    Complete,
}
async fn asset(context: Context, source: GifAsset, preview: bool) -> Result<GifAsset> {
    context.check()?;
    let limit = if preview {
        PREVIEW_LIMIT
    } else {
        crate::file_share::MAX
    };
    let local_context = context.clone();
    let local_source = source.clone();
    let ready = worker(move || {
        local_context.check()?;
        let owner = &local_context.owner;
        let found = local_asset(owner, &local_source, limit).or_else(|| {
            let address = local_source.url.as_ref()?;
            if local_source.size > limit {
                return None;
            }
            let _gate = owner.gate.lock().unwrap();
            let db = open_db(owner).ok()?;
            let key = format!(
                "{}:{}",
                if preview { "preview" } else { "original" },
                address
            );
            let body: Option<String> = db
                .query_row("SELECT body FROM files WHERE id=?", [key], |row| row.get(0))
                .optional()
                .ok()
                .flatten();
            let candidate: GifAsset = serde_json::from_str(&body?).ok()?;
            if local_source.size > 0 && local_source.size != candidate.size {
                return None;
            }
            local_asset(owner, &candidate, limit)
        });
        if let Some(found) = found {
            if !preview
                && digest_file(&local_context, found.path.as_ref().unwrap(), limit)?
                    != found.sha256.clone().unwrap_or_default()
            {
                return Ok(None);
            }
            return Ok(Some(found));
        }
        Ok(None)
    })
    .await?;
    if let Some(ready) = ready {
        return Ok(ready);
    }
    if source.size > limit {
        return Err(Error::TooLarge);
    }
    let url = asset_url(source.url.as_deref().ok_or(Error::MissingFile)?)?;
    let _slot = DOWNLOADS.acquire().await.map_err(|_| Error::Unavailable)?;
    context.check()?;
    let download = download(context.clone(), source, url, preview, limit);
    match tokio::time::timeout(Duration::from_secs(60), download).await {
        Ok(result) => result,
        Err(_) => {
            context.request.cancel();
            Err(Error::Unavailable)
        }
    }
}
async fn download(
    context: Context,
    source: GifAsset,
    mut url: Url,
    preview: bool,
    limit: u64,
) -> Result<GifAsset> {
    let canonical = url.to_string();
    for redirect in 0..=5 {
        context.check()?;
        let client = provider_client(&url, 5, 15).await?;
        let mut response = client
            .get(url.clone())
            .header(header::USER_AGENT, "Monogram-GIFs/1")
            .header(header::ACCEPT, "image/gif, application/octet-stream")
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if [301, 302, 303, 307, 308].contains(&response.status().as_u16()) {
            if redirect == 5 {
                return Err(Error::InvalidAsset);
            }
            let next = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(Error::InvalidAsset)?;
            url = asset_url(url.join(next).map_err(|_| Error::InvalidAsset)?.as_str())?;
            continue;
        }
        if response.status().as_u16() != 200 {
            return Err(Error::Unavailable);
        }
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !(mime == "image/gif"
            || mime == "application/octet-stream"
                && url.path().to_ascii_lowercase().ends_with(".gif"))
            || response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            return Err(Error::InvalidAsset);
        }
        let expected = response.content_length();
        if expected.is_some_and(|size| size == 0 || size > limit) {
            return Err(Error::TooLarge);
        }
        if source.size > 0 && expected.is_some_and(|size| size != source.size) {
            return Err(Error::InvalidAsset);
        }
        let (send, mut receive) = tokio::sync::mpsc::channel::<Chunk>(2);
        let writing = context.clone();
        let address = canonical.clone();
        let writer = executor().spawn_blocking(move || {
            writing.check()?;
            let dir = if preview {
                writing.owner.cache.clone()
            } else {
                writing.owner.path.join("originals")
            };
            let (part, mut file) = {
                let _gate = writing.owner.gate.lock().unwrap();
                writing.check()?;
                temporary(&dir)?
            };
            let mut hasher = Hasher::new(MessageDigest::sha256()).map_err(|_| Error::Storage)?;
            let mut count = 0;
            let mut complete = false;
            let mut last = std::time::Instant::now();
            while let Some(chunk) = receive.blocking_recv() {
                writing.check()?;
                match chunk {
                    Chunk::Data(bytes) => {
                        count += bytes.len() as u64;
                        if count > limit {
                            return Err(Error::TooLarge);
                        }
                        file.write_all(&bytes).map_err(|_| Error::Storage)?;
                        hasher.update(&bytes).map_err(|_| Error::Storage)?;
                        if last.elapsed() > Duration::from_millis(80) {
                            writing.request.update(
                                if preview {
                                    "Loading GIF preview"
                                } else {
                                    "Downloading GIF"
                                },
                                count,
                                expected,
                            );
                            last = std::time::Instant::now();
                        }
                    }
                    Chunk::Complete => {
                        complete = true;
                        break;
                    }
                }
            }
            drop(file);
            if !complete {
                return Err(Error::Cancelled);
            }
            if count == 0
                || expected.is_some_and(|size| size != count)
                || source.size > 0 && source.size != count
            {
                return Err(Error::InvalidAsset);
            }
            let sha = hex(&hasher.finish().map_err(|_| Error::Storage)?);
            finish_file(&writing, part, count, sha, Some(address), preview, true)
        });
        let mut count = 0;
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            context.check()?;
            count += chunk.len() as u64;
            if count > limit {
                return Err(Error::TooLarge);
            }
            send.send(Chunk::Data(chunk.to_vec()))
                .await
                .map_err(|_| Error::InvalidAsset)?;
        }
        let _decoder = DECODERS.acquire().await.map_err(|_| Error::Unavailable)?;
        send.send(Chunk::Complete)
            .await
            .map_err(|_| Error::InvalidAsset)?;
        drop(send);
        let result = writer.await.map_err(|_| Error::Unavailable)??;
        context.check()?;
        let owner = context.owner.clone();
        let recorded = result.clone();
        let key = format!(
            "{}:{}",
            if preview { "preview" } else { "original" },
            canonical
        );
        let commit = context.clone();
        worker(move||{let _gate=owner.gate.lock().unwrap();commit.check()?;let db=open_db(&owner)?;
            db.execute("INSERT INTO files VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body,used=excluded.used",params![key,serde_json::to_string(&recorded).map_err(|_|Error::Storage)?,seconds()]).map_err(|_|Error::Storage)?;
            db.execute("DELETE FROM files WHERE id NOT IN(SELECT id FROM files ORDER BY used DESC,id LIMIT 512)",[]).map_err(|_|Error::Storage)?;
            trim_previews(&owner.cache);Ok(())}).await?;
        return Ok(result);
    }
    Err(Error::InvalidAsset)
}
async fn preview(context: Context, item: GifItem) -> Result<GifAsset> {
    let owner = context.owner.clone();
    let preferred = item.preview.clone();
    let fallback = item.original.clone();
    if let Some(local) = worker(move || {
        check_owner(&owner)?;
        Ok(local_asset(&owner, &preferred, PREVIEW_LIMIT)
            .or_else(|| local_asset(&owner, &fallback, PREVIEW_LIMIT)))
    })
    .await?
    {
        return Ok(local);
    }
    asset(context, item.preview, true).await
}
fn trim_previews(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "gif") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                metadata.modified().unwrap_or(UNIX_EPOCH),
                metadata.len(),
                path,
            ))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut bytes = 0;
    for (index, (_, size, path)) in files.into_iter().enumerate() {
        bytes += size;
        if index >= 128 || bytes > 64 * 1024 * 1024 {
            let _ = fs::remove_file(path);
        }
    }
}
fn copy_into(
    context: &Context,
    source: &Path,
    directory: &Path,
    limit: u64,
    expected: Option<&str>,
    decode: bool,
) -> Result<GifAsset> {
    context.check()?;
    let mut input = fs::File::open(source).map_err(|_| Error::MissingFile)?;
    let size = input.metadata().map_err(|_| Error::Storage)?.len();
    if size == 0 || size > limit {
        return Err(Error::TooLarge);
    }
    let (part, mut output) = {
        let _gate = context.owner.gate.lock().unwrap();
        context.check()?;
        temporary(directory)?
    };
    let mut sha = Hasher::new(MessageDigest::sha256()).map_err(|_| Error::Storage)?;
    let mut buffer = [0; 65536];
    let mut count = 0;
    let mut last = std::time::Instant::now();
    loop {
        context.check()?;
        let n = input.read(&mut buffer).map_err(|_| Error::Storage)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > limit {
            return Err(Error::TooLarge);
        }
        output.write_all(&buffer[..n]).map_err(|_| Error::Storage)?;
        sha.update(&buffer[..n]).map_err(|_| Error::Storage)?;
        if last.elapsed() > Duration::from_millis(80) {
            context.request.update("Saving GIF", count, Some(size));
            last = std::time::Instant::now();
        }
    }
    drop(output);
    if count != size {
        return Err(Error::InvalidAsset);
    }
    let sha = hex(&sha.finish().map_err(|_| Error::Storage)?);
    if expected.is_some_and(|expected| expected != sha) {
        return Err(Error::InvalidAsset);
    }
    let (width, height) = inspect(&part.0, context, decode)?;
    let destination = directory.join(format!("{sha}.gif"));
    {
        let _gate = context.owner.gate.lock().unwrap();
        context.check()?;
        fs::rename(&part.0, &destination).map_err(|_| Error::Storage)?;
    }
    Ok(GifAsset {
        url: None,
        path: Some(destination),
        width,
        height,
        size,
        sha256: Some(sha),
    })
}
async fn import_local(context: Context, path: PathBuf, title: String) -> Result<GifItem> {
    let copy = context.clone();
    let _decoder = DECODERS.acquire().await.map_err(|_| Error::Unavailable)?;
    let original = worker(move || {
        copy_into(
            &copy,
            &path,
            &copy.owner.path.join("originals"),
            crate::file_share::MAX,
            None,
            true,
        )
    })
    .await?;
    context.check()?;
    let item = GifItem {
        id: format!("local-gif-{}", original.sha256.as_deref().unwrap()),
        title: title
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect(),
        preview: original.clone(),
        original,
        favorite: true,
        recent: false,
    };
    let owner = context.owner.clone();
    let commit = context.clone();
    worker(move || {
        let _gate = owner.gate.lock().unwrap();
        commit.check()?;
        update_library(&owner, item, Some(true), false)
    })
    .await
}
async fn change_library(
    context: Context,
    mut item: GifItem,
    favorite: Option<bool>,
    remove_recent: bool,
) -> Result<GifItem> {
    context.check()?;
    if !remove_recent && favorite != Some(false) {
        item.original = asset(context.clone(), item.original.clone(), false).await?;
        if let Ok(ready) = preview(context.clone(), item.clone()).await {
            item.preview = ready;
        }
        if let Some(path) = item
            .preview
            .path
            .clone()
            .filter(|path| path.starts_with(&context.owner.cache))
        {
            let copy = context.clone();
            let expected = item.preview.sha256.clone();
            let url = item.preview.url.clone();
            if let Ok(mut saved) = worker(move || {
                copy_into(
                    &copy,
                    &path,
                    &copy.owner.path.join("previews"),
                    PREVIEW_LIMIT,
                    expected.as_deref(),
                    false,
                )
            })
            .await
            {
                saved.url = url;
                item.preview = saved;
            }
        }
    }
    context.check()?;
    let owner = context.owner.clone();
    worker(move || {
        let _gate = owner.gate.lock().unwrap();
        context.check()?;
        update_library(&owner, item, favorite, remove_recent)
    })
    .await
}
fn update_library(
    owner: &Owner,
    mut item: GifItem,
    favorite: Option<bool>,
    remove_recent: bool,
) -> Result<GifItem> {
    let id = library_id(&item)?;
    let db = open_db(owner)?;
    let transaction = db.unchecked_transaction().map_err(|_| Error::Storage)?;
    let flags: Option<(bool, bool, String)> = transaction
        .query_row(
            "SELECT favorite,recent,body FROM library WHERE id=?",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let (old_favorite, old_recent) = if let Some((favorite, recent, body)) = flags {
        let previous: GifItem = serde_json::from_str(&body).map_err(|_| Error::Storage)?;
        if item.original.path.is_none() {
            item.original = previous.original;
        }
        if item.preview.path.is_none() {
            item.preview = previous.preview;
        }
        (favorite, recent)
    } else {
        (false, false)
    };
    item.favorite = favorite.unwrap_or(old_favorite);
    item.recent = if favorite.is_none() {
        !remove_recent
    } else {
        old_recent
    };
    if item.favorite || item.recent {
        let position: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(position),0)+1 FROM library",
                [],
                |row| row.get(0),
            )
            .map_err(|_| Error::Storage)?;
        let body = serde_json::to_string(&item).map_err(|_| Error::Storage)?;
        transaction.execute("INSERT INTO library VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body,favorite=excluded.favorite,recent=excluded.recent,position=excluded.position",params![id,body,item.favorite,item.recent,position]).map_err(|_|Error::Storage)?;
        let size: i64 = transaction
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(body AS BLOB))),0) FROM library",
                [],
                |row| row.get(0),
            )
            .map_err(|_| Error::Storage)?;
        if size > JSON_LIMIT as i64 {
            return Err(Error::LibraryFull);
        }
    } else {
        transaction
            .execute("DELETE FROM library WHERE id=?", [id])
            .map_err(|_| Error::Storage)?;
    }
    check_owner(owner)?;
    transaction.commit().map_err(|_| Error::Storage)?;
    Ok(item)
}
/// Account removal. The shared credentials account purge separately removes provider secrets.
pub async fn forget(account: &str) -> Result<()> {
    if crate::protocol::bare(account).ok().as_deref() != Some(account) {
        return Err(Error::InvalidAccount);
    }
    let _lifecycle = LIFECYCLE.lock().await;
    let account = account.to_owned();
    worker(move || {
        let (path, cache) = roots(&account)?;
        let owner = OWNERS
            .lock()
            .unwrap()
            .remove(&path)
            .and_then(|owner| owner.upgrade());
        if let Some(owner) = &owner {
            owner.invalidate();
        }
        let _gate = owner.as_ref().map(|owner| owner.gate.lock().unwrap());
        for directory in [path, cache] {
            if directory.exists() {
                fs::remove_dir_all(directory).map_err(|_| Error::Storage)?;
            }
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_giphy_parser_scope_offline_library_and_cancellation() {
        assert!(valid_key("a-valid_key"));
        assert!(!valid_key("bad key"));
        assert!(!valid_key(&"a".repeat(257)));
        for unsafe_url in [
            "http://media.giphy.com/a.gif",
            "https://giphy.com.evil.org/a.gif",
            "https://evilgiphy.com/a.gif",
            "https://user@media.giphy.com/a.gif",
            "https://media.giphy.com:444/a.gif",
            "https://media.giphy.com/a.gif#key",
            "https://media.giphy.com/a.gif?api_key=private",
        ] {
            assert!(!valid_asset_url(unsafe_url));
        }
        let body=br#"{"data":[{"id":"fixture","title":"GIF","images":{"original":{"url":"https://media.giphy.com/a.gif","width":"1","height":"1","size":"35"},"fixed_width":{"url":"https://media.giphy.com/b.gif","width":"1","height":"1","size":"35"}}}],"pagination":{"offset":0,"count":25,"total_count":1000}}"#;
        let page = parse_page(body, true).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.next_offset, Some(25));
        assert_eq!(page.items[0].id, "giphy-fixture");
        let bad = String::from_utf8(body.to_vec())
            .unwrap()
            .replace("media.giphy.com/a.gif", "evil.org/a.gif");
        assert!(parse_page(bad.as_bytes(), true).unwrap().items.is_empty());
        let last = String::from_utf8(body.to_vec())
            .unwrap()
            .replace("\"offset\":0", "\"offset\":499");
        assert!(
            parse_page(last.as_bytes(), true)
                .unwrap()
                .next_offset
                .is_none()
        );
        assert!(parse_page(&vec![0; JSON_LIMIT + 1], false).is_err());
        let root =
            std::env::temp_dir().join(format!("monogram-gif-check-{}", uuid::Uuid::new_v4()));
        let path = root.join("data");
        let cache = root.join("cache");
        private_dir(&path).unwrap();
        private_dir(&cache).unwrap();
        private_dir(&path.join("originals")).unwrap();
        let (changed, _) = tokio::sync::watch::channel(0);
        let owner = Arc::new(Owner {
            account: "gif-check@example.invalid".into(),
            path: path.clone(),
            cache,
            gate: Mutex::new(()),
            valid: AtomicBool::new(true),
            key_saving: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            changed,
        });
        init_schema(&open_db(&owner).unwrap()).unwrap();
        let bytes=b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0\x3b";
        let sha = hash(bytes);
        let local = path.join("originals").join(format!("{sha}.gif"));
        fs::write(&local, bytes).unwrap();
        let mut item = page.items[0].clone();
        item.original.path = Some(local.clone());
        item.original.size = bytes.len() as u64;
        item.original.sha256 = Some(sha);
        item.preview = item.original.clone();
        let request = Request::new();
        let context = Context {
            owner: owner.clone(),
            request: request.clone(),
            search: None,
        };
        assert_eq!(gif_structure(&local, &context).unwrap(), (1, 1));
        item = update_library(&owner, item, Some(true), false).unwrap();
        item = update_library(&owner, item, None, false).unwrap();
        assert!(item.favorite && item.recent);
        let mut from_search = page.items[0].clone();
        from_search.original.path = None;
        from_search.preview.path = None;
        update_library(&owner, from_search, Some(false), false).unwrap();
        let snapshot = read_snapshot(&owner).unwrap();
        assert!(snapshot.favorites.is_empty());
        assert_eq!(snapshot.recents.len(), 1);
        assert_eq!(snapshot.recents[0].original.path.as_ref(), Some(&local));
        assert!(snapshot.recents[0].original.attachment().is_some());
        let searching = Context {
            owner: owner.clone(),
            request: Request::new(),
            search: Some(0),
        };
        owner.bump();
        assert_eq!(searching.check(), Err(Error::Cancelled));
        assert!(
            context.check().is_ok(),
            "key changes do not disable saved local files"
        );
        request.cancel();
        assert_eq!(context.check(), Err(Error::Cancelled));
        owner.invalidate();
        assert_eq!(context.check(), Err(Error::InvalidAccount));
        fs::remove_dir_all(root).unwrap();
    }
}
