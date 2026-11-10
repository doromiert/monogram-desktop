//! Small, account-owned texture cache. Decode and verification never run on GTK.
use crate::{file_share::Attachment, ui::Ui};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
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
}
impl Images {
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.cache.clear();
    }
    pub fn invalidate(&mut self, file: &Attachment) {
        let key = file.key();
        self.tasks.retain(|(k, _), _| k != &key);
        self.cache.retain(|(k, _), _| k != &key);
    }
}
impl Ui {
    pub fn image(
        self: &Rc<Self>,
        file: &Attachment,
        path: &std::path::Path,
        edge: u32,
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
            if images.tasks.contains_key(&key) || images.tasks.len() >= 2 {
                return None;
            }
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
            },
        );
        let path = path.to_path_buf();
        let source = file.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gio::spawn_blocking(move || {
                let input = crate::downloads::verified_input(&path, &source, &stopped)?;
                let mut decoder = crate::image_loader::Decoder::open(input, &cancel)?;
                let frame = decoder.frame(edge)?;
                if stopped.load(Ordering::Acquire) {
                    return Err("Image preview cancelled");
                }
                Ok(Image {
                    texture: frame.texture,
                    mime: decoder.mime,
                    width: decoder.width,
                    height: decoder.height,
                    animated: frame.delay.is_some(),
                })
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
            // Separate budgets prevent thumbnails from evicting the active viewer's neighbours.
            let full = key.1 > 1536;
            let cap = if full { 3 } else { 32 };
            while images
                .cache
                .keys()
                .filter(|(_, edge)| (*edge > 1536) == full)
                .count()
                >= cap
            {
                let oldest = images
                    .cache
                    .iter()
                    .filter(|((_, edge), _)| (*edge > 1536) == full)
                    .min_by_key(|(_, v)| v.0)
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    images.cache.remove(&k);
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
    pub(crate) fn in_chat_viewport(&self, widget: &impl IsA<gtk::Widget>) -> bool {
        widget.is_mapped()
            && widget.compute_bounds(&self.scroll).is_some_and(|bounds| {
                bounds.y() < self.scroll.height() as f32 && bounds.y() + bounds.height() > 0.
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
        glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
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
            if let Some(path) = ui.cached_image_source(&file) {
                let edge = (768 * picture.scale_factor() as u32).min(1536);
                match ui.image(&file, &path, edge) {
                    Some(Ok(image)) => {
                        if !ui
                            .store
                            .remember_image(
                                &owner,
                                &peer,
                                &id,
                                &file,
                                &image.mime,
                                image.width,
                                image.height,
                                image.animated,
                            )
                            .unwrap_or(false)
                        {
                            return glib::ControlFlow::Break;
                        }
                        let adjustment = ui.scroll.vadjustment();
                        let old = adjustment.value();
                        let follow = adjustment.upper() - adjustment.page_size() - old < 20.;
                        picture.set_paintable(Some(image.texture.upcast()));
                        button.set_visible(true);
                        if let Some(controls) = fallback.upgrade() {
                            controls.set_visible(false);
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
                    Some(Err(_)) if file.image_candidate() => {
                        if let Some(controls) = fallback.upgrade() {
                            crate::widgets::clear(&controls);
                            controls.append(&crate::widgets::wrap(
                                "Image preview unavailable",
                                "dim-label",
                            ));
                        }
                        return glib::ControlFlow::Break;
                    }
                    Some(Err(_)) => return glib::ControlFlow::Break,
                    None => {}
                }
            }
            glib::ControlFlow::Continue
        });
    }
}

impl Ui {
    pub fn cached_image_source(&self, file: &Attachment) -> Option<std::path::PathBuf> {
        let owner = self.state.borrow().account.clone();
        let status = self.downloads.borrow().status(&owner, file);
        match status {
            Some(crate::downloads::Status::Ready(path)) => Some(path),
            None => {
                if let Ok(root) = crate::downloads::account_root(&owner) {
                    if root.join(file.key()).is_file() {
                        let _ = self
                            .downloads
                            .borrow_mut()
                            .restore_cached(&owner, file.clone());
                    }
                }
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
                if let Some(path) = ui.cached_image_source(&file) {
                    if let Some(Ok(image)) = ui.image(&file, &path, 384 * p.scale_factor() as u32) {
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
