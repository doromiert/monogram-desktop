//! Account-private packs; XMSP v1 matches Monogram mobile's GPL-3.0-or-later archive profile.
use gtk::{gio, glib, prelude::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self("Pack file could not be read or saved.")
    }
}
impl From<zip::result::ZipError> for Error {
    fn from(_: zip::result::ZipError) -> Self {
        Self("Invalid or unsupported pack archive.")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self("Invalid pack metadata; the original library was preserved.")
    }
}
const FILE_LIMIT: usize = 20 * 1024 * 1024;
const TOTAL_LIMIT: usize = 100 * 1024 * 1024;
const MANIFEST_LIMIT: usize = 4 * 1024 * 1024;
const ARCHIVE_LIMIT: u64 = 105 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Stickers,
    Emoji,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    #[serde(default)] pub sync_id: String,
    pub id: i64,
    pub name: String,
    pub emoji: String,
    pub mime: String,
    pub file: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub remote_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pack {
    #[serde(default)] pub sync_id: String,
    pub id: i64,
    pub name: String,
    pub source: String,
    pub kind: Kind,
    pub installed: bool,
    pub private_pack: bool,
    pub connected: bool,
    pub auto_sync: bool,
    pub last_sync: i64,
    #[serde(default)]
    pub last_attempt: i64,
    pub sync_error: String,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub packs: Vec<Pack>,
    #[serde(default)]
    pub favorites: Vec<i64>,
    #[serde(default)]
    pub recent: Vec<i64>,
}
#[derive(Clone, Debug)]
pub struct Input {
    pub path: PathBuf,
    pub name: String,
    pub emoji: String,
}
#[derive(Clone, Debug)]
pub struct Asset {
    pub item: Item,
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub bytes: u64,
    pub active: bool,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct Job {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    decoder_cancel: gio::Cancellable,
}
impl Default for Job {
    fn default() -> Self {
        Self::new()
    }
}
impl Job {
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(Progress {
                stage: "Preparing pack".into(),
                active: true,
                ..Default::default()
            })),
            decoder_cancel: gio::Cancellable::new(),
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.decoder_cancel.cancel();
    }
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(Error("Operation cancelled."))
        } else {
            Ok(())
        }
    }
    fn update(&self, stage: &str, current: usize, total: usize, bytes: u64) {
        *self.progress.lock().unwrap() = Progress {
            stage: stage.into(),
            current,
            total,
            bytes,
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
            p.current = p.total;
        }
    }
}
struct Owner {
    account: String,
    path: PathBuf,
    gate: Mutex<()>,
    valid: AtomicBool,
}
static OWNERS: LazyLock<Mutex<HashMap<String, Weak<Owner>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
#[derive(Clone)]
pub struct Library {
    owner: Arc<Owner>,
}
#[derive(Clone, Debug)]
pub enum Mutation {
    Rename { pack: i64, name: String },
    RenameItem { pack: i64, item: i64, name: String },
    Tag { pack: i64, item: i64, emoji: String },
    Installed { pack: i64, value: bool },
    Private { pack: i64, value: bool },
    AutoSync { pack: i64, value: bool },
    Detach { pack: i64 },
    RemoveItem { pack: i64, item: i64 },
    MoveItem { pack: i64, from: usize, to: usize },
    Reorder { kind: Kind, ids: Vec<i64> },
    Favorite { item: i64, value: bool },
    Recent { item: i64 },
}
async fn worker<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|_| Error("Pack worker stopped."))?
    } else {
        gio::spawn_blocking(work)
            .await
            .map_err(|_| Error("Pack worker stopped."))?
    }
}
impl Library {
    pub async fn open(account: &str) -> Result<Self> {
        if crate::protocol::bare(account).ok().as_deref() != Some(account) {
            return Err(Error("Invalid account."));
        }
        let account = account.to_owned();
        worker(move || {
            let mut owners = OWNERS.lock().unwrap();
            if let Some(owner) = owners
                .get(&account)
                .and_then(Weak::upgrade)
                .filter(|o| o.valid.load(Ordering::SeqCst))
            {
                return Ok(Self { owner });
            }
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
                });
            let path = base
                .join("monogram-desktop/packs")
                .join(hash(account.as_bytes()));
            private_dir(&path)?;
            let owner = Arc::new(Owner {
                account: account.clone(),
                path,
                gate: Mutex::new(()),
                valid: AtomicBool::new(true),
            });
            let library = Self {
                owner: owner.clone(),
            };
            library.read()?;
            owners.insert(account, Arc::downgrade(&owner));
            Ok(library)
        })
        .await
    }
    pub fn account(&self) -> &str {
        &self.owner.account
    }
    pub fn cancel(&self) {
        self.owner.valid.store(false, Ordering::SeqCst);
    }
    fn check(&self, job: &Job) -> Result<()> {
        job.check()?;
        if !self.owner.valid.load(Ordering::SeqCst) {
            return Err(Error(
                "The account changed. Try again on the intended account.",
            ));
        }
        Ok(())
    }
    fn read(&self) -> Result<Snapshot> {
        if !self.owner.valid.load(Ordering::SeqCst) {
            return Err(Error("The account changed."));
        }
        let path = self.owner.path.join("packs.json");
        if !path.exists() {
            return Ok(Snapshot::default());
        }
        let bytes = read_limit(open_read(&path)?, MANIFEST_LIMIT, &Job::new())?;
        let value: Snapshot = json(&bytes)?;
        let mut ids = HashSet::new();
        for pack in &value.packs {
            valid_name(&pack.name)?;
            if !local_id(pack.id) || !ids.insert(pack.id) {
                return Err(Error(
                    "Invalid pack library; original files were preserved.",
                ));
            }
            for item in &pack.items {
                if !local_id(item.id) || !ids.insert(item.id) {
                    return Err(Error("Invalid pack item."));
                }
                validate_item(item)?;
            }
        }
        let item_ids: HashSet<_> = value
            .packs
            .iter()
            .flat_map(|p| p.items.iter().map(|i| i.id))
            .collect();
        if value
            .favorites
            .iter()
            .chain(value.recent.iter())
            .any(|id| !item_ids.contains(id))
        {
            return Err(Error("Invalid pack references."));
        }
        Ok(value)
    }
    fn write(&self, snapshot: &Snapshot) -> Result<()> {
        let data = serde_json::to_vec(snapshot)?;
        if data.len() > MANIFEST_LIMIT {
            return Err(Error("Pack library metadata exceeds 4 MiB."));
        }
        let temp = self
            .owner
            .path
            .join(format!(".{}.json", uuid::Uuid::new_v4()));
        let result: Result<()> = (|| {
            let mut f = new_file(&temp)?;
            f.write_all(&data)?;
            f.sync_all()?;
            if !self.owner.valid.load(Ordering::SeqCst) {
                return Err(Error("The account changed."));
            }
            fs::rename(&temp, self.owner.path.join("packs.json"))?;
            let _ = File::open(&self.owner.path).and_then(|dir| dir.sync_all());
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    pub async fn sync_tag(&self,id:i64)->Result<Pack>{
        let this=self.clone();worker(move||{let _gate=this.owner.gate.lock().unwrap();let mut snapshot=this.read()?;let pack=snapshot.packs.iter_mut().find(|p|p.id==id).ok_or(Error("Pack unavailable."))?;
        let mut changed=false;if pack.sync_id.is_empty(){pack.sync_id=uuid::Uuid::new_v4().to_string();changed=true;}for item in &mut pack.items{if item.sync_id.is_empty(){item.sync_id=uuid::Uuid::new_v4().to_string();changed=true;}}
        let result=pack.clone();if changed{this.write(&snapshot)?;}Ok(result)}).await
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let this = self.clone();
        worker(move || {
            let _guard = this.owner.gate.lock().unwrap();
            this.read()
        })
        .await
    }
    pub async fn asset(&self, id: i64) -> Result<Asset> {
        let this = self.clone();
        worker(move || {
            let _guard = this.owner.gate.lock().unwrap();
            let snapshot = this.read()?;
            let item = snapshot
                .packs
                .iter()
                .flat_map(|p| &p.items)
                .find(|i| i.id == id)
                .ok_or(Error("Artwork is no longer available."))?
                .clone();
            let path = this.owner.path.join(&item.file);
            let bytes = read_limit(open_read(&path)?, FILE_LIMIT, &Job::new())?;
            validate_media(&bytes, &item.mime, item.width, item.height, None)?;
            Ok(Asset {
                item,
                path,
                size: bytes.len() as u64,
                sha256: hash(&bytes),
            })
        })
        .await
    }
    pub async fn mutate(&self, mutation: Mutation) -> Result<Snapshot> {
        let this = self.clone();
        worker(move || {
            let _guard = this.owner.gate.lock().unwrap();
            let mut s = this.read()?;
            match mutation {
                Mutation::Favorite { item, value } => {
                    find_item(&s, item)?;
                    s.favorites.retain(|i| *i != item);
                    if value {
                        s.favorites.insert(0, item);
                    }
                }
                Mutation::Recent { item } => {
                    find_item(&s, item)?;
                    s.recent.retain(|i| *i != item);
                    s.recent.insert(0, item);
                    s.recent.truncate(200);
                }
                Mutation::Reorder { kind, ids } => {
                    let slots: Vec<_> = s
                        .packs
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.kind == kind && p.installed)
                        .map(|(i, _)| i)
                        .collect();
                    let mut ordered: Vec<_> = slots.iter().map(|i| s.packs[*i].clone()).collect();
                    let mut seen = HashSet::new();
                    let mut order: Vec<_> = ids
                        .into_iter()
                        .filter(|id| seen.insert(*id))
                        .filter_map(|id| {
                            ordered
                                .iter()
                                .position(|p| p.id == id)
                                .map(|i| ordered.remove(i))
                        })
                        .collect();
                    order.extend(ordered);
                    for (i, p) in slots.into_iter().zip(order) {
                        s.packs[i] = p;
                    }
                }
                other => {
                    let id = match &other {
                        Mutation::Rename { pack, .. }
                        | Mutation::RenameItem { pack, .. }
                        | Mutation::Tag { pack, .. }
                        | Mutation::Installed { pack, .. }
                        | Mutation::Private { pack, .. }
                        | Mutation::AutoSync { pack, .. }
                        | Mutation::Detach { pack }
                        | Mutation::RemoveItem { pack, .. }
                        | Mutation::MoveItem { pack, .. } => *pack,
                        _ => unreachable!(),
                    };
                    let index = s
                        .packs
                        .iter()
                        .position(|p| p.id == id)
                        .ok_or(Error("Pack is no longer available."))?;
                    let mut archived = None;
                    let p = &mut s.packs[index];
                    match other {
                        Mutation::Installed { value, .. } => p.installed = value,
                        Mutation::Private { value, .. } => p.private_pack = value,
                        Mutation::AutoSync { value, .. } => {
                            if !p.connected {
                                return Err(Error("Only connected Telegram packs can update."));
                            }
                            p.auto_sync = value;
                        }
                        Mutation::Detach { .. } => {
                            p.connected = false;
                            p.auto_sync = false;
                            p.source = format!("local:{}", uuid::Uuid::new_v4());
                            p.sync_error.clear();
                        }
                        Mutation::Rename { name, .. } => {
                            editable(p)?;
                            p.name = valid_name(&name)?;
                        }
                        Mutation::RenameItem { item, name, .. } => {
                            editable(p)?;
                            p.items
                                .iter_mut()
                                .find(|i| i.id == item)
                                .ok_or(Error("Item is no longer available."))?
                                .name = valid_name(&name)?;
                        }
                        Mutation::Tag { item, emoji, .. } => {
                            editable(p)?;
                            p.items
                                .iter_mut()
                                .find(|i| i.id == item)
                                .ok_or(Error("Item is no longer available."))?
                                .emoji = tag(&emoji)?;
                        }
                        Mutation::MoveItem { from, to, .. } => {
                            editable(p)?;
                            if from >= p.items.len() || to >= p.items.len() {
                                return Err(Error("Invalid item position."));
                            }
                            let item = p.items.remove(from);
                            p.items.insert(to, item);
                        }
                        Mutation::RemoveItem { item, .. } => {
                            editable(p)?;
                            let i = p
                                .items
                                .iter()
                                .position(|i| i.id == item)
                                .ok_or(Error("Item is no longer available."))?;
                            if p.items.len() == 1 {
                                p.installed = false;
                            } else {
                                archived = Some(p.items.remove(i));
                            }
                        }
                        _ => unreachable!(),
                    };
                    if let Some(item) = archived {
                        archive_items(&mut s, id, vec![item]);
                    }
                }
            }
            this.write(&s)?;
            Ok(s)
        })
        .await
    }
    pub async fn create(
        &self,
        name: String,
        kind: Kind,
        inputs: Vec<Input>,
        job: Job,
    ) -> Result<Pack> {
        self.prepare_items(None, None, name, kind, inputs, job)
            .await
    }
    pub async fn add_items(&self, pack: i64, inputs: Vec<Input>, job: Job) -> Result<Pack> {
        self.prepare_items(Some(pack), None, String::new(), Kind::Stickers, inputs, job)
            .await
    }
    pub async fn replace_item(&self, pack: i64, item: i64, input: Input, job: Job) -> Result<Pack> {
        self.prepare_items(
            Some(pack),
            Some(item),
            String::new(),
            Kind::Stickers,
            vec![input],
            job,
        )
        .await
    }
    async fn prepare_items(
        &self,
        target: Option<i64>,
        replace: Option<i64>,
        name: String,
        kind: Kind,
        inputs: Vec<Input>,
        job: Job,
    ) -> Result<Pack> {
        let this = self.clone();
        let done = job.clone();
        let result = worker(move || {
            this.check(&job)?;
            if inputs.is_empty() {
                return Err(Error("Select at least one file."));
            }
            if target.is_none() {
                valid_name(&name)?;
            }
            let mut stage = Staging::new(&this.owner.path)?;
            let mut items = Vec::new();
            let mut total = 0;
            for (i, input) in inputs.iter().enumerate() {
                this.check(&job)?;
                job.update("Preparing artwork", i, inputs.len(), total as u64);
                let bytes = read_limit(open_read(&input.path)?, FILE_LIMIT, &job)?;
                let (data, mime, w, h) = prepare_media(bytes, &job)?;
                total += data.len();
                if total > TOTAL_LIMIT {
                    return Err(Error("Pack exceeds 100 MiB."));
                }
                let file = format!("{}{}", uuid::Uuid::new_v4(), extension(&mime)?);
                stage.save(&file, &data)?;
                items.push(Item {
            sync_id: String::new(),
                    id: new_id(),
                    name: valid_name(&input.name)?,
                    emoji: tag(&input.emoji)?,
                    mime,
                    file,
                    width: w,
                    height: h,
                    remote_id: String::new(),
                });
            }
            let _guard = this.owner.gate.lock().unwrap();
            this.check(&job)?;
            let mut s = this.read()?;
            let pack = if let Some(id) = target {
                let index = s
                    .packs
                    .iter()
                    .position(|p| p.id == id)
                    .ok_or(Error("Pack is no longer available."))?;
                editable(&s.packs[index])?;
                let mut old = Vec::new();
                if let Some(id) = replace {
                    let slot = s.packs[index]
                        .items
                        .iter()
                        .position(|i| i.id == id)
                        .ok_or(Error("Item is no longer available."))?;
                    old.push(std::mem::replace(
                        &mut s.packs[index].items[slot],
                        items.remove(0),
                    ));
                } else {
                    s.packs[index].items.extend(items);
                }
                check_pack_bytes(&this.owner.path, &stage.path, &s.packs[index])?;
                let pack = s.packs[index].clone();
                archive_items(&mut s, target.unwrap(), old);
                pack
            } else {
                let pack = Pack {
            sync_id: String::new(),
                    id: new_id(),
                    name: valid_name(&name)?,
                    source: format!("local:{}", uuid::Uuid::new_v4()),
                    kind,
                    installed: true,
                    private_pack: true,
                    connected: false,
                    auto_sync: false,
                    last_sync: 0,
                    last_attempt: 0,
                    sync_error: String::new(),
                    items,
                };
                s.packs.insert(0, pack.clone());
                pack
            };
            stage.publish(&this.owner.path)?;
            this.check(&job)?;
            this.write(&s)?;
            stage.committed = true;
            Ok(pack)
        })
        .await;
        done.finish(&result);
        result
    }
    pub async fn import_xmsp(&self, path: PathBuf, job: Job) -> Result<Pack> {self.import_paired_archive(path,job,None).await}
    pub async fn import_sync_xmsp(&self,path:PathBuf,job:Job,id:String)->Result<Pack>{self.import_paired_archive(path,job,Some(id)).await}
    async fn import_paired_archive(&self, path: PathBuf, job: Job, sync_id:Option<String>) -> Result<Pack> {
        let this = self.clone();
        let done = job.clone();
        let result = worker(move || {
            this.check(&job)?;
            let mut stage = Staging::new(&this.owner.path)?;
            let mut pack = read_archive(&path, &mut stage, &job)?;
            let _guard = this.owner.gate.lock().unwrap();
            this.check(&job)?;
            let mut s = this.read()?;
            let existing=if let Some(id)=sync_id.as_ref(){
                if uuid::Uuid::parse_str(id).is_err() || pack.sync_id!=*id {return Err(Error("Invalid paired pack identity."));}
                s.packs.iter().find(|p|p.sync_id==*id).cloned()
            }else{pack.sync_id.clear();pack.source=format!("local:{}",uuid::Uuid::new_v4());pack.installed=true;pack.private_pack=true;pack.connected=false;pack.auto_sync=false;for item in &mut pack.items{item.sync_id.clear();}None};
            pack.id=existing.as_ref().map(|p|p.id).unwrap_or_else(new_id);
            for item in &mut pack.items{item.id=existing.as_ref().and_then(|p|p.items.iter().find(|old|!item.sync_id.is_empty()&&old.sync_id==item.sync_id)).map(|old|old.id).unwrap_or_else(new_id);}
            if let Some(old)=existing{let removed=old.items.into_iter().filter(|old|!pack.items.iter().any(|i|i.id==old.id)).collect();s.packs.retain(|p|p.id!=old.id);archive_items(&mut s,old.id,removed);}
            s.packs.insert(0,pack.clone());
            stage.publish(&this.owner.path)?;
            this.check(&job)?;
            this.write(&s)?;
            stage.committed = true;
            Ok(pack)
        })
        .await;
        done.finish(&result);
        result
    }
    pub async fn export_xmsp(&self,id:i64,job:Job)->Result<PathBuf>{self.export_archive(id,job,false).await}
    pub async fn export_sync_xmsp(&self,id:i64,job:Job)->Result<PathBuf>{self.export_archive(id,job,true).await}
    async fn export_archive(&self, id: i64, job: Job,sync:bool) -> Result<PathBuf> {
        let this = self.clone();
        let done = job.clone();
        let result = worker(move || {
            let _guard = this.owner.gate.lock().unwrap();
            this.check(&job)?;
            let s = this.read()?;
            let pack = s
                .packs
                .iter()
                .find(|p| p.id == id)
                .ok_or(Error("Pack is no longer available."))?;
            let output = this
                .owner
                .path
                .join(format!("{}.xmsp", uuid::Uuid::new_v4()));
            let mut pack=pack.clone();if !sync{pack.sync_id.clear();for item in &mut pack.items{item.sync_id.clear();}}
            let result = write_archive(&pack, &this.owner.path, &output, &job);
            if result.is_err() {
                let _ = fs::remove_file(&output);
            }
            result?;
            this.check(&job)?;
            Ok(output)
        })
        .await;
        done.finish(&result);
        result
    }
    pub async fn inline_asset(&self, id: i64) -> Result<InlineAsset> {
        let asset = self.asset(id).await?;
        worker(move || {
            let bytes = read_limit(open_read(&asset.path)?, INLINE_LIMIT, &Job::new())?;
            InlineAsset::new(asset.item.mime, bytes)
        })
        .await
    }
    pub async fn due_updates(&self) -> Result<Vec<i64>> {
        Ok(self
            .snapshot()
            .await?
            .packs
            .into_iter()
            .filter(|p| {
                p.installed
                    && p.connected
                    && p.auto_sync
                    && chrono::Utc::now().timestamp() - p.last_attempt.max(p.last_sync)
                        >= 6 * 60 * 60
            })
            .map(|p| p.id)
            .collect())
    }
}
fn editable(pack: &Pack) -> Result<()> {
    if pack.connected {
        Err(Error("Detach this Telegram pack before editing it."))
    } else {
        Ok(())
    }
}
fn find_item(s: &Snapshot, id: i64) -> Result<&Item> {
    s.packs
        .iter()
        .flat_map(|p| &p.items)
        .find(|i| i.id == id)
        .ok_or(Error("Artwork is no longer available."))
}
fn archive_items(s: &mut Snapshot, parent: i64, items: Vec<Item>) {
    if items.is_empty() {
        return;
    }
    let source = format!("local:replaced:{parent}");
    if let Some(p) = s.packs.iter_mut().find(|p| p.source == source) {
        p.items.extend(items);
    } else {
        let kind = s
            .packs
            .iter()
            .find(|p| p.id == parent)
            .map(|p| p.kind)
            .unwrap_or(Kind::Stickers);
        s.packs.push(Pack {
            sync_id: String::new(),
            id: new_id(),
            name: "Retained artwork".into(),
            source,
            kind,
            installed: false,
            private_pack: true,
            connected: false,
            auto_sync: false,
            last_sync: 0,
            last_attempt: 0,
            sync_error: String::new(),
            items,
        });
    }
}
fn new_id() -> i64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    0x4d50_0000_0000_0000
        | ((u64::from_le_bytes(bytes[..8].try_into().unwrap()) & 0x0000_ffff_ffff_ffff).max(1)
            as i64)
}
fn local_id(id: i64) -> bool {
    id & !0x0000_ffff_ffff_ffff == 0x4d50_0000_0000_0000 && id & 0x0000_ffff_ffff_ffff != 0
}
fn valid_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.encode_utf16().count() > 100 || value.chars().any(char::is_control)
    {
        return Err(Error("Names must contain 1–100 characters."));
    }
    Ok(value.into())
}
fn tag(value: &str) -> Result<String> {
    if value.is_empty() {
        return Ok("🙂".into());
    }
    if value.encode_utf16().count() > 64 || value.chars().any(char::is_control) {
        return Err(Error("Emoji tags are too long."));
    }
    Ok(value.into())
}
fn extension(mime: &str) -> Result<&'static str> {
    match mime {
        "image/png" => Ok(".png"),
        "image/webp" => Ok(".webp"),
        "image/gif" => Ok(".gif"),
        "application/x-tgsticker" => Ok(".tgs"),
        "video/webm" => Ok(".webm"),
        _ => Err(Error("Unsupported pack artwork.")),
    }
}
fn hash(data: &[u8]) -> String {
    openssl::sha::sha256(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error("Invalid library directory."));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn open_read(path: &Path) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(Error("Select a regular file."));
    }
    Ok(f)
}
fn new_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}
fn read_limit(mut input: impl Read, limit: usize, job: &Job) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buf = [0; 16 * 1024];
    loop {
        job.check()?;
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if output.len() + n > limit {
            return Err(Error("File exceeds the safe byte limit."));
        }
        output.extend_from_slice(&buf[..n]);
    }
    if output.is_empty() {
        return Err(Error("Empty artwork or metadata."));
    }
    Ok(output)
}
fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(Error("Pack metadata must be UTF-8 without a BOM."));
    }
    let mut depth = 0;
    let mut quoted = false;
    let mut escape = false;
    for c in bytes {
        if quoted {
            if escape {
                escape = false;
            } else if *c == b'\\' {
                escape = true;
            } else if *c == b'"' {
                quoted = false;
            }
        } else {
            match c {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 16 {
                        return Err(Error("Pack metadata is too deeply nested."));
                    }
                }
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }
    }
    Ok(serde_json::from_slice(bytes)?)
}
fn validate_item(item: &Item) -> Result<()> {
    valid_name(&item.name)?;
    tag(&item.emoji)?;
    let ext = extension(&item.mime)?;
    if !(1..=512).contains(&item.width)
        || !(1..=512).contains(&item.height)
        || !item.file.ends_with(ext)
        || uuid::Uuid::parse_str(item.file.strip_suffix(ext).unwrap_or("")).is_err()
    {
        return Err(Error("Invalid pack artwork metadata."));
    }
    Ok(())
}
struct Staging {
    path: PathBuf,
    published: Vec<PathBuf>,
    committed: bool,
}
impl Staging {
    fn new(root: &Path) -> Result<Self> {
        let path = root.join(format!(".import-{}", uuid::Uuid::new_v4()));
        private_dir(&path)?;
        Ok(Self {
            path,
            published: Vec::new(),
            committed: false,
        })
    }
    fn save(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let mut file = new_file(&self.path.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
    fn publish(&mut self, root: &Path) -> Result<()> {
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let target = root.join(entry.file_name());
            if target.exists() {
                return Err(Error("Artwork identifier collision."));
            }
            fs::rename(entry.path(), &target)?;
            self.published.push(target);
        }
        Ok(())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.committed {
            for file in &self.published {
                let _ = fs::remove_file(file);
            }
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn check_pack_bytes(root: &Path, stage: &Path, pack: &Pack) -> Result<()> {
    let mut total = 0;
    let mut seen = HashSet::new();
    for i in &pack.items {
        if seen.insert(&i.file) {
            let path = if stage.join(&i.file).is_file() {
                stage.join(&i.file)
            } else {
                root.join(&i.file)
            };
            total += open_read(&path)?.metadata()?.len();
            if total > TOTAL_LIMIT as u64 {
                return Err(Error("Pack exceeds 100 MiB."));
            }
        }
    }
    Ok(())
}
fn validate_media(bytes: &[u8], mime: &str, w: u32, h: u32, job: Option<&Job>) -> Result<()> {
    if bytes.is_empty()
        || bytes.len() > FILE_LIMIT
        || !(1..=512).contains(&w)
        || !(1..=512).contains(&h)
    {
        return Err(Error("Invalid artwork size."));
    }
    match mime {
        "image/png" | "image/webp" => {
            let magic = if mime == "image/png" {
                bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            } else {
                bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
            };
            if !magic {
                return Err(Error("Invalid image signature."));
            }
            let cancel = job.map(|j| j.decoder_cancel.clone()).unwrap_or_default();
            let decoder = PackImage::open_bytes(&glib::Bytes::from_owned(bytes.to_vec()), &cancel)
                .map_err(|_| Error("Artwork image could not be decoded."))?;
            if decoder.mime != mime || decoder.width != w || decoder.height != h {
                return Err(Error("Artwork dimensions do not match the manifest."));
            }
        }
        "image/gif" => {
            if bytes.len() < 14 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
                return Err(Error("Invalid GIF."));
            }
            let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
            let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
            if width == 0
                || height == 0
                || width > 12000
                || height > 12000
                || u64::from(width) * u64::from(height) > 50_000_000
            {
                return Err(Error("GIF dimensions are too large."));
            }
        }
        "application/x-tgsticker" => {
            let data = read_limit(
                flate2::read::GzDecoder::new(bytes),
                2 * 1024 * 1024,
                &Job::new(),
            )?;
            let value: serde_json::Value = json(&data)?;
            let fps = value["fr"].as_f64().unwrap_or(0.);
            let start = value["ip"].as_f64().ok_or(Error("Invalid TGS."))?;
            let end = value["op"].as_f64().ok_or(Error("Invalid TGS."))?;
            if value["w"] != 512
                || value["h"] != 512
                || fps <= 0.
                || fps > 60.
                || end - start > fps * 3.
                || value.get("layers").is_none()
            {
                return Err(Error("Unsupported TGS animation."));
            }
        }
        "video/webm" => {
            if !bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
                return Err(Error("Invalid WebM."));
            }
        }
        _ => return Err(Error("Unsupported artwork format.")),
    }
    Ok(())
}
fn prepare_media(bytes: Vec<u8>, job: &Job) -> Result<(Vec<u8>, String, u32, u32)> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if bytes.len() < 10 {
            return Err(Error("Invalid GIF."));
        }
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        let scale = 512.0 / f64::from(w.max(h).max(512));
        let w = (f64::from(w) * scale).round().max(1.) as u32;
        let h = (f64::from(h) * scale).round().max(1.) as u32;
        validate_media(&bytes, "image/gif", w, h, Some(job))?;
        return Ok((bytes, "image/gif".into(), w, h));
    }
    for (magic, mime) in [
        (&[0x1f, 0x8b][..], "application/x-tgsticker"),
        (&[0x1a, 0x45, 0xdf, 0xa3][..], "video/webm"),
    ] {
        if bytes.starts_with(magic) {
            validate_media(&bytes, mime, 512, 512, Some(job))?;
            return Ok((bytes, mime.into(), 512, 512));
        }
    }
    let mut decoder = PackImage::open_bytes(&glib::Bytes::from_owned(bytes), &job.decoder_cancel)
        .map_err(|_| Error("Use a supported image, TGS animation or WebM video."))?;
    if decoder.width == 0
        || decoder.height == 0
        || decoder.width > 12000
        || decoder.height > 12000
        || u64::from(decoder.width) * u64::from(decoder.height) > 50_000_000
    {
        return Err(Error("Image dimensions are too large."));
    }
    let texture = decoder
        .frame(512)
        .map_err(|_| Error("Artwork could not be decoded."))?
        .texture;
    let bytes = texture.save_to_png_bytes().as_ref().to_vec();
    if bytes.len() > FILE_LIMIT {
        return Err(Error("Artwork exceeds 20 MiB."));
    }
    Ok((
        bytes,
        "image/png".into(),
        texture.width() as u32,
        texture.height() as u32,
    ))
}
#[derive(Clone,Serialize,Deserialize)]
struct PackSync {id:String,source:String,installed:bool,private_pack:bool,connected:bool,auto_sync:bool}
#[derive(Serialize,Deserialize)]
struct Manifest {
    #[serde(default)] sync: Option<PackSync>,
    format: String,
    version: u32,
    name: String,
    kind: Kind,
    items: Vec<ManifestItem>,
}
#[derive(Serialize, Deserialize)]
struct ManifestItem {
    id: String,
    name: String,
    emoji: String,
    mime: String,
    width: u32,
    height: u32,
    size: u64,
    sha256: String,
    path: String,
}
fn asset_path(path: &str) -> bool {
    let Some(p) = path.strip_prefix("assets/") else {
        return false;
    };
    let Some((hash, ext)) = p.split_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && ["png", "webp", "gif", "tgs", "webm"].contains(&ext)
}
fn read_archive(path: &Path, stage: &mut Staging, job: &Job) -> Result<Pack> {
    let file = open_read(path)?;
    if file.metadata()?.len() == 0 || file.metadata()?.len() > ARCHIVE_LIMIT {
        return Err(Error("Invalid archive size."));
    }
    let mut zip = ZipArchive::new(file)?;
    let mut entries = HashSet::new();
    let mut total = 0u64;
    let mut metadata = 0;
    for i in 0..zip.len() {
        job.check()?;
        let e = zip.by_index(i)?;
        let entry_name = e.name()?.into_owned();
        metadata += 128 + 2 * entry_name.encode_utf16().count();
        let limit = if entry_name == "manifest.json" {
            MANIFEST_LIMIT
        } else {
            FILE_LIMIT
        };
        if metadata > MANIFEST_LIMIT
            || e.is_dir()
            || e.compression() != CompressionMethod::Stored
            || e.size() == 0
            || e.size() > limit as u64
            || e.compressed_size() != e.size()
            || !(entry_name == "manifest.json" || asset_path(&entry_name))
            || !entries.insert(entry_name.clone())
            || i == 0 && entry_name != "manifest.json"
        {
            return Err(Error("Invalid or duplicate archive entry."));
        }
        if i > 0 {
            total += e.size();
            if total > TOTAL_LIMIT as u64 {
                return Err(Error("Pack exceeds 100 MiB."));
            }
        }
    }
    let bytes = read_limit(zip.by_name("manifest.json")?, MANIFEST_LIMIT, job)?;
    let manifest: Manifest = json(&bytes)?;
    if manifest.format != "xmsp" || manifest.version != 1 || manifest.items.is_empty() {
        return Err(Error("Unsupported or empty XMSP pack."));
    }
    valid_name(&manifest.name)?;
    let mut ids = HashSet::new();
    let mut used = HashSet::from(["manifest.json".to_owned()]);
    let mut dimensions = HashMap::new();
    for item in &manifest.items {
        let uuid =
            uuid::Uuid::parse_str(&item.id).map_err(|_| Error("Invalid archive item ID."))?;
        valid_name(&item.name)?;
        tag(&item.emoji)?;
        let path = format!("assets/{}{}", item.sha256, extension(&item.mime)?);
        if item.id != uuid.to_string()
            || !ids.insert(&item.id)
            || item.size == 0
            || item.size > FILE_LIMIT as u64
            || item.path != path
            || !asset_path(&path)
            || !entries.contains(&path)
            || !(1..=512).contains(&item.width)
            || !(1..=512).contains(&item.height)
        {
            return Err(Error("Invalid archive item metadata."));
        }
        if let Some(previous) =
            dimensions.insert(path.clone(), (item.width, item.height, item.size))
        {
            if previous != (item.width, item.height, item.size) {
                return Err(Error("Conflicting archive artwork."));
            }
        }
        used.insert(path);
    }
    if entries != used {
        return Err(Error("Archive contains unreferenced artwork."));
    }
    let mut items = Vec::new();
    let mut staged_assets = HashMap::<String, String>::new();
    for (i, item) in manifest.items.iter().enumerate() {
        job.update("Importing artwork", i, manifest.items.len(), 0);
        job.check()?;
        let file = if let Some(file) = staged_assets.get(&item.path) {
            file.clone()
        } else {
            let bytes = read_limit(zip.by_name(&item.path)?, FILE_LIMIT, job)?;
            if bytes.len() as u64 != item.size || hash(&bytes) != item.sha256 {
                return Err(Error("Artwork hash or size does not match the manifest."));
            }
            validate_media(&bytes, &item.mime, item.width, item.height, Some(job))?;
            let file = format!("{}{}", uuid::Uuid::new_v4(), extension(&item.mime)?);
            stage.save(&file, &bytes)?;
            staged_assets.insert(item.path.clone(), file.clone());
            file
        };
        items.push(Item {
            sync_id: item.id.clone(),
            id: new_id(),
            name: valid_name(&item.name)?,
            emoji: tag(&item.emoji)?,
            mime: item.mime.clone(),
            width: item.width,
            height: item.height,
            file,
            remote_id: String::new(),
        });
    }
    Ok(Pack {
            sync_id: manifest.sync.as_ref().map(|s|s.id.clone()).unwrap_or_default(),
        id: new_id(),
        name: valid_name(&manifest.name)?,
        source: manifest.sync.as_ref().map(|s|s.source.clone()).unwrap_or_else(||format!("local:{}", uuid::Uuid::new_v4())),
        kind: manifest.kind,
        installed: manifest.sync.as_ref().is_none_or(|s|s.installed),
        private_pack: manifest.sync.as_ref().is_none_or(|s|s.private_pack),
        connected: manifest.sync.as_ref().is_some_and(|s|s.connected),
        auto_sync: manifest.sync.as_ref().is_some_and(|s|s.auto_sync),
        last_sync: 0,
        last_attempt: 0,
        sync_error: String::new(),
        items,
    })
}
fn write_archive(pack: &Pack, root: &Path, output: &Path, job: &Job) -> Result<()> {
    if pack.items.is_empty() {
        return Err(Error("Packs must contain at least one item."));
    }
    let mut manifest = Manifest {
        sync: (!pack.sync_id.is_empty()).then(||PackSync{id:pack.sync_id.clone(),source:pack.source.clone(),installed:pack.installed,private_pack:pack.private_pack,connected:pack.connected,auto_sync:pack.auto_sync}),
        format: "xmsp".into(),
        version: 1,
        name: valid_name(&pack.name)?,
        kind: pack.kind,
        items: Vec::new(),
    };
    let mut assets = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0;
    for (i, item) in pack.items.iter().enumerate() {
        job.update("Preparing archive", i, pack.items.len(), total as u64);
        validate_item(item)?;
        let bytes = read_limit(open_read(&root.join(&item.file))?, FILE_LIMIT, job)?;
        validate_media(&bytes, &item.mime, item.width, item.height, Some(job))?;
        let sha256 = hash(&bytes);
        let path = format!("assets/{sha256}{}", extension(&item.mime)?);
        if seen.insert(path.clone()) {
            total += bytes.len();
            if total > TOTAL_LIMIT {
                return Err(Error("Pack exceeds 100 MiB."));
            }
            assets.push((path.clone(), item.file.clone()));
        }
        manifest.items.push(ManifestItem {
            id: if item.sync_id.is_empty(){uuid::Uuid::new_v4().to_string()}else{item.sync_id.clone()},
            name: item.name.clone(),
            emoji: item.emoji.clone(),
            mime: item.mime.clone(),
            width: item.width,
            height: item.height,
            size: bytes.len() as u64,
            sha256,
            path,
        });
    }
    let json = serde_json::to_vec(&manifest)?;
    if json.len() > MANIFEST_LIMIT {
        return Err(Error("Pack manifest exceeds 4 MiB."));
    }
    let mut zip = ZipWriter::new(new_file(output)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("manifest.json", options)?;
    zip.write_all(&json)?;
    for (path, file) in assets {
        let bytes = read_limit(open_read(&root.join(file))?, FILE_LIMIT, job)?;
        if !path.starts_with(&format!("assets/{}.", hash(&bytes))) {
            return Err(Error("Pack changed during export. Retry."));
        }
        zip.start_file(path, options)?;
        zip.write_all(&bytes)?;
    }
    let file = zip.finish()?;
    file.sync_all()?;
    job.check()?;
    if file.metadata()?.len() > ARCHIVE_LIMIT {
        return Err(Error("Archive exceeds 105 MiB."));
    }
    Ok(())
}

// XEP-0071/XEP-0231 profile shared with mobile xmpp/InlineEmoji.java (GPL-3.0-or-later).
const XHTML: &str = "http://www.w3.org/1999/xhtml";
const HTML: &str = "http://jabber.org/protocol/xhtml-im";
const BOB: &str = "urn:xmpp:bob";
const INLINE_LIMIT: usize = 128 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineAsset {
    pub cid: String,
    pub mime: String,
    #[serde(with = "shared_bytes")]
    pub bytes: Arc<Vec<u8>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineRange {
    pub start: usize,
    pub end: usize,
    pub asset: InlineAsset,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineEmoji {
    pub body: String,
    pub ranges: Vec<InlineRange>,
}
impl InlineAsset {
    pub fn new(mime: String, bytes: Vec<u8>) -> Result<Self> {
        extension(&mime)?;
        if bytes.is_empty() || bytes.len() > INLINE_LIMIT {
            return Err(Error("Inline emoji must fit in 128 KiB."));
        }
        Ok(Self {
            cid: format!("sha-256+{}@bob.xmpp.org", hash(&bytes)),
            mime,
            bytes: Arc::new(bytes),
        })
    }
    fn validate(&self) -> Result<()> {
        extension(&self.mime)?;
        if self.bytes.is_empty()
            || self.bytes.len() > INLINE_LIMIT
            || self.cid != format!("sha-256+{}@bob.xmpp.org", hash(&self.bytes))
        {
            return Err(Error("Inline emoji hash does not match its bytes."));
        }
        Ok(())
    }
}
impl InlineEmoji {
    pub fn new(body: String, mut ranges: Vec<InlineRange>) -> Result<Self> {
        if body.encode_utf16().count() > 131072 || body.chars().count() > 65536 || ranges.len() > 64
        {
            return Err(Error("Too much inline emoji data."));
        }
        ranges.sort_by_key(|r| r.start);
        let chars: Vec<_> = body.chars().collect();
        let mut last = 0;
        let mut assets = HashMap::<String, InlineAsset>::new();
        let mut total = 0;
        for r in &mut ranges {
            r.asset.validate()?;
            if r.start < last
                || r.end <= r.start
                || r.end > chars.len()
                || r.end - r.start > 64
                || chars[r.start..r.end].iter().any(|c| c.is_control())
            {
                return Err(Error("Invalid inline emoji range."));
            }
            last = r.end;
            if let Some(previous) = assets.get(&r.asset.cid) {
                if previous.mime != r.asset.mime {
                    return Err(Error("Conflicting emoji media type."));
                }
                r.asset.bytes = previous.bytes.clone();
            } else {
                total += r.asset.bytes.len();
                assets.insert(r.asset.cid.clone(), r.asset.clone());
            }
        }
        if assets.len() > 16 || total > INLINE_LIMIT {
            return Err(Error("Too much inline emoji data."));
        }
        Ok(Self { body, ranges })
    }
    pub fn in_body(&self, body: String, start: usize) -> Result<Self> {
        let expected: String = body
            .chars()
            .skip(start)
            .take(self.body.chars().count())
            .collect();
        if expected != self.body || start > body.chars().count() {
            return Err(Error("Emoji belong to another body."));
        }
        let ranges = self
            .ranges
            .iter()
            .cloned()
            .map(|mut r| {
                r.start += start;
                r.end += start;
                r
            })
            .collect();
        Self::new(body, ranges)
    }
    pub fn slice(&self, start: usize, end: usize) -> Result<Self> {
        if start > end || end > self.body.chars().count() {
            return Err(Error("Invalid inline emoji slice."));
        }
        Self::new(
            self.body.chars().skip(start).take(end - start).collect(),
            self.ranges
                .iter()
                .filter(|r| r.start >= start && r.end <= end)
                .cloned()
                .map(|mut r| {
                    r.start -= start;
                    r.end -= start;
                    r
                })
                .collect(),
        )
    }
    pub fn without(&self, intervals: &[(usize, usize)]) -> Result<Self> {
        let chars: Vec<_> = self.body.chars().collect();
        let mut removed = vec![false; chars.len()];
        for (start, end) in intervals {
            if start > end || *end > chars.len() {
                return Err(Error("Invalid emoji removal."));
            }
            removed[*start..*end].fill(true);
        }
        let mut map = vec![0; chars.len() + 1];
        let mut body = String::new();
        for (i, c) in chars.iter().enumerate() {
            map[i + 1] = map[i];
            if !removed[i] {
                body.push(*c);
                map[i + 1] += 1;
            }
        }
        Self::new(
            body,
            self.ranges
                .iter()
                .filter(|r| !removed[r.start..r.end].iter().any(|b| *b))
                .cloned()
                .map(|mut r| {
                    r.start = map[r.start];
                    r.end = map[r.end];
                    r
                })
                .collect(),
        )
    }
    pub fn add_to(&self, message: &mut tokio_xmpp::minidom::Element) -> Result<()> {
        use base64::Engine;
        use tokio_xmpp::minidom::Element;
        Self::new(self.body.clone(), self.ranges.clone())?;
        if message
            .get_child("body", "jabber:client")
            .map(|b| b.text())
            .as_deref()
            != Some(&self.body)
            || message
                .children()
                .any(|c| c.is("html", HTML) || c.is("data", BOB))
        {
            return Err(Error("Inline emoji belong to another message."));
        }
        if self.ranges.is_empty() {
            return Ok(());
        }
        let mut p = Element::builder("p", XHTML).build();
        let mut previous = 0;
        let mut assets = HashSet::new();
        for r in &self.ranges {
            let mut span = Element::builder("span", XHTML).build();
            let text: String = self
                .body
                .chars()
                .skip(previous)
                .take(r.start - previous)
                .collect();
            if !text.is_empty() {
                span.append_child(Element::builder("span", XHTML).append(text).build());
            }
            let alt: String = self
                .body
                .chars()
                .skip(r.start)
                .take(r.end - r.start)
                .collect();
            span.append_child(
                Element::builder("img", XHTML)
                    .attr("src".try_into().unwrap(), format!("cid:{}", r.asset.cid))
                    .attr("alt".try_into().unwrap(), alt)
                    .attr("width".try_into().unwrap(), "32")
                    .attr("height".try_into().unwrap(), "32")
                    .build(),
            );
            p.append_child(span);
            previous = r.end;
        }
        let tail: String = self.body.chars().skip(previous).collect();
        if !tail.is_empty() {
            p.append_child(Element::builder("span", XHTML).append(tail).build());
        }
        message.append_child(
            Element::builder("html", HTML)
                .append(Element::builder("body", XHTML).append(p).build())
                .build(),
        );
        for r in &self.ranges {
            if assets.insert(&r.asset.cid) {
                message.append_child(
                    Element::builder("data", BOB)
                        .attr("cid".try_into().unwrap(), &r.asset.cid)
                        .attr("type".try_into().unwrap(), &r.asset.mime)
                        .attr("max-age".try_into().unwrap(), "31536000")
                        .append(
                            base64::engine::general_purpose::STANDARD
                                .encode(r.asset.bytes.as_slice()),
                        )
                        .build(),
                );
            }
        }
        Ok(())
    }
    /// Call only on cleartext or authenticated SCE, never on Legacy encrypted outer XML.
    pub fn from_message(message: &tokio_xmpp::minidom::Element, body: &str) -> Option<Self> {
        Self::parse(message, body).ok()
    }
    fn parse(message: &tokio_xmpp::minidom::Element, body: &str) -> Result<Self> {
        use base64::Engine;
        if message
            .get_child("body", "jabber:client")
            .map(|b| b.text())
            .as_deref()
            != Some(body)
        {
            return Err(Error("Emoji body mismatch."));
        }
        let html: Vec<_> = message.children().filter(|c| c.is("html", HTML)).collect();
        let data: Vec<_> = message.children().filter(|c| c.is("data", BOB)).collect();
        if html.len() != 1 || data.is_empty() || data.len() > 16 {
            return Err(Error("Invalid emoji payload."));
        }
        let mut assets = HashMap::new();
        let mut total = 0;
        for e in data {
            let text = e.text();
            if text.len() > (INLINE_LIMIT + 2) / 3 * 4
                || !attrs(e, &["cid", "type", "max-age"])
                || e.children().next().is_some()
            {
                return Err(Error("Invalid emoji asset."));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| Error("Invalid emoji encoding."))?;
            total += bytes.len();
            if total > INLINE_LIMIT {
                return Err(Error("Too much inline emoji data."));
            }
            let asset = InlineAsset::new(e.attr("type").unwrap_or("").into(), bytes)?;
            if e.attr("cid") != Some(&asset.cid)
                || assets.insert(asset.cid.clone(), asset).is_some()
            {
                return Err(Error("Invalid or duplicate emoji asset."));
            }
        }
        let wrapper = html[0];
        if !attrs(wrapper, &[])
            || !wrapper.text().trim().is_empty()
            || wrapper.children().count() != 1
        {
            return Err(Error("Invalid XHTML wrapper."));
        }
        let content = wrapper.children().next().unwrap();
        if !content.is("body", XHTML)
            || !attrs(content, &["xml:lang"])
            || !content.text().trim().is_empty()
        {
            return Err(Error("Invalid XHTML body."));
        }
        if let Some(language) = content.attr_ns("http://www.w3.org/XML/1998/namespace", "lang") {
            if message.attr_ns("http://www.w3.org/XML/1998/namespace", "lang") != Some(language) {
                return Err(Error("Emoji language mismatch."));
            }
        }
        let mut text = String::new();
        let mut ranges = Vec::new();
        let mut nodes = 0;
        for child in content.children() {
            parse_inline(child, &assets, &mut text, &mut ranges, 0, &mut nodes)?;
        }
        if text != body || ranges.is_empty() {
            return Err(Error("Emoji fallback mismatch."));
        }
        Self::new(text, ranges)
    }
}
fn attrs(e: &tokio_xmpp::minidom::Element, names: &[&str]) -> bool {
    e.attrs().iter().all(|((ns, name), _)| {
        let name = if ns.as_ref() == "http://www.w3.org/XML/1998/namespace" {
            format!("xml:{name}")
        } else if ns.as_ref() == "" {
            name.to_string()
        } else {
            return false;
        };
        names.contains(&name.as_str())
    })
}
fn parse_inline(
    e: &tokio_xmpp::minidom::Element,
    assets: &HashMap<String, InlineAsset>,
    text: &mut String,
    ranges: &mut Vec<InlineRange>,
    depth: usize,
    nodes: &mut usize,
) -> Result<()> {
    *nodes += 1;
    if depth > 8 || *nodes > 256 || e.ns() != XHTML {
        return Err(Error("Invalid inline emoji XHTML."));
    }
    if e.name() == "img" {
        if !attrs(e, &["src", "alt", "width", "height"])
            || e.children().next().is_some()
            || !e.text().trim().is_empty()
        {
            return Err(Error("Invalid inline emoji image."));
        }
        let cid = e
            .attr("src")
            .and_then(|s| s.strip_prefix("cid:"))
            .ok_or(Error("Invalid emoji CID."))?;
        let alt = e
            .attr("alt")
            .filter(|s| !s.is_empty() && s.encode_utf16().count() <= 128)
            .ok_or(Error("Invalid emoji fallback."))?;
        let asset = assets
            .get(cid)
            .ok_or(Error("Missing emoji asset."))?
            .clone();
        let start = text.chars().count();
        text.push_str(alt);
        ranges.push(InlineRange {
            start,
            end: text.chars().count(),
            asset,
        });
    } else if matches!(e.name(), "span" | "p") {
        if !attrs(e, &[]) || e.children().next().is_some() && !e.text().trim().is_empty() {
            return Err(Error("Invalid inline emoji text."));
        }
        if e.children().next().is_some() {
            for c in e.children() {
                parse_inline(c, assets, text, ranges, depth + 1, nodes)?;
            }
        } else {
            text.push_str(&e.text());
        }
    } else {
        return Err(Error("Unsupported inline emoji node."));
    }
    if text.encode_utf16().count() > 131072 || ranges.len() > 64 {
        return Err(Error("Too much inline emoji data."));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramLink {
    pub name: String,
    pub kind: Kind,
    pub url: String,
}
impl TelegramLink {
    pub fn parse(value: &str) -> Result<Self> {
        let url = reqwest::Url::parse(value.trim())
            .map_err(|_| Error("Use an https://t.me/addstickers/name or addemoji/name link."))?;
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("t.me" | "telegram.me"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error("Invalid Telegram pack link."));
        }
        let path: Vec<_> = url
            .path_segments()
            .ok_or(Error("Invalid pack link."))?
            .collect();
        if path.len() != 2
            || !matches!(path[0], "addstickers" | "addemoji")
            || path[1].is_empty()
            || path[1].len() > 64
            || !path[1]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error("Invalid Telegram pack link."));
        }
        Ok(Self {
            name: path[1].into(),
            kind: if path[0] == "addemoji" {
                Kind::Emoji
            } else {
                Kind::Stickers
            },
            url: format!("https://t.me/{}/{}", path[0], path[1]),
        })
    }
}
pub fn telegram_token_valid(value: &str) -> bool {
    let Some((id, key)) = value.split_once(':') else {
        return false;
    };
    (5..=20).contains(&id.len())
        && id.bytes().all(|c| c.is_ascii_digit())
        && (20..=200).contains(&key.len())
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
impl Library {
    pub async fn telegram_enabled(&self) -> Result<bool> {
        self.check(&Job::new())?;
        crate::credentials::load_service(self.account(), "telegram-packs")
            .await
            .map(|s| s.is_some())
            .map_err(|_| Error("The system keyring could not read the Telegram token."))
    }
    pub async fn set_telegram_token(&self, token: zeroize::Zeroizing<String>) -> Result<()> {
        self.check(&Job::new())?;
        if token.is_empty() {
            crate::credentials::remove_service(self.account(), "telegram-packs")
                .await
                .map_err(|_| Error("The system keyring could not remove the Telegram token."))
        } else {
            if !telegram_token_valid(&token) {
                return Err(Error("Enter a valid Telegram bot token."));
            }
            crate::credentials::save_service(self.account(), "telegram-packs", token)
                .await
                .map_err(|_| Error("The system keyring could not save the Telegram token."))
        }
    }
    pub async fn import_telegram(
        &self,
        link: String,
        update_pack: Option<i64>,
        job: Job,
    ) -> Result<Pack> {
        let parsed = TelegramLink::parse(&link)?;
        self.check(&job)?;
        let token = crate::credentials::load_service(self.account(), "telegram-packs")
            .await
            .map_err(|_| Error("Unlock the system keyring to read the Telegram token."))?
            .ok_or(Error(
                "Enable Telegram pack imports with your bot token first.",
            ))?;
        if !telegram_token_valid(&token) {
            return Err(Error("Replace the saved Telegram bot token."));
        }
        let this = self.clone();
        let done = job.clone();
        let failure_this = self.clone();
        let failure_link = parsed.url.clone();
        let result = worker(move || {
            this.check(&job)?;
            let previous = {
                let _guard = this.owner.gate.lock().unwrap();
                let mut snapshot = this.read()?;
                if let Some(id) = update_pack {
                    let pack = snapshot
                        .packs
                        .iter_mut()
                        .find(|p| p.id == id)
                        .ok_or(Error("Pack is no longer available."))?;
                    if !pack.connected
                        || !pack.installed
                        || !pack.source.eq_ignore_ascii_case(&parsed.url)
                    {
                        return Err(Error("The pack was detached or removed."));
                    }
                    let previous = pack.clone();
                    pack.last_attempt = chrono::Utc::now().timestamp();
                    this.write(&snapshot)?;
                    Some(previous)
                } else {
                    if snapshot.packs.iter().any(|p| {
                        p.installed && p.connected && p.source.eq_ignore_ascii_case(&parsed.url)
                    }) {
                        return Err(Error(
                            "This pack is already installed. Check it for updates.",
                        ));
                    }
                    None
                }
            };
            let mut stage = Staging::new(&this.owner.path)?;
            let mut pack = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Error("Network worker unavailable."))?
                .block_on(this.telegram_download(
                    &parsed,
                    previous.as_ref(),
                    &token,
                    &mut stage,
                    &job,
                ))?;
            this.check(&job)?;
            let _guard = this.owner.gate.lock().unwrap();
            let mut snapshot = this.read()?;
            if let Some(id) = update_pack {
                let index = snapshot
                    .packs
                    .iter()
                    .position(|p| p.id == id)
                    .ok_or(Error("The pack was removed."))?;
                let latest = snapshot.packs[index].clone();
                if !latest.connected
                    || !latest.installed
                    || !latest.source.eq_ignore_ascii_case(&parsed.url)
                {
                    return Err(Error(
                        "The pack was detached or removed. Its local copy was preserved.",
                    ));
                }
                let mut remote: HashMap<_, _> = pack
                    .items
                    .iter()
                    .cloned()
                    .map(|i| (i.remote_id.clone(), i))
                    .collect();
                let mut ordered = Vec::new();
                let mut retained = Vec::new();
                for old in &latest.items {
                    if let Some(mut current) = remote.remove(&old.remote_id) {
                        if current.file == old.file {
                            current = old.clone();
                        }
                        ordered.push(current);
                    } else {
                        retained.push(old.clone());
                    }
                }
                for item in &pack.items {
                    if let Some(item) = remote.remove(&item.remote_id) {
                        ordered.push(item);
                    }
                }
                pack.items = ordered;
                pack.id = id;
                pack.private_pack = latest.private_pack;
                pack.auto_sync = latest.auto_sync;
                snapshot.packs[index] = pack.clone();
                archive_items(&mut snapshot, id, retained);
            } else {
                if snapshot.packs.iter().any(|p| {
                    p.installed && p.connected && p.source.eq_ignore_ascii_case(&parsed.url)
                }) {
                    return Err(Error("This pack was installed while importing."));
                }
                snapshot.packs.insert(0, pack.clone());
            }
            check_pack_bytes(&this.owner.path, &stage.path, &pack)?;
            stage.publish(&this.owner.path)?;
            this.check(&job)?;
            this.write(&snapshot)?;
            stage.committed = true;
            Ok(pack)
        })
        .await;
        if let (Some(id), Err(error)) = (update_pack, &result) {
            if done.check().is_ok() {
                let error = error.to_string();
                let _ = worker(move || {
                    let _guard = failure_this.owner.gate.lock().unwrap();
                    let mut s = failure_this.read()?;
                    if let Some(p) = s.packs.iter_mut().find(|p| {
                        p.id == id && p.connected && p.source.eq_ignore_ascii_case(&failure_link)
                    }) {
                        p.sync_error = error;
                        p.last_attempt = chrono::Utc::now().timestamp();
                        failure_this.write(&s)?;
                    }
                    Ok(())
                })
                .await;
            }
        }
        done.finish(&result);
        result
    }
    async fn telegram_download(
        &self,
        link: &TelegramLink,
        previous: Option<&Pack>,
        token: &str,
        stage: &mut Staging,
        job: &Job,
    ) -> Result<Pack> {
        use futures_util::{StreamExt, stream};
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| Error("Secure Telegram connection unavailable."))?;
        job.update("Loading Telegram pack", 0, 0, 0);
        let set = self
            .telegram_json(&client, token, "getStickerSet", "name", &link.name, job)
            .await?;
        if !set["name"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(&link.name))
            || (set["sticker_type"].as_str() == Some("custom_emoji")) != (link.kind == Kind::Emoji)
        {
            return Err(Error("Telegram returned a different pack."));
        }
        let name = valid_name(
            set["title"]
                .as_str()
                .ok_or(Error("Telegram returned incomplete pack data."))?,
        )?;
        let stickers = set["stickers"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or(Error("This Telegram pack has no stickers."))?;
        let mut identities = HashSet::new();
        let mut tasks = Vec::new();
        let mut reused = HashMap::new();
        for (index, sticker) in stickers.iter().enumerate() {
            self.check(job)?;
            let file_id = sticker["file_id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 2048)
                .ok_or(Error("Telegram returned an invalid file ID."))?;
            let identity = sticker["file_unique_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("file:{file_id}"));
            if identity.len() > 4096 || !identities.insert(identity.clone()) {
                return Err(Error(
                    "Telegram returned duplicate or invalid file identities.",
                ));
            }
            if let Some(item) =
                previous.and_then(|p| p.items.iter().find(|i| i.remote_id == identity))
            {
                if let Ok(file) = open_read(&self.owner.path.join(&item.file)) {
                    let bytes = read_limit(file, FILE_LIMIT, job)?;
                    validate_media(&bytes, &item.mime, item.width, item.height, Some(job))?;
                    reused.insert(index, (item.clone(), bytes.len()));
                    continue;
                }
            }
            tasks.push((index, file_id.to_owned(), identity, sticker.clone()));
        }
        let count = stickers.len();
        let mut items = HashMap::new();
        let mut total = 0;
        for (index, (item, size)) in reused {
            total += size;
            items.insert(index, item);
        }
        if total > TOTAL_LIMIT {
            return Err(Error("Pack exceeds 100 MiB."));
        }
        let mut downloads = stream::iter(tasks.into_iter().map(
            |(index, file_id, identity, sticker)| {
                let client = &client;
                let stage_path = stage.path.clone();
                async move {
                    let remote = self
                        .telegram_json(client, token, "getFile", "file_id", &file_id, job)
                        .await?;
                    let path = remote["file_path"]
                        .as_str()
                        .filter(|s| telegram_file_path(s))
                        .ok_or(Error("Telegram returned an invalid media path."))?;
                    let ext = path.rsplit_once('.').unwrap().1;
                    let mime = match ext {
                        "png" => "image/png",
                        "webp" => "image/webp",
                        "gif" => "image/gif",
                        "tgs" => "application/x-tgsticker",
                        "webm" => "video/webm",
                        _ => return Err(Error("Unsupported Telegram artwork.")),
                    };
                    let bytes = self
                        .telegram_bytes(
                            client,
                            format!("https://api.telegram.org/file/bot{token}/{path}"),
                            FILE_LIMIT,
                            job,
                        )
                        .await?;
                    let width = sticker["width"].as_u64().unwrap_or(512).clamp(1, 512) as u32;
                    let height = sticker["height"].as_u64().unwrap_or(512).clamp(1, 512) as u32;
                    validate_media(&bytes, mime, width, height, Some(job))?;
                    let file = format!("{}{}", uuid::Uuid::new_v4(), extension(mime)?);
                    let mut output = new_file(&stage_path.join(&file))?;
                    output.write_all(&bytes)?;
                    output.sync_all()?;
                    Ok::<_, Error>((
                        index,
                        Item {
                            sync_id: String::new(),
                            id: new_id(),
                            name: format!("Item {}", index + 1),
                            emoji: tag(sticker["emoji"].as_str().unwrap_or("🙂"))?,
                            mime: mime.into(),
                            file,
                            width,
                            height,
                            remote_id: identity,
                        },
                        bytes.len(),
                    ))
                }
            },
        ))
        .buffer_unordered(4);
        while let Some(result) = downloads.next().await {
            let (index, item, size) = result?;
            total += size;
            if total > TOTAL_LIMIT {
                return Err(Error("Pack exceeds 100 MiB."));
            }
            items.insert(index, item);
            job.update(
                "Downloading Telegram artwork",
                items.len(),
                count,
                total as u64,
            );
        }
        let mut ordered = Vec::with_capacity(count);
        for index in 0..count {
            ordered.push(
                items
                    .remove(&index)
                    .ok_or(Error("Telegram artwork was incomplete."))?,
            );
        }
        let now = chrono::Utc::now().timestamp();
        Ok(Pack {
            sync_id: String::new(),
            id: previous.map(|p| p.id).unwrap_or_else(new_id),
            name,
            source: link.url.clone(),
            kind: link.kind,
            installed: true,
            private_pack: true,
            connected: true,
            auto_sync: false,
            last_sync: now,
            last_attempt: now,
            sync_error: String::new(),
            items: ordered,
        })
    }
    async fn telegram_json(
        &self,
        client: &reqwest::Client,
        token: &str,
        method: &str,
        key: &str,
        value: &str,
        job: &Job,
    ) -> Result<serde_json::Value> {
        let mut url = reqwest::Url::parse(&format!("https://api.telegram.org/bot{token}/{method}"))
            .map_err(|_| Error("Invalid Telegram request."))?;
        url.query_pairs_mut().append_pair(key, value);
        let bytes = self
            .telegram_bytes(client, url.to_string(), MANIFEST_LIMIT, job)
            .await?;
        let v: serde_json::Value = json(&bytes)?;
        if v["ok"] != true {
            return Err(telegram_error(v["error_code"].as_u64().unwrap_or(0) as u16));
        }
        v.get("result")
            .cloned()
            .ok_or(Error("Telegram returned incomplete data."))
    }
    async fn telegram_bytes(
        &self,
        client: &reqwest::Client,
        url: String,
        limit: usize,
        job: &Job,
    ) -> Result<Vec<u8>> {
        use futures_util::StreamExt;
        let response = self
            .network(client.get(url).send(), job)
            .await?
            .map_err(|_| {
                Error("Could not securely connect to Telegram. Check the connection and retry.")
            })?;
        if response.status() != 200 {
            return Err(telegram_error(response.status().as_u16()));
        }
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(Error("Telegram file exceeds the safe byte limit."));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = self.network(stream.next(), job).await? {
            let chunk = chunk.map_err(|_| Error("Telegram download was interrupted."))?;
            if bytes.len() + chunk.len() > limit {
                return Err(Error("Telegram file exceeds the safe byte limit."));
            }
            bytes.extend_from_slice(&chunk);
            let mut p = job.progress.lock().unwrap();
            p.bytes += chunk.len() as u64;
        }
        self.check(job)?;
        if bytes.is_empty() {
            return Err(Error("Telegram returned an empty response."));
        }
        Ok(bytes)
    }
    async fn network<T>(
        &self,
        future: impl std::future::Future<Output = T>,
        job: &Job,
    ) -> Result<T> {
        tokio::pin!(future);
        loop {
            self.check(job)?;
            tokio::select! {value=&mut future=>return Ok(value),_=tokio::time::sleep(std::time::Duration::from_millis(80))=>{}}
        }
    }
}
fn telegram_file_path(path: &str) -> bool {
    let Some((stem, ext)) = path.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && path.len() <= 4096
        && !path.contains("..")
        && !path.starts_with('/')
        && stem
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'-'))
        && ["webp", "png", "gif", "tgs", "webm"].contains(&ext)
}
fn telegram_error(code: u16) -> Error {
    Error(match code {
        401 => "Telegram rejected the bot token. Replace it in pack settings.",
        403 => "Telegram denied this bot access.",
        404 => "Telegram pack or file was not found.",
        413 => "Telegram file exceeds the bot download limit.",
        429 => "Telegram is rate limiting this bot. Try again later.",
        500..=599 => "Telegram is temporarily unavailable.",
        _ => "Telegram rejected the request. Check the pack link and token.",
    })
}

pub fn normalized_inline(
    message: &tokio_xmpp::minidom::Element,
    visible: &str,
) -> Option<InlineEmoji> {
    let raw = message
        .get_child("body", "jabber:client")
        .map(|e| e.text())
        .unwrap_or_default();
    let ((_, _, _, clean), removed) = crate::protocol::reply_projection(message, &raw);
    let mut model = InlineEmoji::from_message(message, &raw)
        .unwrap_or(InlineEmoji {
            body: raw.clone(),
            ranges: Vec::new(),
        })
        .without(&removed)
        .ok()?;
    if model.body != clean {
        return None;
    }
    if let Some((attribution, body)) = crate::forwarding::parse(&model.body) {
        model = model
            .slice(
                attribution.prefix().chars().count(),
                model.body.chars().count(),
            )
            .ok()?;
        if model.body != body {
            return None;
        }
    } else if let Some((_, body)) = crate::forwarding::standalone(message) {
        let inner = message
            .get_child("forwarded", crate::forwarding::NS)?
            .get_child("message", "jabber:client")?;
        let offset = if model.body.is_empty() {
            0
        } else {
            model.body.chars().count() + 2
        };
        let mut full = model.body.clone();
        if !full.is_empty() {
            full.push_str("\n\n");
        }
        full.push_str(&body);
        let mut ranges = model.ranges;
        if let Some(inner) = InlineEmoji::from_message(inner, &body) {
            ranges.extend(inner.in_body(full.clone(), offset).ok()?.ranges);
        }
        model = InlineEmoji::new(full, ranges).ok()?;
    }
    (model.body == visible && !model.ranges.is_empty()).then_some(model)
}
pub fn decorate_inline(root: &mut tokio_xmpp::minidom::Element, message: &crate::model::Message) {
    if message.mode() == crate::encryption_policy::Mode::Legacy
        || message.attachment.is_some()
        || message.poll.is_some()
        || matches!(
            message.action,
            Some(
                crate::message_actions::Action::React { .. }
                    | crate::message_actions::Action::Retract { .. }
            )
        )
    {
        return;
    }
    let Some(emoji) = message
        .inline_emoji
        .as_ref()
        .filter(|m| m.body == message.text)
    else {
        return;
    };
    let Some(body) = root.get_child("body", "jabber:client").map(|b| b.text()) else {
        return;
    };
    let accompanied = message
        .forwarded
        .as_ref()
        .map(|a| a.wire_body(&message.text))
        .unwrap_or_else(|| message.text.clone());
    if !body.ends_with(&accompanied) {
        return;
    }
    let offset = body.chars().count() - message.text.chars().count();
    if let Ok(emoji) = emoji.in_body(body, offset) {
        let _ = emoji.add_to(root);
    }
}

// Use the existing libglycin sandbox ABI without depending on GTK message-renderer modules.
struct PackImage {
    image: glib::Object,
    cancel: gio::Cancellable,
    mime: String,
    width: u32,
    height: u32,
}
unsafe extern "C" {
    fn monogram_image_open(
        stream: *mut gio::ffi::GInputStream,
        cancel: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut glib::gobject_ffi::GObject;
    fn monogram_image_frame(
        image: *mut glib::gobject_ffi::GObject,
        edge: u32,
        delay: *mut i64,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut gtk::gdk::ffi::GdkTexture;
    fn gly_image_get_mime_type(image: *mut glib::gobject_ffi::GObject) -> *const std::ffi::c_char;
    fn gly_image_get_width(image: *mut glib::gobject_ffi::GObject) -> u32;
    fn gly_image_get_height(image: *mut glib::gobject_ffi::GObject) -> u32;
}
struct PackFrame {
    texture: gtk::gdk::Texture,
}
impl PackImage {
    fn open_bytes(
        bytes: &glib::Bytes,
        cancel: &gio::Cancellable,
    ) -> std::result::Result<Self, &'static str> {
        use glib::translate::*;
        let stream = gio::MemoryInputStream::from_bytes(bytes);
        let mut error = std::ptr::null_mut();
        let raw = unsafe {
            monogram_image_open(
                stream.upcast_ref::<gio::InputStream>().to_glib_none().0,
                cancel.to_glib_none().0,
                &mut error,
            )
        };
        if !error.is_null() {
            unsafe { glib::ffi::g_error_free(error) }
        }
        if raw.is_null() {
            return Err("Artwork decoder unavailable");
        }
        let image: glib::Object = unsafe { from_glib_full(raw) };
        let mime = unsafe {
            let value = gly_image_get_mime_type(raw);
            if value.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(value)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        if !mime.starts_with("image/") {
            return Err("Unsupported artwork");
        }
        Ok(Self {
            image,
            cancel: cancel.clone(),
            mime,
            width: unsafe { gly_image_get_width(raw) },
            height: unsafe { gly_image_get_height(raw) },
        })
    }
    fn frame(&mut self, edge: u32) -> std::result::Result<PackFrame, &'static str> {
        use glib::translate::*;
        if self.cancel.is_cancelled() {
            return Err("Operation cancelled");
        }
        let mut delay = 0;
        let mut error = std::ptr::null_mut();
        let raw = unsafe {
            monogram_image_frame(self.image.to_glib_none().0, edge, &mut delay, &mut error)
        };
        if !error.is_null() {
            unsafe { glib::ffi::g_error_free(error) }
        }
        if raw.is_null() {
            return Err("Artwork decoder unavailable");
        }
        Ok(PackFrame {
            texture: unsafe { from_glib_full(raw) },
        })
    }
}
#[derive(Clone, Debug)]
pub struct InlineFile {
    pub path: PathBuf,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}
impl Library {
    pub async fn cache_inline(&self, asset: InlineAsset, job: Job) -> Result<InlineFile> {
        let this = self.clone();
        worker(move || {
            this.check(&job)?;
            asset.validate()?;
            let (w, h) = if asset.mime.starts_with("image/") {
                let decoder = PackImage::open_bytes(
                    &glib::Bytes::from_owned(asset.bytes.as_ref().clone()),
                    &job.decoder_cancel,
                )
                .map_err(|_| Error("Inline emoji image could not be decoded."))?;
                if decoder.mime != asset.mime
                    || decoder.width == 0
                    || decoder.height == 0
                    || decoder.width > 2048
                    || decoder.height > 2048
                    || u64::from(decoder.width) * u64::from(decoder.height) > 4_194_304
                {
                    return Err(Error("Inline emoji dimensions are too large."));
                }
                (decoder.width, decoder.height)
            } else {
                validate_media(&asset.bytes, &asset.mime, 512, 512, Some(&job))?;
                (512, 512)
            };
            let root = this.owner.path.join("inline");
            private_dir(&root)?;
            let path = root.join(format!("{}{}", hash(&asset.bytes), extension(&asset.mime)?));
            let _guard = this.owner.gate.lock().unwrap();
            this.check(&job)?;
            let valid = open_read(&path)
                .ok()
                .and_then(|file| read_limit(file, INLINE_LIMIT, &job).ok())
                .is_some_and(|bytes| bytes == *asset.bytes);
            if !valid {
                let temp = root.join(format!(".{}", uuid::Uuid::new_v4()));
                let result: Result<()> = (|| {
                    let mut f = new_file(&temp)?;
                    f.write_all(&asset.bytes)?;
                    f.sync_all()?;
                    this.check(&job)?;
                    fs::rename(&temp, &path)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                result?;
            }
            Ok(InlineFile {
                path,
                mime: asset.mime,
                width: w,
                height: h,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mobile_sync_archive_keeps_stable_ids_and_policy(){
        let root=std::env::temp_dir().join(format!("mobile-sync-pack-{}",uuid::Uuid::new_v4()));private_dir(&root).unwrap();let mut stage=Staging::new(&root).unwrap();
        let pack=read_archive(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/device-sync/mobile.xmsp"),&mut stage,&Job::new()).unwrap();
        assert_eq!(pack.sync_id,"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");assert_eq!(pack.items[0].sync_id,"11111111-1111-4111-8111-111111111111");assert!(!pack.installed);assert!(!pack.private_pack);assert!(pack.connected);assert!(pack.auto_sync);
        assert_eq!(pack.items[0].emoji,"😂");drop(stage);let _=fs::remove_dir_all(root);
    }
    #[test]
    fn private_archive_and_inline_emoji_preserve_mobile_contract() {
        let root =
            std::env::temp_dir().join(format!("monogram-pack-check-{}", uuid::Uuid::new_v4()));
        private_dir(&root).unwrap();
        let data = b"GIF89a\x01\0\x01\0\0\0\0\0".to_vec();
        let file = format!("{}.gif", uuid::Uuid::new_v4());
        let mut f = new_file(&root.join(&file)).unwrap();
        f.write_all(&data).unwrap();
        drop(f);
        let pack = Pack {
            sync_id: String::new(),
            id: new_id(),
            name: "Local pack".into(),
            source: "local:test".into(),
            kind: Kind::Emoji,
            installed: true,
            private_pack: true,
            connected: false,
            auto_sync: false,
            last_sync: 0,
            last_attempt: 0,
            sync_error: String::new(),
            items: vec![Item {
                sync_id:String::new(),
                id: new_id(),
                name: "Wave".into(),
                emoji: "👋".into(),
                mime: "image/gif".into(),
                file,
                width: 1,
                height: 1,
                remote_id: String::new(),
            }],
        };
        let path = root.join("roundtrip.xmsp");
        write_archive(&pack, &root, &path, &Job::new()).unwrap();
        let mut stage = Staging::new(&root).unwrap();
        let imported = read_archive(&path, &mut stage, &Job::new()).unwrap();
        assert_eq!(imported.kind, Kind::Emoji);
        assert_eq!(imported.items[0].emoji, "👋");
        assert_ne!(imported.items[0].id, pack.items[0].id);
        assert_eq!(
            fs::read(stage.path.join(&imported.items[0].file)).unwrap(),
            data
        );
        let cancel = Job::new();
        cancel.cancel();
        assert!(read_archive(&path, &mut stage, &cancel).is_err());
        let asset = InlineAsset::new("image/gif".into(), data).unwrap();
        let emoji = InlineEmoji::new(
            "😀 :wave: hi".into(),
            vec![InlineRange {
                start: 2,
                end: 8,
                asset,
            }],
        )
        .unwrap();
        let mut message: tokio_xmpp::minidom::Element =
            "<message xmlns='jabber:client'><body>😀 :wave: hi</body></message>"
                .parse()
                .unwrap();
        emoji.add_to(&mut message).unwrap();
        assert_eq!(
            InlineEmoji::from_message(&message, &emoji.body),
            Some(emoji.clone())
        );
        let projected = emoji.without(&[(0, 2)]).unwrap();
        assert_eq!(projected.body, ":wave: hi");
        assert_eq!(projected.ranges[0].start, 0);
        assert!(TelegramLink::parse("https://t.me/addemoji/Actual_pack").is_ok());
        assert!(TelegramLink::parse("https://t.me/addemoji/Actual_pack?token=hidden").is_err());
        assert!(!telegram_file_path("../sticker.webp"));
        drop(stage);
        fs::remove_dir_all(root).unwrap();
    }
}

mod shared_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        bytes: &Arc<Vec<u8>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        bytes.as_slice().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Arc<Vec<u8>>, D::Error> {
        Ok(Arc::new(Vec::<u8>::deserialize(deserializer)?))
    }
}
#[derive(Serialize, Deserialize)]
struct StoredInline {
    body: String,
    ranges: Vec<(usize, usize, String)>,
    assets: Vec<InlineAsset>,
}
impl Serialize for InlineEmoji {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut seen = HashSet::new();
        StoredInline {
            body: self.body.clone(),
            ranges: self
                .ranges
                .iter()
                .map(|r| (r.start, r.end, r.asset.cid.clone()))
                .collect(),
            assets: self
                .ranges
                .iter()
                .filter(|r| seen.insert(&r.asset.cid))
                .map(|r| r.asset.clone())
                .collect(),
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for InlineEmoji {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let stored = StoredInline::deserialize(deserializer)?;
        if stored.assets.len() > 16 || stored.ranges.len() > 64 {
            return Err(D::Error::custom("Too much inline emoji data"));
        }
        let mut assets = HashMap::new();
        for asset in stored.assets {
            asset.validate().map_err(D::Error::custom)?;
            if assets.insert(asset.cid.clone(), asset).is_some() {
                return Err(D::Error::custom("Duplicate emoji asset"));
            }
        }
        let mut ranges = Vec::new();
        for (start, end, cid) in stored.ranges {
            ranges.push(InlineRange {
                start,
                end,
                asset: assets
                    .get(&cid)
                    .ok_or_else(|| D::Error::custom("Missing emoji asset"))?
                    .clone(),
            });
        }
        Self::new(stored.body, ranges).map_err(D::Error::custom)
    }
}

fn rooted_file(root: &Path, name: &str) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    if name.is_empty() || name.contains('/') || name.contains('\0') || matches!(name, "." | "..") {
        return Err(Error("Invalid artwork filename."));
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(0x10000 | 0x20000 | 0x80000)
        .open(root)?;
    let name = std::ffi::CString::new(name).map_err(|_| Error("Invalid artwork filename."))?;
    unsafe extern "C" {
        fn openat(dirfd: i32, path: *const std::ffi::c_char, flags: i32, ...) -> i32;
    }
    let fd = unsafe { openat(directory.as_raw_fd(), name.as_ptr(), 0x20000 | 0x80000) };
    if fd < 0 {
        return Err(Error("Artwork file is unavailable."));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(Error("Artwork is not a regular file."));
    }
    Ok(file)
}
impl Library {
    pub async fn verified_asset(
        &self,
        id: i64,
        job: Job,
    ) -> Result<(Asset, crate::downloads::VerifiedFile)> {
        let this = self.clone();
        worker(move || {
            use std::io::{Seek, SeekFrom};
            this.check(&job)?;
            let _guard = this.owner.gate.lock().unwrap();
            let snapshot = this.read()?;
            let item = find_item(&snapshot, id)?.clone();
            validate_item(&item)?;
            job.update("Preparing artwork", 0, 1, 0);
            let mut input = rooted_file(&this.owner.path, &item.file)?;
            let bytes = read_limit(&mut input, FILE_LIMIT, &job)?;
            validate_media(&bytes, &item.mime, item.width, item.height, Some(&job))?;
            this.check(&job)?;
            input.seek(SeekFrom::Start(0))?;
            let asset = Asset {
                path: this.owner.path.join(&item.file),
                item,
                size: bytes.len() as u64,
                sha256: hash(&bytes),
            };
            Ok((asset, crate::downloads::VerifiedFile(input)))
        })
        .await
    }
    pub async fn verified_inline(
        &self,
        asset: InlineAsset,
        job: Job,
    ) -> Result<(InlineFile, crate::downloads::VerifiedFile)> {
        let file = self.cache_inline(asset.clone(), job.clone()).await?;
        let this = self.clone();
        worker(move || {
            use std::io::{Seek, SeekFrom};
            this.check(&job)?;
            let name = file
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(Error("Invalid inline artwork path."))?;
            let mut input = rooted_file(&this.owner.path.join("inline"), name)?;
            let bytes = read_limit(&mut input, INLINE_LIMIT, &job)?;
            if bytes != *asset.bytes
                || format!("sha-256+{}@bob.xmpp.org", hash(&bytes)) != asset.cid
            {
                return Err(Error("Inline artwork changed before loading."));
            }
            input.seek(SeekFrom::Start(0))?;
            this.check(&job)?;
            Ok((file, crate::downloads::VerifiedFile(input)))
        })
        .await
    }
}
