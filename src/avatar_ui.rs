use crate::{avatar::Source, model::Chat, protocol::Command, ui::Ui};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Avatars {
    widgets: HashMap<String, (Source, Vec<glib::WeakRef<adw::Avatar>>)>,
    cache: HashMap<String, (Instant, Option<gdk::Texture>)>,
    fetching: HashMap<String, (u64, gio::Cancellable)>,
    hints: HashMap<String, String>,
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
        }
        if let Some((_, cancel)) = self.fetching.get(peer) {
            cancel.cancel();
        }
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
        if !self.online.get() || !self.window.is_mapped() || self.on_welcome() {
            return;
        }
        let mut avatars = self.avatars.borrow_mut();
        avatars.widgets.retain(|_, (_, widgets)| {
            widgets.retain(|avatar| avatar.upgrade().is_some());
            !widgets.is_empty()
        });
        {
            let Avatars {
                hints,
                widgets,
                cache,
                ..
            } = &mut *avatars;
            hints.retain(|peer, _| widgets.contains_key(peer) || cache.contains_key(peer));
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
                    .is_some_and(|(expires, _)| *expires > Instant::now())
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
        let (reply, answer) = tokio::sync::oneshot::channel();
        if !self.command(Command::Avatar {
            peer: peer.clone(),
            source,
            reply,
        }) {
            return;
        }
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        let cancel = gio::Cancellable::new();
        let serial = {
            let mut avatars = self.avatars.borrow_mut();
            avatars.serial += 1;
            let serial = avatars.serial;
            avatars
                .fetching
                .insert(peer.clone(), (serial, cancel.clone()));
            serial
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = match answer.await {
                Ok(Ok(Some(image))) => gio::spawn_blocking(move || image.decode(&cancel))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map(Some),
                Ok(Ok(None)) => Some(None),
                _ => None,
            };
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
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
            let (_, request) = avatars.fetching.remove(&peer).unwrap();
            if request.is_cancelled() {
                return;
            }
            let successful = result.is_some();
            let image = result.unwrap_or_else(|| {
                avatars
                    .cache
                    .get(&peer)
                    .and_then(|(_, image)| image.clone())
            });
            let expires = Instant::now() + Duration::from_secs(if successful { 1800 } else { 45 });
            if avatars.cache.len() >= 256 && !avatars.cache.contains_key(&peer) {
                if let Some(oldest) = avatars
                    .cache
                    .iter()
                    .min_by_key(|(_, (expires, _))| *expires)
                    .map(|(peer, _)| peer.clone())
                {
                    avatars.cache.remove(&oldest);
                }
            }
            avatars.cache.insert(peer.clone(), (expires, image.clone()));
            if let Some((_, widgets)) = avatars.widgets.get(&peer) {
                for widget in widgets.iter().filter_map(|weak| weak.upgrade()) {
                    widget.set_custom_image(image.as_ref());
                }
            }
        });
    }
}
