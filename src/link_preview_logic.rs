//! Account-owned preview jobs; GTK keeps cached cards while optional I/O runs off-thread.
use crate::{
    link_previews::{self, Permit, Preview, Privacy},
    model::Message,
    storage::Store,
    ui::Ui,
};
use gtk::glib;
use rusqlite::params;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Entry {
    preview: Option<Preview>,
    expires: i64,
    used: u64,
}
#[derive(Clone)]
struct Job {
    id: u64,
    epoch: u64,
    owner: String,
    url: String,
    permit: Option<Permit>,
    db: PathBuf,
}
#[derive(Default)]
pub struct Jobs {
    owner: String,
    permissions: (bool, bool),
    saving: bool,
    policy: Option<Privacy>,
    epoch: u64,
    next: u64,
    clock: u64,
    cache: HashMap<String, Entry>,
    queue: VecDeque<Job>,
    pending: HashMap<(String, String), u64>,
    active: HashSet<u64>,
    history_pending: Option<HistoryWindow>,
    history_idle: bool,
    history_active: Option<(u64, Permit)>,
    history_attempts: VecDeque<HistoryAttempt>,
}
const CACHE_LIMIT: usize = 256;
const QUEUE_LIMIT: usize = 150;
#[derive(Clone)]
struct HistoryWindow {
    peer: String,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    encrypted: bool,
}
impl HistoryWindow {
    fn contains(&self, peer: &str, time: chrono::DateTime<chrono::Utc>) -> bool {
        self.peer == peer && self.start <= time && time <= self.end
    }
    fn overlaps(&self, other: &Self) -> bool {
        self.peer == other.peer && self.start <= other.end && other.start <= self.end
    }
}
struct HistoryAttempt {
    id: u64,
    window: HistoryWindow,
    attempts: u8,
    success: bool,
    active: bool,
    retry_after: Instant,
}
impl Jobs {
    fn cached(&mut self, url: &str) -> Option<Entry> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.cache.get_mut(url)?;
        entry.used = self.clock;
        Some(entry.clone())
    }
    fn remember(&mut self, url: String, mut entry: Entry) {
        self.clock = self.clock.wrapping_add(1);
        entry.used = self.clock;
        while self.cache.len() >= CACHE_LIMIT && !self.cache.contains_key(&url) {
            let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(url, _)| url.clone())
            else {
                break;
            };
            self.cache.remove(&oldest);
        }
        self.cache.insert(url, entry);
    }
    fn enqueue(&mut self, job: Job, visible: bool) {
        let key = (job.owner.clone(), job.url.clone());
        if self.pending.contains_key(&key) {
            if visible
                && let Some(index) = self
                    .queue
                    .iter()
                    .position(|queued| queued.owner == job.owner && queued.url == job.url)
            {
                let queued = self.queue.remove(index).unwrap();
                self.queue.push_front(queued);
            }
            return;
        }
        // A completed task rescans the viewport, so deferred visible rows cannot be lost.
        if self.queue.len() >= QUEUE_LIMIT {
            if !visible {
                return;
            }
            if let Some(displaced) = self.queue.pop_back() {
                self.pending.remove(&(displaced.owner, displaced.url));
                if let Some(permit) = displaced.permit {
                    permit.cancel();
                }
            }
        }
        self.pending.insert(key, job.id);
        if visible {
            self.queue.push_front(job);
        } else {
            self.queue.push_back(job);
        }
    }
    fn note_history(
        &mut self,
        peer: &str,
        time: chrono::DateTime<chrono::Utc>,
        encrypted: bool,
    ) -> bool {
        let now = Instant::now();
        if self.history_attempts.iter().any(|attempt| {
            attempt.window.contains(peer, time)
                && (attempt.active
                    || attempt.success
                    || attempt.attempts >= 3
                    || now < attempt.retry_after)
        }) {
            return false;
        }
        let Some(start) = time.checked_sub_signed(chrono::Duration::seconds(1)) else {
            return false;
        };
        let Some(end) = time.checked_add_signed(chrono::Duration::seconds(1)) else {
            return false;
        };
        if let Some((id, permit)) = &self.history_active
            && self
                .history_attempts
                .iter()
                .any(|attempt| attempt.id == *id && attempt.window.peer != peer)
        {
            permit.cancel();
        }
        if let Some(window) = self
            .history_pending
            .as_mut()
            .filter(|window| window.peer == peer)
        {
            window.start = window.start.min(start);
            window.end = window.end.max(end);
            window.encrypted |= encrypted;
        } else {
            self.history_pending = Some(HistoryWindow {
                peer: peer.into(),
                start,
                end,
                encrypted,
            });
        }
        true
    }
    fn cancel_history(&mut self) {
        if let Some((_, permit)) = self.history_active.take() {
            permit.cancel();
        }
        self.history_pending = None;
        self.history_idle = false;
        self.history_attempts.retain(|attempt| !attempt.active);
    }
    pub fn cancel(&mut self) {
        if let Some(policy) = self.policy.take() {
            policy.cancel();
        }
        SETTINGS_SERIAL.fetch_add(1, Ordering::AcqRel);
        self.cancel_history();
        self.history_attempts.clear();
        self.epoch = self.epoch.wrapping_add(1);
        self.saving = false;
        self.owner.clear();
        self.cache.clear();
        self.queue.clear();
        self.pending.retain(|_, id| self.active.contains(id));
    }
    fn initialise(&mut self, owner: &str, permissions: (bool, bool)) {
        if self.owner != owner {
            self.cancel();
            self.owner = owner.into();
            self.permissions = permissions;
            self.policy = Some(Privacy::new(owner, permissions.0, permissions.1));
        }
    }
}
impl Drop for Jobs {
    fn drop(&mut self) {
        self.cancel();
    }
}
static SETTINGS_SERIAL: AtomicU64 = AtomicU64::new(0);
static SETTINGS_WRITER: Mutex<()> = Mutex::new(());

fn save_permissions(
    store: &Store,
    owner: &str,
    enabled: bool,
    encrypted: bool,
    serial: u64,
) -> Result<(), String> {
    let _writer = SETTINGS_WRITER
        .lock()
        .map_err(|_| "Link preview settings could not be saved")?;
    if SETTINGS_SERIAL.load(Ordering::Acquire) != serial {
        return Err("Link preview settings changed".into());
    }
    let transaction = store
        .db
        .unchecked_transaction()
        .map_err(|_| "Link preview settings could not be saved")?;
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE jid=?)",
            [owner],
            |row| row.get(0),
        )
        .map_err(|_| "Account settings are unavailable")?;
    if !exists {
        return Err("This account is no longer available".into());
    }
    for (key, value) in [
        (format!("link-previews-enabled:{owner}"), enabled),
        (format!("link-previews-encrypted:{owner}"), encrypted),
    ] {
        transaction.execute("INSERT INTO preferences(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, if value {"true"} else {"false"}]).map_err(|_| "Link preview settings could not be saved")?;
    }
    if SETTINGS_SERIAL.load(Ordering::Acquire) != serial {
        return Err("Link preview settings changed".into());
    }
    transaction
        .commit()
        .map_err(|_| String::from("Link preview settings could not be saved"))
}
fn effective_permissions(
    previous: (bool, bool),
    wanted: (bool, bool),
    saved: bool,
) -> (bool, bool) {
    if saved {
        wanted
    } else {
        (previous.0 && wanted.0, previous.1 && wanted.1)
    }
}
fn executor() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("monogram-preview")
            .enable_all()
            .build()
            .expect("preview executor")
    })
}
fn permissions(store: &Store, owner: &str) -> (bool, bool) {
    (
        store.pref(&format!("link-previews-enabled:{owner}"), "false") == "true",
        store.pref(&format!("link-previews-encrypted:{owner}"), "false") == "true",
    )
}
fn open_store(path: &Path, write: bool) -> rusqlite::Result<Store> {
    let flags = if write {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = rusqlite::Connection::open_with_flags(
        path,
        flags | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(250))?;
    if write {
        db.pragma_update(None, "foreign_keys", true)?;
    }
    Ok(Store { db })
}
async fn load(job: &Job) -> Option<Entry> {
    let (owner, url, db) = (job.owner.clone(), job.url.clone(), job.db.clone());
    let mut cached = tokio::task::spawn_blocking(move || {
        let store = open_store(&db, false).ok()?;
        Some(Entry {
            preview: store.cached_link_preview(&owner, &url),
            expires: store.link_preview_expiry(&owner, &url).unwrap_or(0),
            used: 0,
        })
    })
    .await
    .ok()
    .flatten()?;
    if cached.expires > link_previews::now_ms() {
        return Some(cached);
    }
    let Some(permit) = &job.permit else {
        return Some(cached);
    };
    if !permit.allowed() {
        return None;
    }
    let fetched = link_previews::fetch(&job.url, permit).await.ok();
    if !permit.allowed() {
        return None;
    }
    let (owner, url, permit, db) = (
        job.owner.clone(),
        job.url.clone(),
        permit.clone(),
        job.db.clone(),
    );
    let result = tokio::task::spawn_blocking(move || {
        let store = open_store(&db, true).ok()?;
        if !store
            .save_link_preview(
                &owner,
                &url,
                fetched.as_ref(),
                link_previews::now_ms(),
                &permit,
            )
            .ok()?
        {
            return None;
        }
        Some((
            fetched,
            store.link_preview_expiry(&owner, &url).unwrap_or(0),
        ))
    })
    .await
    .ok()
    .flatten();
    if let Some((preview, expires)) = result {
        cached.preview = preview.or(cached.preview);
        cached.expires = expires;
    } else {
        // A cache failure must not cause a request storm while the chat stays open.
        cached.expires = link_previews::now_ms() + 60 * 60 * 1000;
    }
    Some(cached)
}
impl Ui {
    pub fn link_preview_permissions(&self) -> (bool, bool) {
        let owner = self.state.borrow().account.clone();
        let jobs = self.link_preview_jobs.borrow();
        if jobs.owner == owner && jobs.policy.is_some() {
            jobs.permissions
        } else {
            permissions(&self.store, &owner)
        }
    }
    pub fn set_link_preview_permissions(
        self: &Rc<Self>,
        enabled: bool,
        encrypted: bool,
    ) -> impl std::future::Future<Output = Result<(), String>> + 'static {
        let owner = self.state.borrow().account.clone();
        let weak = Rc::downgrade(self);
        let prepared = (|| -> Result<_, String> {
            if owner.is_empty() {
                return Err("Sign in before changing link previews".into());
            }
            let previous = self.link_preview_permissions();
            let db = self
                .store
                .db
                .path()
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .ok_or("Link preview settings are unavailable")?;
            let mut jobs = self.link_preview_jobs.borrow_mut();
            jobs.initialise(&owner, previous);
            if jobs.saving {
                return Err("A link preview setting is still being saved".into());
            }
            jobs.saving = true;
            jobs.permissions = (enabled, encrypted);
            if let Some(policy) = &jobs.policy {
                policy.set_permissions(false, false);
            }
            jobs.epoch = jobs.epoch.wrapping_add(1);
            jobs.cancel_history();
            jobs.history_attempts.retain(|attempt| attempt.success);
            jobs.queue.clear();
            let active = jobs.active.clone();
            jobs.pending.retain(|_, id| active.contains(id));
            let serial = SETTINGS_SERIAL.fetch_add(1, Ordering::AcqRel) + 1;
            Ok((previous, jobs.epoch, serial, db))
        })();
        async move {
            let (previous, epoch, serial, db) = prepared?;
            let account = owner.clone();
            let result = gtk::gio::spawn_blocking(move || -> Result<(), String> {
                let store = open_store(&db, true)
                    .map_err(|_| "Link preview settings could not be saved")?;
                save_permissions(&store, &account, enabled, encrypted, serial)
            })
            .await
            .unwrap_or_else(|_| Err("Link preview settings could not be saved".into()));
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == owner)
            else {
                return Err("The active account changed".into());
            };
            {
                let mut jobs = ui.link_preview_jobs.borrow_mut();
                if jobs.owner != owner || jobs.epoch != epoch {
                    return Err("Link preview settings changed".into());
                }
                jobs.saving = false;
                let permissions =
                    effective_permissions(previous, (enabled, encrypted), result.is_ok());
                jobs.permissions = permissions;
                if let Some(policy) = &jobs.policy {
                    policy.set_permissions(permissions.0, permissions.1);
                }
            }
            ui.prepare_link_previews();
            result
        }
    }
    pub fn link_preview_for_message(self: &Rc<Self>, message: &Message) -> Option<Preview> {
        let Some(url) = link_previews::candidate(message) else {
            self.request_preview_history(message, false);
            return None;
        };
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return None;
        }
        let permissions = self.link_preview_permissions();
        let visible = self
            .bubbles
            .borrow()
            .get(&message.id)
            .is_some_and(|widget| self.in_chat_viewport(widget));
        let cached = {
            let mut jobs = self.link_preview_jobs.borrow_mut();
            jobs.initialise(&owner, permissions);
            let entry = jobs.cached(&url);
            let preview = entry.as_ref().and_then(|entry| entry.preview.clone());
            let acknowledged = !message.own
                || matches!(message.delivery.as_str(), "sent" | "delivered" | "read")
                || (message.delivery == "stored" && self.state.borrow().current().jid == owner);
            let permit = acknowledged
                .then(|| {
                    jobs.policy
                        .as_ref()
                        .and_then(|policy| policy.ticket(message.encrypted))
                })
                .flatten();
            let refresh = entry.is_none()
                || permit.is_some()
                    && entry
                        .as_ref()
                        .is_some_and(|entry| entry.expires <= link_previews::now_ms());
            if refresh && let Some(db) = self.store.db.path().filter(|path| !path.is_empty()) {
                jobs.next = jobs.next.wrapping_add(1);
                let job = Job {
                    id: jobs.next,
                    epoch: jobs.epoch,
                    owner,
                    url,
                    db: PathBuf::from(db),
                    // Show a persisted stale card before starting optional network refresh.
                    permit: if entry.is_none() { None } else { permit },
                };
                jobs.enqueue(job, visible);
            }
            preview
        };
        self.pump_link_previews();
        cached
    }
    fn pump_link_previews(self: &Rc<Self>) {
        loop {
            let job = {
                let mut jobs = self.link_preview_jobs.borrow_mut();
                if jobs.active.len() >= 2 {
                    return;
                }
                let Some(job) = jobs.queue.pop_front() else {
                    return;
                };
                jobs.active.insert(job.id);
                job
            };
            let (reply, receive) = tokio::sync::oneshot::channel();
            let work = job.clone();
            executor().spawn(async move {
                let result = load(&work).await;
                let _ = reply.send(result);
            });
            let weak = Rc::downgrade(self);
            glib::MainContext::default().spawn_local(async move {
                let result = receive.await.ok().flatten();
                let Some(ui) = weak.upgrade() else { return };
                let current = {
                    let mut jobs = ui.link_preview_jobs.borrow_mut();
                    jobs.active.remove(&job.id);
                    let key = (job.owner.clone(), job.url.clone());
                    if jobs.pending.get(&key) == Some(&job.id) {
                        jobs.pending.remove(&key);
                    }
                    let current = jobs.owner == job.owner
                        && jobs.epoch == job.epoch
                        && job.permit.as_ref().is_none_or(Permit::allowed);
                    if current {
                        let preview = jobs
                            .cache
                            .get(&job.url)
                            .and_then(|entry| entry.preview.clone());
                        let entry = result.unwrap_or(Entry {
                            preview,
                            expires: link_previews::now_ms() + 60 * 60 * 1000,
                            used: 0,
                        });
                        jobs.remember(job.url.clone(), entry);
                    }
                    current
                };
                if ui.state.borrow().account == job.owner {
                    ui.refresh_link_preview_rows(&job.url);
                    if !current {
                        ui.prepare_loaded_link_previews();
                    }
                }
                ui.prepare_visible_link_previews();
                ui.pump_link_previews();
            });
        }
    }
    fn prepare_visible_link_previews(self: &Rc<Self>) {
        let messages = self
            .state
            .borrow()
            .current()
            .messages
            .iter()
            .filter(|message| {
                self.bubbles
                    .borrow()
                    .get(&message.id)
                    .is_some_and(|widget| self.in_chat_viewport(widget))
            })
            .cloned()
            .collect::<Vec<_>>();
        for message in messages {
            self.link_preview_for_message(&message);
        }
    }
    pub(crate) fn prepare_loaded_link_previews(self: &Rc<Self>) {
        self.prepare_visible_link_previews();
        let messages: Vec<_> = self
            .state
            .borrow()
            .current()
            .messages
            .iter()
            .rev()
            .take(150)
            .cloned()
            .collect();
        for message in messages {
            self.link_preview_for_message(&message);
        }
    }
    pub(crate) fn prepare_link_previews(self: &Rc<Self>) {
        self.prepare_loaded_link_previews();
        let (owner, peer) = {
            let state = self.state.borrow();
            (state.account.clone(), state.current().jid.clone())
        };
        let Some(db) = self
            .store
            .db
            .path()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let (account, address) = (owner.clone(), peer.clone());
            let messages = gtk::gio::spawn_blocking(move || {
                open_store(&db, false).map(|store| store.history(&account, &address, None, 150))
            })
            .await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                let state = ui.state.borrow();
                state.account == owner && state.current().jid == peer
            }) else {
                return;
            };
            if let Ok(Ok(messages)) = messages {
                for message in messages {
                    ui.request_preview_history(&message, true);
                    ui.link_preview_for_message(&message);
                }
            }
        });
    }
}

impl Ui {
    fn request_preview_history(self: &Rc<Self>, message: &Message, latest: bool) {
        if !self.online.get()
            || message
                .preview_spoilers
                .as_ref()
                .is_some_and(|context| context.matches(&message.text))
        {
            return;
        }
        // Eligibility probing never authorizes HTTP: only recovered, hash-bound metadata can do that.
        let mut probe = message.clone();
        probe.preview_spoilers = Some(crate::spoilers::Context::clear());
        if link_previews::candidate(&probe).is_none() {
            return;
        }
        let Ok(time) = chrono::DateTime::parse_from_rfc3339(&message.timestamp)
            .map(|time| time.with_timezone(&chrono::Utc))
        else {
            return;
        };
        let (owner, peer, in_latest) = {
            let state = self.state.borrow();
            (
                state.account.clone(),
                state.current().jid.clone(),
                state
                    .current()
                    .messages
                    .iter()
                    .rev()
                    .take(150)
                    .any(|row| row.id == message.id),
            )
        };
        let visible = self
            .bubbles
            .borrow()
            .get(&message.id)
            .is_some_and(|widget| self.in_chat_viewport(widget));
        if owner.is_empty() || !latest && !in_latest && !visible {
            return;
        }
        let permissions = self.link_preview_permissions();
        let epoch = {
            let mut jobs = self.link_preview_jobs.borrow_mut();
            jobs.initialise(&owner, permissions);
            if jobs
                .policy
                .as_ref()
                .and_then(|policy| policy.ticket(message.encrypted))
                .is_none()
            {
                return;
            }
            if !jobs.note_history(&peer, time, message.encrypted) {
                return;
            }
            if jobs.history_idle {
                return;
            }
            jobs.history_idle = true;
            jobs.epoch
        };
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == owner)
            else {
                return;
            };
            {
                let mut jobs = ui.link_preview_jobs.borrow_mut();
                if jobs.owner != owner || jobs.epoch != epoch {
                    return;
                }
                jobs.history_idle = false;
            }
            ui.pump_preview_history();
        });
    }
    fn pump_preview_history(self: &Rc<Self>) {
        let (owner, peer) = {
            let state = self.state.borrow();
            (state.account.clone(), state.current().jid.clone())
        };
        if !self.online.get() {
            return;
        }
        let generation = self.generation.get();
        let (id, epoch, window, permit) = {
            let mut jobs = self.link_preview_jobs.borrow_mut();
            if jobs.owner != owner || jobs.history_active.is_some() {
                return;
            }
            let Some(window) = jobs
                .history_pending
                .take()
                .filter(|window| window.peer == peer)
            else {
                return;
            };
            let Some(permit) = jobs
                .policy
                .as_ref()
                .and_then(|policy| policy.ticket(window.encrypted))
            else {
                return;
            };
            let attempts = jobs
                .history_attempts
                .iter()
                .filter(|attempt| !attempt.success && attempt.window.overlaps(&window))
                .map(|attempt| attempt.attempts)
                .max()
                .unwrap_or(0)
                + 1;
            if attempts > 3 {
                return;
            }
            jobs.history_attempts
                .retain(|attempt| attempt.success || !attempt.window.overlaps(&window));
            jobs.next = jobs.next.wrapping_add(1);
            let id = jobs.next;
            jobs.history_attempts.push_back(HistoryAttempt {
                id,
                window: window.clone(),
                attempts,
                success: false,
                active: true,
                retry_after: Instant::now(),
            });
            while jobs.history_attempts.len() > 64 {
                jobs.history_attempts.pop_front();
            }
            jobs.history_active = Some((id, permit.clone()));
            (id, jobs.epoch, window, permit)
        };
        let (reply, mut receive) = tokio::sync::oneshot::channel();
        self.command(crate::protocol::Command::PreviewHistory {
            peer: window.peer.clone(),
            start: window
                .start
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            end: window
                .end
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            reply,
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let deadline = Instant::now() + Duration::from_secs(65);
            let result = loop {
                if !permit.allowed() {
                    break None;
                }
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let current = {
                    let jobs = ui.link_preview_jobs.borrow();
                    ui.state.borrow().account == owner && jobs.owner == owner && jobs.epoch == epoch
                };
                let same_generation = ui.generation.get() == generation;
                drop(ui);
                if !current {
                    break None;
                }
                if !same_generation || Instant::now() >= deadline {
                    break Some(false);
                }
                match receive.try_recv() {
                    Ok(success) => break Some(success),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break Some(false),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                }
                glib::timeout_future(Duration::from_millis(150)).await;
            };
            // Closing this receiver also cancels the backend's paged metadata-only request.
            drop(receive);
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.state.borrow().account == owner)
            else {
                return;
            };
            let retry = {
                let mut jobs = ui.link_preview_jobs.borrow_mut();
                if jobs.history_active.as_ref().map(|active| active.0) != Some(id) {
                    return;
                }
                jobs.history_active.take();
                let current = jobs.owner == owner && jobs.epoch == epoch;
                if !current || result.is_none() {
                    jobs.history_attempts.retain(|attempt| attempt.id != id);
                    None
                } else if let Some(attempt) = jobs
                    .history_attempts
                    .iter_mut()
                    .find(|attempt| attempt.id == id)
                {
                    attempt.active = false;
                    attempt.success = result == Some(true);
                    let delay = Duration::from_secs(30u64 << (attempt.attempts - 1));
                    attempt.retry_after = Instant::now() + delay;
                    (!attempt.success && attempt.attempts < 3).then_some(delay)
                } else {
                    None
                }
            };
            ui.pump_preview_history();
            if let Some(delay) = retry {
                let weak = Rc::downgrade(&ui);
                let peer = window.peer.clone();
                glib::timeout_add_local_once(delay, move || {
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        let state = ui.state.borrow();
                        state.account == owner && state.current().jid == peer
                    }) else {
                        return;
                    };
                    if ui.link_preview_jobs.borrow().epoch == epoch {
                        ui.prepare_loaded_link_previews();
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_cache_queue_recovery_and_atomic_settings() {
        let owner = "controller@example.invalid";
        {
            let mut jobs = Jobs::default();
            jobs.initialise(owner, (true, true));
            let entry = || Entry {
                preview: None,
                expires: 123,
                used: 0,
            };
            for i in 0..256 {
                jobs.remember(format!("url-{i}"), entry());
            }
            jobs.cached("url-0");
            jobs.remember("extra".into(), entry());
            assert_eq!(jobs.cache.len(), 256);
            assert!(jobs.cache.contains_key("url-0"));
            assert!(!jobs.cache.contains_key("url-1"));
            let job = |id, url: String| Job {
                id,
                epoch: jobs.epoch,
                owner: owner.into(),
                url,
                permit: Some(Privacy::new(owner, true, true).ticket(false).unwrap()),
                db: PathBuf::from("/unused-fixture.sqlite"),
            };
            let queued = (0..200)
                .map(|i| job(i, format!("queued-{i}")))
                .collect::<Vec<_>>();
            for item in queued {
                jobs.enqueue(item, false);
            }
            assert_eq!((jobs.queue.len(), jobs.pending.len()), (150, 150));
            let displaced = jobs.queue.back().unwrap().clone();
            let displaced_permit = displaced.permit.clone().unwrap();
            jobs.enqueue(
                Job {
                    id: 1000,
                    epoch: jobs.epoch,
                    owner: owner.into(),
                    url: "visible".into(),
                    permit: None,
                    db: PathBuf::new(),
                },
                true,
            );
            assert_eq!(jobs.queue.len(), 150);
            assert_eq!(jobs.queue.front().unwrap().url, "visible");
            assert!(!jobs.pending.contains_key(&(owner.into(), displaced.url)));
            assert!(!displaced_permit.allowed());
            let time = chrono::DateTime::parse_from_rfc3339("2026-11-11T12:00:00.123Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
            assert!(jobs.note_history("peer@example.invalid", time, false));
            assert!(jobs.note_history(
                "peer@example.invalid",
                time + chrono::Duration::seconds(10),
                true
            ));
            let window = jobs.history_pending.take().unwrap();
            assert_eq!(window.start, time - chrono::Duration::seconds(1));
            assert_eq!(window.end, time + chrono::Duration::seconds(11));
            assert!(window.encrypted);
            jobs.history_attempts.push_back(HistoryAttempt {
                id: 2000,
                window: window.clone(),
                attempts: 1,
                success: true,
                active: false,
                retry_after: Instant::now(),
            });
            assert!(
                !jobs.note_history(&window.peer, time, false),
                "a completed window must not loop on unrecoverable metadata"
            );
            jobs.history_attempts.back_mut().unwrap().success = false;
            jobs.history_attempts.back_mut().unwrap().retry_after =
                Instant::now() + Duration::from_secs(30);
            assert!(
                !jobs.note_history(&window.peer, time, false),
                "failed queries must back off"
            );
            jobs.history_attempts.back_mut().unwrap().retry_after =
                Instant::now() - Duration::from_secs(1);
            jobs.history_attempts.back_mut().unwrap().attempts = 3;
            assert!(
                !jobs.note_history(&window.peer, time, false),
                "bounded retries"
            );
            let permit = jobs.policy.as_ref().unwrap().ticket(false).unwrap();
            jobs.cancel();
            assert!(!permit.allowed());
            assert!(jobs.cache.is_empty());
            assert!(jobs.queue.is_empty());
            assert!(jobs.history_pending.is_none());
        }
        assert_eq!(
            effective_permissions((false, true), (true, false), false),
            (false, false),
            "a failed combined save may revoke, but cannot grant"
        );
        let store = Store::open(true).unwrap();
        store
            .save_account(&crate::model::Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let enabled = format!("link-previews-enabled:{owner}");
        let encrypted = format!("link-previews-encrypted:{owner}");
        store.set_pref(&enabled, "false").unwrap();
        store.set_pref(&encrypted, "false").unwrap();
        store.db.execute_batch(&format!("CREATE TEMP TRIGGER fail_second_preview_setting BEFORE INSERT ON preferences WHEN NEW.key='{encrypted}' BEGIN SELECT RAISE(ABORT,'fixture'); END;")).unwrap();
        let serial = SETTINGS_SERIAL.fetch_add(1, Ordering::AcqRel) + 1;
        assert!(save_permissions(&store, owner, true, true, serial).is_err());
        assert_eq!(
            permissions(&store, owner),
            (false, false),
            "both preferences roll back together"
        );
        store
            .db
            .execute_batch("DROP TRIGGER fail_second_preview_setting")
            .unwrap();
        save_permissions(&store, owner, true, true, serial).unwrap();
        assert_eq!(permissions(&store, owner), (true, true));
        SETTINGS_SERIAL.fetch_add(1, Ordering::AcqRel);
        assert!(save_permissions(&store, owner, false, false, serial).is_err());
        assert_eq!(
            permissions(&store, owner),
            (true, true),
            "stale callbacks cannot commit"
        );
    }
}
