use crate::{
    avatar::{Avatar, MAX_BYTES, Source},
    downloads,
    model::Chat,
    protocol::Command,
    ui::Ui,
};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, UNIX_EPOCH},
};

const FRESH_FOR: Duration = Duration::from_secs(1800);
const RETRY_AFTER: Duration = Duration::from_secs(45);
const MAX_ENTRIES: usize = 256;
const MAX_DISK_BYTES: u64 = 32 * 1024 * 1024;

fn avatar_path(owner: &str, peer: &str) -> Option<PathBuf> {
    let id: String = openssl::sha::sha256(peer.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(
        downloads::account_root(owner)
            .ok()?
            .join("avatars")
            .join(format!("{id}.avatar")),
    )
}

fn load_cached(
    path: &Path,
    source: Source,
    cancel: &gio::Cancellable,
) -> Option<(gdk::Texture, Duration)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if cancel.is_cancelled()
        || !metadata.is_file()
        || !(2..=MAX_BYTES as u64 + 1).contains(&metadata.len())
    {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut format = [0];
    file.read_exact(&mut format).ok()?;
    if format[0] > 1 || (source == Source::VCard && format[0] != 0) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }
    let texture = Avatar::new(bytes, format[0] == 1).decode(cancel).ok()?;
    let remaining = metadata
        .modified()
        .ok()
        .and_then(|time| time.elapsed().ok())
        .map_or(Duration::ZERO, |age| FRESH_FOR.saturating_sub(age));
    Some((texture, remaining))
}

fn write_cached(path: &Path, avatar: Option<&Avatar>, cancel: &gio::Cancellable) -> io::Result<()> {
    let _guard = downloads::CACHE_IO.lock().unwrap();
    if cancel.is_cancelled() {
        return Ok(());
    }
    let Some(avatar) = avatar else {
        return match fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    };
    let dir = path.parent().unwrap();
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&[u8::from(avatar.pep)])?;
        file.write_all(avatar.bytes.as_ref())?;
        if !cancel.is_cancelled() {
            fs::rename(&temp, path)?;
            trim_disk_cache(dir);
        }
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}

fn trim_disk_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?;
            if path.extension()?.to_str()? != "avatar"
                || stem.len() != 64
                || !stem.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                    metadata.len(),
                    path,
                )
            })
        })
        .collect();
    let mut bytes: u64 = files.iter().map(|(_, size, _)| size).sum();
    let mut count = files.len();
    if bytes <= MAX_DISK_BYTES && count <= MAX_ENTRIES {
        return;
    }
    files.sort_unstable_by_key(|(time, _, _)| *time);
    for (_, size, path) in files {
        if bytes <= MAX_DISK_BYTES && count <= MAX_ENTRIES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            bytes -= size;
            count -= 1;
        }
    }
}

#[derive(Default)]
pub struct Avatars {
    widgets: HashMap<String, (Source, Vec<glib::WeakRef<adw::Avatar>>)>,
    cache: HashMap<String, (Instant, Option<gdk::Texture>)>,
    fetching: HashMap<String, (u64, gio::Cancellable)>,
    hints: HashMap<String, String>,
    invalidated: HashSet<String>,
    serial: u64,
}
impl Avatars {
    pub fn clear(&mut self) {
        for (_, cancel) in self.fetching.values() {
            cancel.cancel();
        }
        self.fetching.clear();
        self.cache.clear();
        self.widgets.clear();
        self.hints.clear();
        self.invalidated.clear();
    }
    pub fn invalidate(&mut self, peer: &str, hint: Option<String>) {
        if !self.widgets.contains_key(peer) && !self.cache.contains_key(peer) {
            return;
        }
        if let Some(hint) = hint {
            if self.hints.get(peer) == Some(&hint) {
                return;
            }
            self.hints.insert(peer.into(), hint);
        }
        if let Some((expires, _)) = self.cache.get_mut(peer) {
            *expires = Instant::now();
        } else {
            self.invalidated.insert(peer.into());
        }
        if let Some((_, cancel)) = self.fetching.get(peer) {
            cancel.cancel();
        }
    }

    fn set(&mut self, peer: &str, texture: Option<gdk::Texture>, valid_for: Duration) {
        if self.cache.len() >= MAX_ENTRIES && !self.cache.contains_key(peer) {
            if let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, (expires, _))| *expires)
                .map(|(peer, _)| peer.clone())
            {
                self.cache.remove(&oldest);
            }
        }
        if let Some((_, widgets)) = self.widgets.get(peer) {
            for widget in widgets.iter().filter_map(|weak| weak.upgrade()) {
                widget.set_custom_image(texture.as_ref());
            }
        }
        self.cache
            .insert(peer.into(), (Instant::now() + valid_for, texture));
    }
}

impl Ui {
    pub fn chat_avatar(&self, chat: &Chat, size: i32) -> adw::Avatar {
        let avatar = crate::widgets::avatar(chat, size);
        let owner = &self.state.borrow().account;
        if owner.is_empty() || chat.id == "saved" || chat.jid.is_empty() {
            return avatar;
        }
        let mut avatars = self.avatars.borrow_mut();
        if let Some((_, texture)) = avatars.cache.get(&chat.jid) {
            avatar.set_custom_image(texture.as_ref());
        }
        let source = if chat.group {
            Source::VCard
        } else {
            Source::Pep
        };
        avatars
            .widgets
            .entry(chat.jid.clone())
            .or_insert_with(|| (source, Vec::new()))
            .1
            .push(avatar.downgrade());
        avatar
    }

    pub fn poll_avatars(self: &Rc<Self>) {
        if !self.window.is_mapped() || self.on_welcome() {
            return;
        }
        let online = self.online.get();
        let now = Instant::now();
        let mut avatars = self.avatars.borrow_mut();
        avatars.widgets.retain(|_, (_, widgets)| {
            widgets.retain(|avatar| avatar.upgrade().is_some());
            !widgets.is_empty()
        });
        {
            let Avatars {
                hints,
                invalidated,
                widgets,
                cache,
                ..
            } = &mut *avatars;
            hints.retain(|peer, _| widgets.contains_key(peer) || cache.contains_key(peer));
            invalidated.retain(|peer| widgets.contains_key(peer));
        }
        let capacity = 4usize.saturating_sub(avatars.fetching.len());
        if capacity == 0 {
            return;
        }
        let mut visible = Vec::new();
        for (peer, (source, widgets)) in &avatars.widgets {
            if avatars.fetching.contains_key(peer)
                || avatars
                    .cache
                    .get(peer)
                    .is_some_and(|(expires, _)| !online || *expires > now)
            {
                continue;
            }
            let top = widgets
                .iter()
                .filter_map(|weak| weak.upgrade())
                .filter(|avatar| avatar.is_mapped())
                .filter_map(|avatar| avatar.compute_bounds(&self.window))
                .filter(|bounds| {
                    bounds.y() + bounds.height() > 0. && bounds.y() < self.window.height() as f32
                })
                .map(|bounds| bounds.y() as i32)
                .min();
            if let Some(top) = top {
                visible.push((top, peer.clone(), *source));
            }
        }
        drop(avatars);
        visible.sort_by_key(|(top, _, _)| *top);
        for (_, peer, source) in visible.into_iter().take(capacity) {
            self.fetch_avatar(peer, source);
        }
    }

    fn fetch_avatar(self: &Rc<Self>, peer: String, source: Source) {
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        let cancel = gio::Cancellable::new();
        let (serial, load_disk) = {
            let mut avatars = self.avatars.borrow_mut();
            avatars.serial += 1;
            let serial = avatars.serial;
            avatars
                .fetching
                .insert(peer.clone(), (serial, cancel.clone()));
            (serial, !avatars.cache.contains_key(&peer))
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let current = || {
                weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation && ui.state.borrow().account == owner
                })
            };
            let path = avatar_path(&owner, &peer);
            let stored = if load_disk {
                let path = path.clone();
                let cancel = cancel.clone();
                gio::spawn_blocking(move || load_cached(path.as_deref()?, source, &cancel))
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            let answer = {
                let Some(ui) = current() else {
                    return;
                };
                let mut avatars = ui.avatars.borrow_mut();
                if avatars
                    .fetching
                    .get(&peer)
                    .is_none_or(|(expected, _)| *expected != serial)
                {
                    return;
                }
                if cancel.is_cancelled() {
                    avatars.fetching.remove(&peer);
                    return;
                }
                if load_disk {
                    let (texture, mut remaining) = stored
                        .map_or((None, Duration::ZERO), |(texture, remaining)| {
                            (Some(texture), remaining)
                        });
                    if avatars.invalidated.remove(&peer) {
                        remaining = Duration::ZERO;
                    }
                    avatars.set(&peer, texture, remaining);
                }
                if !ui.online.get()
                    || avatars
                        .cache
                        .get(&peer)
                        .is_some_and(|(expires, _)| *expires > Instant::now())
                {
                    avatars.fetching.remove(&peer);
                    return;
                }
                let (reply, answer) = tokio::sync::oneshot::channel();
                if !ui.command(Command::Avatar {
                    peer: peer.clone(),
                    source,
                    reply,
                }) {
                    avatars.fetching.remove(&peer);
                    if let Some((expires, _)) = avatars.cache.get_mut(&peer) {
                        *expires = Instant::now() + RETRY_AFTER;
                    }
                    return;
                }
                answer
            };
            let result = match answer.await {
                Ok(Ok(image)) => {
                    let cancel = cancel.clone();
                    gio::spawn_blocking(move || {
                        let texture = match &image {
                            Some(image) => Some(image.decode(&cancel).ok()?),
                            None => None,
                        };
                        if let Some(path) = path.as_deref() {
                            let _ = write_cached(path, image.as_ref(), &cancel);
                        }
                        Some(texture)
                    })
                    .await
                    .ok()
                    .flatten()
                }
                _ => None,
            };
            let Some(ui) = current() else {
                return;
            };
            let mut avatars = ui.avatars.borrow_mut();
            if avatars
                .fetching
                .get(&peer)
                .is_none_or(|(expected, _)| *expected != serial)
            {
                return;
            }
            avatars.fetching.remove(&peer);
            if cancel.is_cancelled() {
                return;
            }
            if let Some(texture) = result {
                avatars.set(&peer, texture, FRESH_FOR);
            } else if let Some((expires, _)) = avatars.cache.get_mut(&peer) {
                *expires = Instant::now() + RETRY_AFTER;
            }
        });
    }
}
