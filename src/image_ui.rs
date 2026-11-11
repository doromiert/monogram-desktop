//! Small, account-owned texture cache. Decode and verification never run on GTK.
use crate::{file_share::Attachment, ui::Ui};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone)]
pub struct Image {
    pub texture: gdk::Texture,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}
struct Task {
    cancel: gio::Cancellable,
    stopped: Arc<AtomicBool>,
    priority: u8,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.cancel.cancel();
    }
}
#[derive(Default)]
pub struct Images {
    tasks: HashMap<(String, u32), Task>,
    cache: HashMap<(String, u32), (u64, Result<Image, &'static str>)>,
    clock: u64,
    remembered: HashSet<(String, String, String)>,
    disk_checked: HashSet<(String, u32)>,
    pub(crate) videos: VecDeque<(String, Rc<crate::video_ui::Playback>)>,
    pub(crate) video_tasks: HashSet<String>,
    pub(crate) video_failures: HashMap<String, &'static str>,
}
impl Images {
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.cache.clear();
        self.remembered.clear();
        self.disk_checked.clear();
        self.videos.clear();
        self.video_tasks.clear();
        self.video_failures.clear();
    }
    pub fn invalidate(&mut self, file: &Attachment) {
        let key = file.key();
        self.tasks.retain(|(k, _), _| k != &key);
        self.cache.retain(|(k, _), _| k != &key);
        self.disk_checked.retain(|(k, _)| k != &key);
        self.videos.retain(|(k, _)| k != &key);
        self.video_failures.remove(&key);
    }
    pub(crate) fn video(&mut self, key: &str) -> Option<Rc<crate::video_ui::Playback>> {
        let index = self.videos.iter().position(|(cached, _)| cached == key)?;
        let entry = self.videos.remove(index)?;
        let player = entry.1.clone();
        self.videos.push_back(entry);
        Some(player)
    }
    pub(crate) fn remember_video(&mut self, key: String, player: Rc<crate::video_ui::Playback>) {
        self.videos.retain(|(cached, _)| cached != &key);
        while self.videos.len() >= 4 {
            self.videos.pop_front();
        }
        self.videos.push_back((key, player));
    }
}
impl Ui {
    pub fn image(
        self: &Rc<Self>,
        file: &Attachment,
        path: &std::path::Path,
        edge: u32,
    ) -> Option<Result<Image, &'static str>> {
        self.image_with_priority(file, path, edge, if edge > 1536 { 2 } else { 1 }, true)
    }
    fn image_with_priority(
        self: &Rc<Self>,
        file: &Attachment,
        path: &std::path::Path,
        edge: u32,
        priority: u8,
        allow_decode: bool,
    ) -> Option<Result<Image, &'static str>> {
        let key = (file.key(), edge);
        {
            let mut images = self.images.borrow_mut();
            images.clock += 1;
            let clock = images.clock;
            if let Some((used, result)) = images.cache.get_mut(&key) {
                *used = clock;
                return Some(result.clone());
            }
            if edge <= 1536 {
                let larger = images
                    .cache
                    .keys()
                    .filter(|(cached, size)| cached == &key.0 && *size >= edge && *size <= 1536)
                    .min_by_key(|(_, size)| *size)
                    .cloned();
                if let Some(larger) = larger {
                    let (used, result) = images.cache.get_mut(&larger).unwrap();
                    *used = clock;
                    return Some(result.clone());
                }
            }
            if !allow_decode && images.disk_checked.contains(&key) {
                return None;
            }
            if let Some(task) = images.tasks.get_mut(&key) {
                task.priority = task.priority.max(priority);
                return None;
            }
            if images.tasks.len() >= 2 {
                let background = images
                    .tasks
                    .iter()
                    .filter(|(_, task)| task.priority < priority)
                    .min_by_key(|(_, task)| task.priority)
                    .map(|(key, _)| key.clone());
                if let Some(background) = background {
                    images.tasks.remove(&background);
                } else {
                    return None;
                }
            }
        }
        if !allow_decode {
            self.images.borrow_mut().disk_checked.insert(key.clone());
        }
        let generation = self.generation.get();
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return None;
        }
        let cancel = gio::Cancellable::new();
        let stopped = Arc::new(AtomicBool::new(false));
        let completed_token = cancel.clone();
        self.images.borrow_mut().tasks.insert(
            key.clone(),
            Task {
                cancel: cancel.clone(),
                stopped: stopped.clone(),
                priority,
            },
        );
        let path = path.to_path_buf();
        let source = file.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(move || {
                crate::image_loader::preview(&source, &path, edge, &cancel, &stopped, allow_decode)
            })
            .await
            .unwrap_or(Err("Image preview unavailable"));
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            let mut images = ui.images.borrow_mut();
            if images
                .tasks
                .get(&key)
                .is_none_or(|task| task.cancel != completed_token)
            {
                return;
            }
            if images.tasks.remove(&key).is_none() {
                return;
            }
            if matches!(result, Err("Preview not cached")) {
                return;
            }
            // Keep a memory budget as well as an entry count for scaled and full-size textures.
            let full = key.1 > 1536;
            let cap = if full { 3 } else { 48 };
            let budget = if full {
                128 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            };
            let bytes = |result: &Result<Image, &'static str>| {
                result.as_ref().map_or(0usize, |image| {
                    image.texture.width() as usize * image.texture.height() as usize * 4
                })
            };
            let requested = bytes(&result);
            loop {
                let entries: Vec<_> = images
                    .cache
                    .iter()
                    .filter(|((_, edge), _)| (*edge > 1536) == full)
                    .collect();
                if entries.len() < cap
                    && entries
                        .iter()
                        .map(|(_, (_, value))| bytes(value))
                        .sum::<usize>()
                        + requested
                        <= budget
                {
                    break;
                }
                let oldest = entries
                    .into_iter()
                    .min_by_key(|(_, (used, _))| used)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    images.cache.remove(&oldest);
                } else {
                    break;
                }
            }
            images.clock += 1;
            let clock = images.clock;
            images.cache.insert(key, (clock, result));
        });
        None
    }
}

impl Ui {
    pub(crate) fn remember_media_metadata(
        &self,
        owner: &str,
        peer: &str,
        id: &str,
        file: &Attachment,
        mime: &str,
        width: u32,
        height: u32,
        animated: bool,
    ) {
        if !self
            .images
            .borrow_mut()
            .remembered
            .insert((peer.into(), id.into(), file.key()))
        {
            return;
        }
        let Some(path) = self
            .store
            .db
            .path()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
        else {
            let _ = self
                .store
                .remember_image(owner, peer, id, file, mime, width, height, animated);
            return;
        };
        let (owner, peer, id, file, mime) = (
            owner.to_owned(),
            peer.to_owned(),
            id.to_owned(),
            file.clone(),
            mime.to_owned(),
        );
        gio::spawn_blocking(move || {
            let Ok(db) = rusqlite::Connection::open(path) else {
                return;
            };
            let _ = db.busy_timeout(std::time::Duration::from_millis(250));
            let store = crate::storage::Store { db };
            let _ = store.remember_image(&owner, &peer, &id, &file, &mime, width, height, animated);
        });
    }
    pub(crate) fn in_chat_viewport(&self, widget: &impl IsA<gtk::Widget>) -> bool {
        widget.is_mapped()
            && widget.compute_bounds(&self.scroll).is_some_and(|bounds| {
                bounds.y() < self.scroll.height() as f32 && bounds.y() + bounds.height() > 0.
            })
    }
    pub(crate) fn in_chat_prefetch(&self, widget: &impl IsA<gtk::Widget>) -> bool {
        widget.is_mapped()
            && widget.compute_bounds(&self.scroll).is_some_and(|bounds| {
                let height = self.scroll.height() as f32;
                bounds.y() < height * 2. && bounds.y() + bounds.height() > -height
            })
    }
    pub fn attach_image(
        self: &Rc<Self>,
        message: &crate::model::Message,
        content: &gtk::Box,
        controls: &gtk::Box,
    ) {
        let file = message.attachment.as_ref().unwrap().clone();
        let sticker = file.sticker;
        let id = message.id.clone();
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let generation = self.generation.get();
        let picture =
            crate::widgets::AspectImage::thumbnail(if sticker { 168 } else { 320 }, sticker);
        let button = gtk::Button::builder()
            .child(&picture)
            .visible(false)
            .halign(gtk::Align::Start)
            .tooltip_text(if sticker {
                "View sticker"
            } else {
                "View image"
            })
            .build();
        button.set_widget_name(&id);
        button.add_css_class("shared-media-tile");
        button.add_css_class("message-media-tile");
        if sticker {
            button.add_css_class("message-sticker-tile");
        }
        content.append(&button);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |button| {
            if let Some(ui) = weak.upgrade() {
                ui.media_viewer(Some(button.clone().upcast()));
            }
        });
        let weak = Rc::downgrade(self);
        let p = picture.downgrade();
        let b = button.downgrade();
        let fallback = controls.downgrade();
        let location = content.downgrade();
        let load = move || {
            let (Some(ui), Some(picture), Some(button)) =
                (weak.upgrade(), p.upgrade(), b.upgrade())
            else {
                return glib::ControlFlow::Break;
            };
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return glib::ControlFlow::Break;
            }
            if picture.paintable().is_some() {
                return glib::ControlFlow::Break;
            }
            let Some(location) = location.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !ui.in_chat_prefetch(&location) {
                return glib::ControlFlow::Continue;
            }
            let ready = ui.cached_image_source(&file);
            let source = ready.clone().or_else(|| {
                crate::downloads::account_root(&owner)
                    .ok()
                    .map(|root| root.join(file.key()))
            });
            if let Some(path) = source {
                let edge = (512 * picture.scale_factor() as u32).min(1024);
                let priority = if ui.in_chat_viewport(&location) { 2 } else { 0 };
                match ui.image_with_priority(&file, &path, edge, priority, ready.is_some()) {
                    Some(Ok(image)) => {
                        ui.remember_media_metadata(
                            &owner,
                            &peer,
                            &id,
                            &file,
                            &image.mime,
                            image.width,
                            image.height,
                            image.animated,
                        );
                        let adjustment = ui.scroll.vadjustment();
                        let old = adjustment.value();
                        let follow = adjustment.upper() - adjustment.page_size() - old < 20.;
                        picture.set_paintable(Some(image.texture.clone().upcast()));
                        button.set_visible(true);
                        if let Some(controls) = fallback.upgrade() {
                            crate::widgets::reveal_media(&controls, &image.texture);
                        }
                        if image.animated {
                            let weak = Rc::downgrade(&ui);
                            crate::image_loader::play(
                                &picture,
                                path,
                                file.clone(),
                                if sticker { 352 } else { 640 },
                                move |picture| {
                                    weak.upgrade()
                                        .is_some_and(|ui| ui.in_chat_viewport(picture))
                                },
                                |picture, frame| picture.set_paintable(Some(frame.upcast())),
                            );
                        }
                        if follow {
                            let button = button.downgrade();
                            glib::timeout_add_local_once(
                                std::time::Duration::from_millis(45),
                                move || {
                                    if button.upgrade().is_some_and(|button| button.is_mapped())
                                        && adjustment.value() >= old - 16.
                                    {
                                        adjustment.set_value(
                                            (adjustment.upper() - adjustment.page_size()).max(0.),
                                        );
                                    }
                                },
                            );
                        }
                        return glib::ControlFlow::Break;
                    }
                    Some(Err(reason)) if file.image_candidate() => {
                        if let Some(controls) = fallback.upgrade() {
                            controls.add_css_class("media-preview-error");
                            controls.set_tooltip_text(Some(reason));
                            controls.set_visible(true);
                        }
                    }
                    Some(Err(_)) => return glib::ControlFlow::Break,
                    None => {}
                }
            }
            glib::ControlFlow::Continue
        };
        if load() == glib::ControlFlow::Continue {
            glib::timeout_add_local(std::time::Duration::from_millis(150), load);
        }
    }
}

impl Ui {
    pub fn cached_image_source(&self, file: &Attachment) -> Option<std::path::PathBuf> {
        let owner = self.state.borrow().account.clone();
        let status = self.downloads.borrow().status(&owner, file);
        match status {
            Some(crate::downloads::Status::Ready(path)) => Some(path),
            None => {
                let _ = self
                    .downloads
                    .borrow_mut()
                    .restore_cached(&owner, file.clone());
                None
            }
            _ => None,
        }
    }
    pub fn live_shared_media(self: &Rc<Self>) {
        use crate::widgets::*;
        let owner = self.state.borrow().account.clone();
        let peer = self.state.borrow().current().jid.clone();
        let generation = self.generation.get();
        let pane = adw::ToolbarView::new();
        pane.add_css_class("utility-sidebar");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .title_widget(&label("Shared Media", "heading"))
            .build();
        let back = icon_button("go-previous-symbolic", "Conversation info");
        self.weak_button(&back, |ui| ui.show_info());
        header.pack_start(&back);
        let close = icon_button("sidebar-show-symbolic", "Close shared media");
        let utility = self.utility.downgrade();
        close.connect_clicked(move |_| {
            if let Some(u) = utility.upgrade() {
                u.set_show_sidebar(false);
            }
        });
        header.pack_end(&close);
        pane.add_top_bar(&header);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        let account = owner.clone();
        let conversation = peer.clone();
        factory.connect_bind(move |_, item| {
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.state.borrow().account == account
                    && ui.state.borrow().current().jid == conversation
            }) else {
                return;
            };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let data = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
            let message = data.borrow::<crate::model::Message>();
            let file = message.attachment.as_ref().unwrap().clone();
            let picture = AspectImage::square();
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&picture));
            let pending = gtk::Image::from_icon_name("image-x-generic-symbolic");
            overlay.add_overlay(&pending);
            let tile = gtk::Button::builder()
                .child(&overlay)
                .tooltip_text(&file.name)
                .hexpand(true)
                .build();
            tile.add_css_class("shared-media-tile");
            tile.set_overflow(gtk::Overflow::Hidden);
            tile.set_widget_name(&message.id);
            tile.update_property(&[gtk::accessible::Property::Label(&file.name)]);
            let w = Rc::downgrade(&ui);
            tile.connect_clicked(move |tile| {
                if let Some(ui) = w.upgrade() {
                    ui.media_viewer(Some(tile.clone().upcast()));
                }
            });
            item.set_child(Some(&tile));
            let w = Rc::downgrade(&ui);
            let p = picture.downgrade();
            let icon = pending.downgrade();
            let account = account.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                let (Some(ui), Some(p), Some(icon)) = (w.upgrade(), p.upgrade(), icon.upgrade())
                else {
                    return glib::ControlFlow::Break;
                };
                if ui.generation.get() != generation || ui.state.borrow().account != account {
                    return glib::ControlFlow::Break;
                }
                if p.paintable().is_some() {
                    return glib::ControlFlow::Break;
                }
                if !p.is_mapped() {
                    return glib::ControlFlow::Continue;
                }
                if let Some(path) = ui.cached_image_source(&file) {
                    if file.video_candidate() {
                        if let Some(player) = ui.video(&file, &path)
                            && player.media.is_prepared()
                            && player.media.has_video()
                            && player.media.intrinsic_width() > 0
                        {
                            p.set_paintable(Some(player.media.current_image()));
                            icon.set_visible(false);
                            return glib::ControlFlow::Break;
                        }
                    } else if let Some(Ok(image)) =
                        ui.image(&file, &path, 384 * p.scale_factor() as u32)
                    {
                        p.set_paintable(Some(image.texture.upcast()));
                        icon.set_visible(false);
                        return glib::ControlFlow::Break;
                    }
                }
                glib::ControlFlow::Continue
            });
        });
        factory.connect_unbind(|_, item| {
            item.downcast_ref::<gtk::ListItem>()
                .unwrap()
                .set_child(gtk::Widget::NONE);
        });
        let grid = gtk::GridView::new(
            Some(gtk::NoSelection::new(Some(model.clone()))),
            Some(factory),
        );
        grid.set_min_columns(3);
        grid.set_max_columns(3);
        grid.set_single_click_activate(false);
        grid.add_css_class("shared-media-grid");
        grid.set_widget_name("shared-media-grid");
        let body = bx(true, 6);
        let scrolling = scroll(&grid);
        scrolling.set_vexpand(true);
        let content = gtk::Stack::new();
        content.set_vexpand(true);
        let pending = gtk::Spinner::builder().spinning(true).build();
        let loading = bx(true, 8);
        loading.set_valign(gtk::Align::Center);
        loading.append(&pending);
        loading.append(&label("Loading shared media…", "dim-label"));
        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title("No Shared Media")
            .build();
        content.add_named(&loading, Some("loading"));
        content.add_named(&scrolling, Some("photos"));
        content.add_named(&empty, Some("empty"));
        content.set_visible_child_name("loading");
        body.append(&content);
        let more = gtk::Button::with_label("Load More");
        more.add_css_class("flat");
        more.set_visible(false);
        body.append(&more);
        let weak = Rc::downgrade(self);
        let more_ref = more.downgrade();
        let content_ref = content.downgrade();
        let list = model.clone();
        let cursor = Rc::new(RefCell::new(None::<(String, String)>));
        let finished = Rc::new(Cell::new(false));
        let load: Rc<dyn Fn()> = Rc::new(move || {
            let Some(more) = more_ref.upgrade() else {
                return;
            };
            if finished.get() || !more.is_sensitive() {
                return;
            }
            more.set_sensitive(false);
            more.set_label("Loading…");
            let before = cursor.borrow().clone();
            let (owner, peer) = (owner.clone(), peer.clone());
            let (weak, more_ref, content_ref) =
                (weak.clone(), more_ref.clone(), content_ref.clone());
            let (list, cursor, finished) = (list.clone(), cursor.clone(), finished.clone());
            glib::MainContext::default().spawn_local(async move {
                let Some(ui) = weak.upgrade() else { return };
                let (account, conversation) = (owner.clone(), peer.clone());
                let page = ui
                    .store
                    .read_snapshot(move |store| {
                        store.media_page(
                            &account,
                            &conversation,
                            before
                                .as_ref()
                                .map(|(time, id)| (time.as_str(), id.as_str())),
                        )
                    })
                    .await;
                let (Some(more), Some(content)) = (more_ref.upgrade(), content_ref.upgrade())
                else {
                    return;
                };
                if ui.generation.get() != generation
                    || ui.state.borrow().account != owner
                    || ui.state.borrow().current().jid != peer
                {
                    return;
                }
                more.set_sensitive(true);
                match page {
                    Ok(page) => {
                        finished.set(page.next.is_none());
                        cursor.replace(page.next);
                        let items: Vec<_> = page
                            .messages
                            .into_iter()
                            .map(glib::BoxedAnyObject::new)
                            .collect();
                        list.extend_from_slice(&items);
                        content.set_visible_child_name(if list.n_items() == 0 {
                            "empty"
                        } else {
                            "photos"
                        });
                        more.set_label("Load More");
                        more.set_visible(!finished.get());
                    }
                    Err(_) => {
                        content.set_visible_child_name("photos");
                        more.set_label("Retry");
                        more.set_visible(true);
                        ui.notice("Shared media could not be loaded");
                    }
                }
            });
        });
        // The explicit button and scroll threshold share one bounded page request.
        let load_more = load.clone();
        more.connect_clicked(move |_| load_more());
        let load_scroll = load.clone();
        scrolling.vadjustment().connect_value_changed(move |a| {
            if a.value() > 0. && a.upper() - a.page_size() - a.value() < 120. {
                load_scroll();
            }
        });
        load();
        pane.set_content(Some(&body));
        self.track_sidebar(&pane);
        self.utility.set_sidebar(gtk::Widget::NONE);
        self.utility.set_sidebar(Some(&pane));
        pane.set_visible(true);
        self.media_mode.replace("shared-media".into());
        self.utility.set_show_sidebar(true);
    }
}
