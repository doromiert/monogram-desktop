use crate::ui::favorite_order::{self, Kind as FavoriteKind};
use crate::{
    packs::{Asset, Job, Kind, Library, Pack, Snapshot},
    ui::Ui,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    sync::OnceLock,
    time::Duration,
};

pub(crate) async fn lottie_permit() -> tokio::sync::OwnedSemaphorePermit {
    static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    WORKERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .expect("lottie worker semaphore remains open")
}

fn lottie_visible(picture: &gtk::Picture, scroll: &glib::WeakRef<gtk::ScrolledWindow>) -> bool {
    scroll.upgrade().is_some_and(|scroll| {
        picture.compute_bounds(&scroll).is_some_and(|bounds| {
            bounds.y() + bounds.height() >= 0. && bounds.y() <= scroll.height() as f32
        })
    })
}

fn wait_lottie_visible(
    picture: glib::WeakRef<gtk::Picture>,
    spinner: glib::WeakRef<gtk::Spinner>,
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    source: Arc<crate::downloads::VerifiedFile>,
    serial: Rc<Cell<u64>>,
    expected: u64,
) {
    glib::timeout_add_local_once(Duration::from_millis(100), move || {
        if serial.get() != expected {
            return;
        }
        let (Some(picture), Some(spinner), Some(scroll)) =
            (picture.upgrade(), spinner.upgrade(), scroll.upgrade())
        else {
            return;
        };
        if lottie_visible(&picture, &scroll.downgrade()) {
            start_lottie(&picture, &spinner, &scroll, source, serial);
        } else {
            wait_lottie_visible(
                picture.downgrade(),
                spinner.downgrade(),
                scroll.downgrade(),
                source,
                serial,
                expected,
            );
        }
    });
}

fn schedule_lottie_frame(
    picture: glib::WeakRef<gtk::Picture>,
    spinner: glib::WeakRef<gtk::Spinner>,
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    source: Arc<crate::downloads::VerifiedFile>,
    serial: Rc<Cell<u64>>,
    expected: u64,
    cancel: gio::Cancellable,
    decoder: crate::lottie::Decoder,
    delay: Duration,
) {
    glib::timeout_add_local_once(delay.max(Duration::from_millis(16)), move || {
        let Some(picture_now) = picture.upgrade() else {
            return;
        };
        if serial.get() != expected || cancel.is_cancelled() {
            return;
        }
        if !lottie_visible(&picture_now, &scroll) {
            cancel.cancel();
            let waiting = expected.wrapping_add(1);
            serial.set(waiting);
            drop(decoder);
            wait_lottie_visible(picture, spinner, scroll, source, serial, waiting);
            return;
        }
        let picture_after = picture.clone();
        let scroll_after = scroll.clone();
        let serial_after = serial.clone();
        let cancel_after = cancel.clone();
        glib::spawn_future_local(async move {
            let permit = lottie_permit().await;
            let result = gio::spawn_blocking(move || {
                let mut decoder = decoder;
                let frame = decoder.frame();
                (decoder, frame)
            })
            .await;
            drop(permit);
            let Ok((decoder, Ok(frame))) = result else {
                return;
            };
            let Some(picture) = picture_after.upgrade() else {
                return;
            };
            if serial_after.get() != expected || cancel_after.is_cancelled() {
                return;
            }
            picture.set_paintable(Some(&frame.texture));
            schedule_lottie_frame(
                picture.downgrade(),
                spinner,
                scroll_after,
                source,
                serial_after,
                expected,
                cancel_after,
                decoder,
                frame.delay,
            );
        });
    });
}

fn start_lottie(
    picture: &gtk::Picture,
    spinner: &gtk::Spinner,
    scroll: &gtk::ScrolledWindow,
    source: Arc<crate::downloads::VerifiedFile>,
    serial: Rc<Cell<u64>>,
) {
    let expected = serial.get().wrapping_add(1);
    serial.set(expected);
    let cancel = gio::Cancellable::new();
    let picture_weak = picture.downgrade();
    let spinner_weak = spinner.downgrade();
    let scroll_weak = scroll.downgrade();
    let source_frames = source.clone();
    glib::spawn_future_local(async move {
        let permit = lottie_permit().await;
        let worker_cancel = cancel.clone();
        let result = gio::spawn_blocking(move || {
            let input = source.reopen()?;
            let mut decoder = crate::lottie::Decoder::open(input, 192, &worker_cancel)?;
            let frame = decoder.frame()?;
            Ok::<_, &'static str>((decoder, frame))
        })
        .await;
        drop(permit);
        let (Some(picture), Ok(Ok((decoder, frame)))) = (picture_weak.upgrade(), result) else {
            return;
        };
        if serial.get() != expected || cancel.is_cancelled() {
            return;
        }
        picture.set_paintable(Some(&frame.texture));
        if let Some(spinner) = spinner_weak.upgrade() {
            spinner.stop();
            spinner.set_visible(false);
        }
        schedule_lottie_frame(
            picture.downgrade(),
            spinner_weak,
            scroll_weak,
            source_frames,
            serial,
            expected,
            cancel,
            decoder,
            frame.delay,
        );
    });
}

fn install_lottie(
    picture: &gtk::Picture,
    spinner: &gtk::Spinner,
    scroll: &gtk::ScrolledWindow,
    verified: crate::downloads::VerifiedFile,
) {
    let source = Arc::new(verified);
    let serial = Rc::new(Cell::new(0));
    let picture_weak = picture.downgrade();
    let spinner_weak = spinner.downgrade();
    let scroll_weak = scroll.downgrade();
    let source_map = source.clone();
    let serial_map = serial.clone();
    picture.connect_map(move |_| {
        if let (Some(picture), Some(spinner), Some(scroll)) = (
            picture_weak.upgrade(),
            spinner_weak.upgrade(),
            scroll_weak.upgrade(),
        ) {
            start_lottie(
                &picture,
                &spinner,
                &scroll,
                source_map.clone(),
                serial_map.clone(),
            );
        }
    });
    let serial_unmap = serial.clone();
    picture.connect_unmap(move |_| serial_unmap.set(serial_unmap.get().wrapping_add(1)));
    if picture.is_mapped() {
        start_lottie(picture, spinner, scroll, source, serial);
    }
}

#[path = "gif_grid.rs"]
mod gif_grid;

pub(crate) struct GifLibraryPage {
    pub(crate) root: gtk::Box,
    ui: std::rc::Weak<Ui>,
    library: crate::gifs::Library,
    status: gtk::Label,
    grid: gif_grid::Mosaic,
    scroll: gtk::ScrolledWindow,
    items: RefCell<Vec<crate::gifs::GifItem>>,
    add_key: gtk::Button,
    retry: gtk::Button,
    configured: Cell<bool>,
    saved_view: Cell<bool>,
    failed: Cell<bool>,
    request: RefCell<Option<crate::gifs::Request>>,
    request_serial: Cell<u64>,
    query: RefCell<String>,
    offset: RefCell<Option<u32>>,
    on_send: Box<dyn Fn(std::path::PathBuf)>,
}

impl GifLibraryPage {
    pub(crate) fn new(
        library: crate::gifs::Library,
        parent: &adw::ApplicationWindow,
        ui: std::rc::Weak<Ui>,
        on_send: impl Fn(std::path::PathBuf) + 'static,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("gif-library-page");
        root.set_margin_start(10);
        root.set_margin_end(10);
        root.set_margin_bottom(10);
        root.set_vexpand(true);
        let feedback = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let status = gtk::Label::builder()
            .xalign(0.)
            .wrap(true)
            .hexpand(true)
            .build();
        status.add_css_class("dim-label");
        feedback.append(&status);
        let add_key = gtk::Button::with_label("Add key");
        add_key.set_visible(false);
        feedback.append(&add_key);
        let retry = gtk::Button::with_label("Retry");
        retry.set_visible(false);
        feedback.append(&retry);
        root.append(&feedback);
        let grid = gif_grid::Mosaic::new(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
        grid.add_css_class("gif-library-grid");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        root.append(&scroll);
        let page = Rc::new(Self {
            root,
            ui,
            library,
            status,
            grid,
            scroll: scroll.clone(),
            items: RefCell::new(Vec::new()),
            add_key,
            retry,
            configured: Cell::new(false),
            saved_view: Cell::new(true),
            failed: Cell::new(false),
            request: RefCell::new(None),
            request_serial: Cell::new(0),
            query: RefCell::new(String::new()),
            offset: RefCell::new(None),
            on_send: Box::new(on_send),
        });
        let weak = Rc::downgrade(&page);
        page.grid.set_factory(move |index| {
            let Some(page) = weak.upgrade() else {
                return gtk::Box::new(gtk::Orientation::Vertical, 0).upcast();
            };
            let item = page.items.borrow().get(index).cloned();
            item.map(|item| page.gif_tile(item))
                .unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast())
        });
        let weak = Rc::downgrade(&page);
        scroll.vadjustment().connect_value_changed(move |_| {
            if let Some(page) = weak.upgrade() {
                page.grid.refresh();
                page.maybe_load_more();
            }
        });
        let weak = Rc::downgrade(&page);
        scroll.vadjustment().connect_changed(move |_| {
            if let Some(page) = weak.upgrade() {
                page.grid.refresh();
                page.maybe_load_more();
            }
        });
        let weak = Rc::downgrade(&page);
        scroll.connect_map(move |_| {
            if let Some(page) = weak.upgrade() {
                page.grid.refresh();
                page.maybe_load_more();
            }
        });
        let weak = Rc::downgrade(&page);
        let parent = parent.downgrade();
        page.add_key.connect_clicked(move |_| {
            if let (Some(page), Some(parent)) = (weak.upgrade(), parent.upgrade()) {
                page.settings(&parent);
            }
        });
        let weak = Rc::downgrade(&page);
        page.retry.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.failed.set(false);
                let append = page.offset.borrow().is_some();
                page.load(append);
            }
        });
        page.show_saved();
        page
    }

    pub(crate) fn bind_search(self: &Rc<Self>, search: &gtk::SearchEntry) {
        let weak = Rc::downgrade(self);
        search.connect_search_changed(move |search| {
            let Some(page) = weak.upgrade().filter(|page| page.root.is_mapped()) else {
                return;
            };
            page.query.replace(search.text().trim().to_string());
            page.offset.replace(None);
            page.failed.set(false);
            if page.configured.get() {
                page.load(false);
            } else {
                page.show_saved();
            }
        });
    }

    fn show_saved(self: &Rc<Self>) {
        self.saved_view.set(true);
        if let Some(request) = self.request.take() {
            request.cancel();
        }
        let serial = self.request_serial.get().wrapping_add(1);
        self.request_serial.set(serial);
        self.status.set_label("Loading saved GIFs…");
        let library = self.library.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (saved, configured) =
                futures_util::future::join(library.snapshot(), library.configured()).await;
            let Some(page) = weak
                .upgrade()
                .filter(|page| page.request_serial.get() == serial)
            else {
                return;
            };
            let has_key = matches!(configured, Ok(true));
            page.configured.set(has_key);
            page.add_key.set_visible(!has_key);
            match saved {
                Ok(snapshot) => {
                    let query = page.query.borrow().to_lowercase();
                    let mut items = snapshot.favorites;
                    page.sort_saved_favorites(&mut items);
                    let ids: std::collections::HashSet<_> =
                        items.iter().map(|item| item.id.clone()).collect();
                    items.extend(
                        snapshot
                            .recents
                            .into_iter()
                            .filter(|item| !ids.contains(&item.id)),
                    );
                    items.retain(|item| {
                        query.is_empty() || item.title.to_lowercase().contains(&query)
                    });
                    page.status.set_label(if has_key {
                        "Saved GIFs"
                    } else {
                        "Saved GIFs · Add a GIPHY key to search online"
                    });
                    page.render(items, false);
                    if has_key && page.items.borrow().is_empty() {
                        page.load(false);
                    }
                }
                Err(error) => page.status.set_label(&error.to_string()),
            }
            if let Err(error) = configured {
                page.status.set_label(&error.to_string());
            }
        });
    }

    fn maybe_load_more(self: &Rc<Self>) {
        let adjustment = self.scroll.vadjustment();
        if self.root.is_mapped()
            && self.request.borrow().is_none()
            && !self.failed.get()
            && self.offset.borrow().is_some()
            && adjustment.value() + adjustment.page_size() + 300. >= adjustment.upper()
        {
            self.load(true);
        }
    }

    fn load(self: &Rc<Self>, append: bool) {
        if append && self.request.borrow().is_some() {
            return;
        }
        let offset = if append { *self.offset.borrow() } else { None };
        if append && offset.is_none() {
            return;
        }
        self.saved_view.set(false);
        if let Some(request) = self.request.take() {
            request.cancel();
        }
        self.failed.set(false);
        self.retry.set_visible(false);
        if !append {
            self.offset.replace(None);
            self.render(Vec::new(), false);
            self.scroll.vadjustment().set_value(0.);
        }
        self.status.set_label(if append {
            "Loading more GIFs…"
        } else if self.query.borrow().is_empty() {
            "Loading trending GIFs…"
        } else {
            "Searching GIFs…"
        });
        let request = crate::gifs::Request::new();
        let serial = self.request_serial.get().wrapping_add(1);
        self.request_serial.set(serial);
        self.request.replace(Some(request.clone()));
        let library = self.library.clone();
        let query = self.query.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = library.search(query, offset, request).await;
            let Some(page) = weak
                .upgrade()
                .filter(|page| page.request_serial.get() == serial)
            else {
                return;
            };
            page.request.take();
            match result {
                Ok(result) => {
                    page.offset.replace(
                        result
                            .next_offset
                            .filter(|next| offset.is_none_or(|previous| *next > previous)),
                    );
                    page.status.set_label(
                        if result.items.is_empty() && page.items.borrow().is_empty() {
                            "No GIFs found"
                        } else {
                            "Powered by GIPHY"
                        },
                    );
                    page.render(result.items, append);
                    let weak = Rc::downgrade(&page);
                    glib::idle_add_local_once(move || {
                        if let Some(page) = weak.upgrade() {
                            page.maybe_load_more();
                        }
                    });
                }
                Err(error) => {
                    page.failed.set(true);
                    page.status.set_label(&error.to_string());
                    if matches!(
                        error,
                        crate::gifs::Error::KeyRequired | crate::gifs::Error::KeyRejected
                    ) {
                        page.add_key.set_visible(true);
                    } else {
                        page.retry.set_visible(true);
                    }
                }
            }
        });
    }

    fn render(self: &Rc<Self>, items: Vec<crate::gifs::GifItem>, append: bool) {
        if !append {
            self.items.borrow_mut().clear();
        }
        let mut current = self.items.borrow_mut();
        let mut ids: std::collections::HashSet<_> =
            current.iter().map(|item| item.id.clone()).collect();
        current.extend(items.into_iter().filter(|item| ids.insert(item.id.clone())));
        let ratios = current
            .iter()
            .map(|item| item.preview.width.max(1) as f64 / item.preview.height.max(1) as f64)
            .collect();
        drop(current);
        self.grid.set_items(ratios, append);
    }

    fn gif_tile(self: &Rc<Self>, item: crate::gifs::GifItem) -> gtk::Widget {
        let tile = gtk::Overlay::new();
        tile.add_css_class("gif-library-tile");
        let button = gtk::Button::builder().tooltip_text(&item.title).build();
        button.add_css_class("gif-library-item");
        button.add_css_class("loading");
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(24, 24);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.start();
        button.set_child(Some(&spinner));
        tile.set_child(Some(&button));
        let favorite = gtk::ToggleButton::builder()
            .icon_name(if item.favorite {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            })
            .tooltip_text(if item.favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            })
            .active(item.favorite)
            .halign(gtk::Align::End)
            .build();
        favorite.add_css_class("flat");
        favorite.set_valign(gtk::Align::Start);
        favorite.set_margin_top(4);
        favorite.set_margin_end(4);
        favorite.add_css_class("gif-favorite");
        tile.add_overlay(&favorite);
        if self.saved_view.get() && item.favorite {
            if let Some(ui) = self.ui.upgrade() {
                let owner = self.library.account().to_string();
                let weak = Rc::downgrade(self);
                favorite_order::attach(
                    &ui,
                    &button,
                    FavoriteKind::Gifs,
                    favorite_order::gif_key(&owner, &item.id),
                    move || {
                        weak.upgrade()
                            .map(|page| {
                                page.items
                                    .borrow()
                                    .iter()
                                    .filter(|item| item.favorite)
                                    .map(|item| favorite_order::gif_key(&owner, &item.id))
                                    .collect()
                            })
                            .unwrap_or_default()
                    },
                );
            }
        }
        let weak = Rc::downgrade(self);
        let preview_library = self.library.clone();
        let preview_item = item.clone();
        let button_weak = button.downgrade();
        let request = crate::gifs::Request::new();
        let cancelled = request.clone();
        button.connect_unmap(move |_| cancelled.cancel());
        glib::spawn_future_local(async move {
            let Ok(asset) = preview_library.preview(preview_item.clone(), request).await else {
                Self::preview_failed(&button_weak);
                return;
            };
            let (Some(path), Some(file)) = (asset.path.clone(), asset.attachment()) else {
                Self::preview_failed(&button_weak);
                return;
            };
            let play_path = path.clone();
            let play_file = file.clone();
            let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancel = gio::Cancellable::new();
            let worker_stopped = stopped.clone();
            let decoded = gio::spawn_blocking(move || {
                crate::image_loader::preview(&file, &path, 320, &cancel, &worker_stopped, true)
            })
            .await;
            let Ok(Ok(image)) = decoded else {
                Self::preview_failed(&button_weak);
                return;
            };
            if let (Some(page), Some(button)) = (weak.upgrade(), button_weak.upgrade()) {
                let picture = gtk::Picture::for_paintable(&image.texture);
                picture.set_content_fit(gtk::ContentFit::Cover);
                button.remove_css_class("loading");
                button.set_child(Some(&picture));
                let scroll = page.scroll.downgrade();
                crate::image_loader::play(
                    &picture,
                    play_path,
                    play_file,
                    320,
                    move |picture| {
                        scroll.upgrade().is_some_and(|scroll| {
                            picture.compute_bounds(&scroll).is_some_and(|bounds| {
                                bounds.y() + bounds.height() >= 0.
                                    && bounds.y() <= scroll.height() as f32
                            })
                        })
                    },
                    |picture, texture| picture.set_paintable(Some(&texture)),
                );
            }
        });
        let weak = Rc::downgrade(self);
        let favorite_item = item.clone();
        let restoring = Rc::new(Cell::new(false));
        favorite.connect_toggled(move |button| {
            if restoring.get() {
                return;
            }
            let Some(page) = weak.upgrade() else { return };
            let enabled = button.is_active();
            button.set_sensitive(false);
            let button = button.downgrade();
            let library = page.library.clone();
            let item = favorite_item.clone();
            let restoring = restoring.clone();
            let weak = Rc::downgrade(&page);
            glib::spawn_future_local(async move {
                let result = library
                    .set_favorite(item.clone(), enabled, crate::gifs::Request::new())
                    .await;
                let Some(page) = weak.upgrade() else { return };
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(true);
                    match &result {
                        Ok(_) => {
                            button.set_icon_name(if enabled {
                                "starred-symbolic"
                            } else {
                                "non-starred-symbolic"
                            });
                            button.set_tooltip_text(Some(if enabled {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            }));
                        }
                        Err(_) => {
                            restoring.set(true);
                            button.set_active(!enabled);
                            restoring.set(false);
                        }
                    }
                }
                match result {
                    Ok(_) => {
                        for entry in page
                            .items
                            .borrow_mut()
                            .iter_mut()
                            .filter(|entry| entry.id == item.id)
                        {
                            entry.favorite = enabled;
                        }
                    }
                    Err(error) => page.status.set_label(&error.to_string()),
                }
            });
        });
        let weak = Rc::downgrade(self);
        let library = self.library.clone();
        button.connect_clicked(move |_| {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let Some(ui) = page.ui.upgrade() else { return };
            let peer = ui.state.borrow().current().jid.clone();
            let owner = ui.state.borrow().account.clone();
            let generation = ui.generation.get();
            page.status.set_label("Preparing GIF…");
            let library = library.clone();
            let item = item.clone();
            let weak = Rc::downgrade(&page);
            glib::spawn_future_local(async move {
                let request = crate::gifs::Request::new();
                let asset = match library.original(item.clone(), request.clone()).await {
                    Ok(asset) => asset,
                    Err(error) => {
                        if let Some(page) = weak.upgrade() {
                            page.status.set_label(&error.to_string());
                        }
                        return;
                    }
                };
                let Some(path) = asset.path else {
                    return;
                };
                let _ = library.remember(item, crate::gifs::Request::new()).await;
                if let Some(page) = weak.upgrade() {
                    if page.ui.upgrade().is_some_and(|ui| {
                        ui.generation.get() == generation
                            && ui.state.borrow().account == owner
                            && ui.state.borrow().current().jid == peer
                    }) {
                        (page.on_send)(path);
                        page.status.set_label("Saved GIFs");
                    } else {
                        page.status
                            .set_label("Chat changed. Choose the GIF again to attach it here.");
                    }
                }
            });
        });
        tile.upcast()
    }

    fn preview_failed(button: &glib::WeakRef<gtk::Button>) {
        if let Some(button) = button.upgrade() {
            button.remove_css_class("loading");
            let image = gtk::Image::from_icon_name("image-missing-symbolic");
            image.set_pixel_size(24);
            button.set_child(Some(&image));
            button.set_tooltip_text(Some("Preview unavailable. Click to attach this GIF."));
        }
    }

    fn sort_saved_favorites(&self, items: &mut [crate::gifs::GifItem]) {
        if let Some(ui) = self.ui.upgrade() {
            if let Some(order) = ui.favorite_orders.borrow().get(&FavoriteKind::Gifs) {
                order.sort(items, |item| {
                    favorite_order::gif_key(self.library.account(), &item.id)
                });
            }
        }
    }

    fn refresh_saved_order(self: &Rc<Self>) {
        if !self.saved_view.get() {
            return;
        }
        let (mut favorites, recent): (Vec<_>, Vec<_>) = self
            .items
            .borrow()
            .iter()
            .cloned()
            .partition(|item| item.favorite);
        self.sort_saved_favorites(&mut favorites);
        favorites.extend(recent);
        self.render(favorites, false);
    }
    pub(crate) fn settings(self: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        let entry = adw::PasswordEntryRow::builder()
            .title("GIPHY API key")
            .build();
        let group = adw::PreferencesGroup::builder()
            .description("Searches and previews connect directly to GIPHY. The key is stored in the system keyring for this account; saved GIFs remain available offline.")
            .build();
        group.add(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading("GIF Search")
            .extra_child(&group)
            .build();
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("remove", "Remove Key"),
            ("save", "Save"),
        ]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let library = page.library.clone();
            let value = zeroize::Zeroizing::new(entry.text().trim().to_string());
            let remove = response == "remove";
            if response != "save" && !remove {
                return;
            }
            page.status.set_label(if remove {
                "Removing GIF search key…"
            } else {
                "Saving GIF search key…"
            });
            page.add_key.set_sensitive(false);
            glib::spawn_future_local(async move {
                let result = if remove {
                    library.remove_key().await
                } else {
                    library.save_key(value).await
                };
                page.add_key.set_sensitive(true);
                match result {
                    Ok(()) => {
                        page.configured.set(!remove);
                        page.add_key.set_visible(remove);
                        if remove {
                            page.show_saved();
                        } else {
                            page.load(false);
                        }
                    }
                    Err(error) => page.status.set_label(&error.to_string()),
                }
            });
        });
        dialog.present(Some(parent));
    }
}

impl Drop for GifLibraryPage {
    fn drop(&mut self) {
        if let Some(request) = self.request.borrow_mut().take() {
            request.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackLibraryAction {
    ImportXmsp,
    ImportTelegram,
    Create,
    Manage,
}

#[derive(Clone, Debug)]
pub(crate) enum PackSelection {
    Sticker(Asset),
    CustomEmoji { item_id: i64, fallback: String },
}

#[derive(Clone)]
enum PickerRow {
    Heading(String),
    Empty(String),
    Emoji(Vec<crate::emoji_completion::EmojiEntry>),
    Pack(Vec<crate::packs::Item>),
    Favorites(Vec<FavoriteEntry>),
}

#[derive(Clone)]
enum FavoriteEntry {
    Emoji(crate::emoji_completion::EmojiEntry),
    Pack(crate::packs::Item),
}
impl FavoriteEntry {
    fn key(&self, owner: &str) -> String {
        match self {
            Self::Emoji(entry) => favorite_order::emoji_key(&entry.text),
            Self::Pack(item) => favorite_order::pack_key(owner, item.id),
        }
    }
}

struct PackPreviewCell {
    button: glib::WeakRef<gtk::Button>,
    picture: gtk::Picture,
    spinner: gtk::Spinner,
    item_id: i64,
}

fn pack_selector_button(icon: &str, title: &str) -> gtk::ToggleButton {
    let picture = crate::emoji_picker_ui::mobile_emoji_icon(icon, 24);
    picture.add_css_class("emoji-category-icon");
    let button = gtk::ToggleButton::builder()
        .child(&picture)
        .tooltip_text(title)
        .valign(gtk::Align::Center)
        .build();
    button.set_size_request(30, 30);
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    button
}

/// Native pack-backed sticker/custom-emoji page. The controller owns no
/// fixtures: every visible item comes from the account's pack snapshot.
pub(crate) struct PackLibraryPage {
    pub(crate) root: gtk::Box,
    library: Library,
    kind: Kind,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    selector: gtk::Box,
    group_anchor: Option<gtk::ToggleButton>,
    external_selector: bool,
    model: gio::ListStore,
    list: gtk::ListView,
    scroll: gtk::ScrolledWindow,
    ui: std::rc::Weak<Ui>,
    query: RefCell<String>,
    anchors: RefCell<std::collections::HashMap<i64, u32>>,
    favorites: RefCell<std::collections::HashSet<i64>>,
    favorite_keys: RefCell<Vec<String>>,
    preview_active: Cell<usize>,
    preview_epoch: Cell<u64>,
    preview_queue: RefCell<Vec<PackPreviewCell>>,
    selected: RefCell<Option<i64>>,
    sections: RefCell<Vec<(i64, String, Vec<crate::packs::Item>)>>,
    job: RefCell<Option<Job>>,
    on_select: Box<dyn Fn(PackSelection)>,
    on_view: Box<dyn Fn(i64)>,
    on_manage: Box<dyn Fn(&Rc<PackLibraryPage>, PackLibraryAction, Option<i64>)>,
}

impl PackLibraryPage {
    pub(crate) fn new(
        library: Library,
        kind: Kind,
        ui: std::rc::Weak<Ui>,
        selector_hosts: Option<(gtk::Box, gtk::Box, gtk::ToggleButton)>,
        on_select: impl Fn(PackSelection) + 'static,
        on_view: impl Fn(i64) + 'static,
        on_manage: impl Fn(&Rc<PackLibraryPage>, PackLibraryAction, Option<i64>) + 'static,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("pack-library-page");
        root.set_vexpand(true);
        let external_selector = selector_hosts.is_some();
        if !external_selector {
            root.add_css_class("sticker-sidebar");
        }
        let (selector, settings, group_anchor) = if let Some(hosts) = selector_hosts {
            (hosts.0, hosts.1, Some(hosts.2))
        } else {
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            bar.add_css_class("pack-selector");
            let selector = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            selector.add_css_class("emoji-category-bar");
            let settings = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            let selector_scroll = crate::widgets::horizontal_strip(&selector);
            selector_scroll.set_hexpand(true);
            bar.append(&selector_scroll);
            bar.append(&settings);
            root.append(&bar);
            (selector, settings, None)
        };
        while let Some(child) = selector.first_child() {
            selector.remove(&child);
        }
        while let Some(child) = settings.first_child() {
            settings.remove(&child);
        }
        let import = gtk::Button::builder().label("Import XMSP Pack…").build();
        let telegram = gtk::Button::builder()
            .label("Connect Telegram Pack…")
            .build();
        let create = gtk::Button::builder()
            .label(match kind {
                Kind::Stickers => "Create sticker pack",
                Kind::Emoji => "Create custom emoji pack",
            })
            .build();
        let manage = gtk::Button::builder().label("Manage Packs…").build();
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.add_css_class("message-menu-panel");
        for button in [&import, &telegram, &create, &manage] {
            button.add_css_class("flat");
            button.add_css_class("context-menu-row");
            button.add_css_class("picker-menu-row");
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.);
            }
            menu.append(button);
        }
        let popover = gtk::Popover::builder()
            .has_arrow(false)
            .child(&menu)
            .build();
        popover.add_css_class("message-menu");
        for button in [&import, &telegram, &create, &manage] {
            let popup = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }
            });
        }
        let settings_icon =
            crate::emoji_picker_ui::mobile_emoji_icon("smiles_tab_settings.webp", 24);
        settings_icon.add_css_class("emoji-category-icon");
        let gear = gtk::MenuButton::builder()
            .child(&settings_icon)
            .tooltip_text("Pack settings")
            .popover(&popover)
            .build();
        gear.add_css_class("flat");
        gear.add_css_class("picker-settings");
        gear.set_valign(gtk::Align::Center);
        settings.append(&gear);

        let status = gtk::Label::builder()
            .label("Loading packs…")
            .wrap(true)
            .xalign(0.)
            .build();
        status.add_css_class("dim-label");
        root.append(&status);
        let progress_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        progress_row.add_css_class("pack-library-progress");
        let progress = gtk::ProgressBar::new();
        progress.set_hexpand(true);
        progress_row.append(&progress);
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel operation")
            .build();
        cancel.add_css_class("flat");
        progress_row.append(&cancel);
        progress_row.set_visible(false);
        root.append(&progress_row);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(model.clone()))),
            Some(factory.clone()),
        );
        list.add_css_class("picker-section-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();
        root.append(&scroll);
        let page = Rc::new(Self {
            root,
            library,
            kind,
            status,
            progress,
            cancel: cancel.clone(),
            selector,
            group_anchor,
            external_selector,
            model,
            list,
            scroll: scroll.clone(),
            ui,
            query: RefCell::new(String::new()),
            anchors: RefCell::new(Default::default()),
            favorites: RefCell::new(std::collections::HashSet::new()),
            favorite_keys: RefCell::new(Vec::new()),
            preview_active: Cell::new(0),
            preview_epoch: Cell::new(0),
            preview_queue: RefCell::new(Vec::new()),
            selected: RefCell::new(None),
            sections: RefCell::new(Default::default()),
            job: RefCell::new(None),
            on_select: Box::new(on_select),
            on_view: Box::new(on_view),
            on_manage: Box::new(on_manage),
        });
        {
            let weak = Rc::downgrade(&page);
            factory.connect_bind(move |_, item| {
                let Some(page) = weak.upgrade() else { return };
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(data) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                    return;
                };
                item.set_child(Some(&page.row_widget(&data.borrow::<PickerRow>())));
            });
            factory.connect_unbind(|_, item| {
                item.downcast_ref::<gtk::ListItem>()
                    .unwrap()
                    .set_child(gtk::Widget::NONE);
            });
            let weak = Rc::downgrade(&page);
            scroll.vadjustment().connect_value_changed(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.pump_pack_previews();
                }
            });
            let weak = Rc::downgrade(&page);
            scroll.vadjustment().connect_changed(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.pump_pack_previews();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            import.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    (page.on_manage)(&page, PackLibraryAction::ImportXmsp, None);
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            telegram.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    (page.on_manage)(&page, PackLibraryAction::ImportTelegram, None);
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            create.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    (page.on_manage)(&page, PackLibraryAction::Create, None);
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            cancel.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade()
                    && let Some(job) = page.job.borrow().as_ref()
                {
                    job.cancel();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            manage.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    (page.on_manage)(&page, PackLibraryAction::Manage, *page.selected.borrow());
                }
            });
        }
        page.refresh();
        page
    }

    pub(crate) fn track_job(self: &Rc<Self>, job: Job) {
        self.job.replace(Some(job));
        let Some(progress_row) = self.progress.parent() else {
            return;
        };
        progress_row.set_visible(true);
        self.cancel.set_sensitive(true);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let Some(page) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(job) = page.job.borrow().as_ref().cloned() else {
                return glib::ControlFlow::Break;
            };
            let state = job.progress();
            page.status.set_label(&state.stage);
            page.status.set_visible(true);
            if state.total > 0 {
                page.progress
                    .set_fraction((state.current as f64 / state.total as f64).clamp(0., 1.));
            } else {
                page.progress.pulse();
            }
            if state.active {
                glib::ControlFlow::Continue
            } else {
                page.cancel.set_sensitive(false);
                if let Some(error) = state.error {
                    page.status.set_label(&error);
                } else {
                    page.refresh();
                }
                page.job.take();
                glib::ControlFlow::Break
            }
        });
    }

    pub(crate) fn refresh(self: &Rc<Self>) {
        self.status.set_label("Loading packs…");
        self.status.set_visible(true);
        let library = self.library.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = library.snapshot().await;
            let Some(page) = weak
                .upgrade()
                .filter(|page| page.library.account() == library.account())
            else {
                return;
            };
            match result {
                Ok(snapshot) => page.show_snapshot(snapshot),
                Err(error) => {
                    page.status.set_label(&error.to_string());
                    page.status.set_visible(true);
                }
            }
        });
    }

    fn show_snapshot(self: &Rc<Self>, snapshot: Snapshot) {
        while let Some(child) = self.selector.first_child() {
            self.selector.remove(&child);
        }
        let packs = snapshot
            .packs
            .into_iter()
            .filter(|pack| pack.kind == self.kind && pack.installed)
            .collect::<Vec<_>>();
        let all_items = packs
            .iter()
            .flat_map(|pack| pack.items.iter().cloned())
            .map(|item| (item.id, item))
            .collect::<std::collections::HashMap<_, _>>();
        self.favorites
            .replace(snapshot.favorites.iter().copied().collect());
        let sections = [
            (
                -1,
                "Favorites",
                snapshot
                    .favorites
                    .iter()
                    .filter_map(|id| all_items.get(id).cloned())
                    .collect::<Vec<_>>(),
            ),
            (
                -2,
                "Recent",
                snapshot
                    .recent
                    .iter()
                    .filter_map(|id| all_items.get(id).cloned())
                    .collect::<Vec<_>>(),
            ),
        ]
        .into_iter()
        .collect::<Vec<_>>();
        self.sections.replace(
            sections
                .iter()
                .map(|(id, title, items)| (*id, title.to_string(), items.clone()))
                .chain(
                    packs
                        .iter()
                        .map(|pack| (pack.id, pack.name.clone(), pack.items.clone())),
                )
                .collect(),
        );
        let selected = (*self.selected.borrow())
            .filter(|id| {
                packs.iter().any(|pack| pack.id == *id)
                    || sections.iter().any(|(section, _, _)| section == id)
            })
            .unwrap_or(-1);
        self.selected.replace(Some(selected));
        let mut group = self.group_anchor.clone();
        if !self.external_selector {
            for (id, title, items) in &sections {
                let icon = if *id == -1 {
                    "emoji_tabs_faves.webp"
                } else {
                    "msg_emoji_recent.webp"
                };
                let button = pack_selector_button(icon, title);
                button.add_css_class("flat");
                button.add_css_class("pack-selector-item");
                if let Some(first) = &group {
                    button.set_group(Some(first));
                } else {
                    group = Some(button.clone());
                }
                button.set_active(*id == selected);
                let weak = Rc::downgrade(self);
                let id = *id;
                button.connect_toggled(move |button| {
                    if button.is_active()
                        && let Some(page) = weak.upgrade()
                    {
                        page.selected.replace(Some(id));
                        page.show_section(id);
                        (page.on_view)(id);
                    }
                });
                self.selector.append(&button);
            }
        }
        for pack in &packs {
            let button = gtk::ToggleButton::builder()
                .tooltip_text(&pack.name)
                .build();
            button.add_css_class("flat");
            button.add_css_class("pack-selector-item");
            button.set_valign(gtk::Align::Center);
            button.set_size_request(30, 30);
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            button.set_active(pack.id == selected);
            let weak = Rc::downgrade(self);
            let preview = button.downgrade();
            let library = self.library.clone();
            if let Some(item) = pack.items.first() {
                let item = item.id;
                glib::spawn_future_local(async move {
                    let Ok((asset, verified)) = library.verified_asset(item, Job::new()).await
                    else {
                        return;
                    };
                    let Some(button) = preview.upgrade() else {
                        return;
                    };
                    let mime = asset.item.mime.clone();
                    let texture = gio::spawn_blocking(move || {
                        let cancel = gio::Cancellable::new();
                        if mime == "application/x-tgsticker" {
                            return crate::lottie::Decoder::open(verified.reopen()?, 32, &cancel)?
                                .frame()
                                .map(|frame| frame.texture);
                        }
                        crate::image_loader::Decoder::open_sticker(verified, &cancel)?
                            .frame(32)
                            .map(|frame| frame.texture)
                    })
                    .await;
                    if let Ok(Ok(texture)) = texture {
                        let picture = gtk::Image::from_paintable(Some(&texture));
                        picture.set_pixel_size(24);
                        picture.set_size_request(24, 24);
                        button.set_child(Some(&picture));
                    }
                });
            }
            let pack = pack.clone();
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(page) = weak.upgrade()
                {
                    page.selected.replace(Some(pack.id));
                    page.show_pack(Some(&pack));
                    (page.on_view)(pack.id);
                }
            });
            self.selector.append(&button);
        }
        if selected >= 0 {
            self.selected.replace(Some(selected));
        }
        self.rebuild_rows();
    }

    fn show_pack(self: &Rc<Self>, pack: Option<&Pack>) {
        if let Some(pack) = pack {
            self.show_section(pack.id);
        }
    }

    pub(crate) fn show_section(self: &Rc<Self>, id: i64) {
        self.selected.replace(Some(id));
        if let Some(position) = self.anchors.borrow().get(&id).copied() {
            self.list
                .scroll_to(position, gtk::ListScrollFlags::NONE, None);
        }
    }

    pub(crate) fn search(self: &Rc<Self>, query: &str) {
        self.query.replace(query.to_owned());
        self.rebuild_rows();
    }

    pub(crate) fn rebuild_rows(self: &Rc<Self>) {
        self.preview_epoch
            .set(self.preview_epoch.get().wrapping_add(1));
        self.preview_queue.borrow_mut().clear();
        let mut rows = Vec::new();
        self.anchors.borrow_mut().clear();
        self.favorite_keys.borrow_mut().clear();
        self.status.set_visible(false);
        let query = self.query.borrow().trim().to_lowercase();
        let ui = self.ui.upgrade();
        let preferences = ui
            .as_ref()
            .and_then(|ui| ui.store.emoji_preferences().ok())
            .unwrap_or_default();
        let sections = self.sections.borrow().clone();
        for (id, title, items) in sections {
            if !query.is_empty() && id < 0 {
                continue;
            }
            let items: Vec<_> = items
                .into_iter()
                .filter(|item| {
                    query.is_empty()
                        || item.name.to_lowercase().contains(&query)
                        || item.emoji.contains(&query)
                        || title.to_lowercase().contains(&query)
                })
                .collect();
            let builtin: Vec<_> = if self.kind == Kind::Emoji && query.is_empty() && id < 0 {
                let saved = if id == -1 {
                    &preferences.favorites
                } else {
                    &preferences.recent
                };
                saved
                    .iter()
                    .filter_map(|emoji| {
                        crate::emoji_completion::catalog()
                            .iter()
                            .find(|entry| entry.text == *emoji)
                            .cloned()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if !query.is_empty() && items.is_empty() {
                continue;
            }
            self.push_heading(&mut rows, id, &title);
            if id == -1 {
                let owner = self.library.account();
                let mut entries: Vec<_> = builtin
                    .into_iter()
                    .map(FavoriteEntry::Emoji)
                    .chain(items.into_iter().map(FavoriteEntry::Pack))
                    .collect();
                let category = if self.kind == Kind::Emoji {
                    FavoriteKind::Emoji
                } else {
                    FavoriteKind::Stickers
                };
                if let Some(ui) = ui.as_ref() {
                    if let Some(order) = ui.favorite_orders.borrow().get(&category) {
                        order.sort(&mut entries, |entry| entry.key(owner));
                    }
                }
                self.favorite_keys
                    .replace(entries.iter().map(|entry| entry.key(owner)).collect());
                for row in entries.chunks(if self.kind == Kind::Emoji { 8 } else { 4 }) {
                    Self::push_row(&mut rows, PickerRow::Favorites(row.to_vec()));
                }
                if entries.is_empty() {
                    Self::push_row(&mut rows, PickerRow::Empty("No favorites yet".into()));
                }
                continue;
            }
            for row in builtin.chunks(8) {
                Self::push_row(&mut rows, PickerRow::Emoji(row.to_vec()));
            }
            for row in items.chunks(if self.kind == Kind::Emoji { 8 } else { 4 }) {
                Self::push_row(&mut rows, PickerRow::Pack(row.to_vec()));
            }
            if rows.len() == self.anchors.borrow()[&id] as usize + 1 {
                Self::push_row(
                    &mut rows,
                    PickerRow::Empty(format!("No {} yet", title.to_lowercase())),
                );
            }
        }
        if self.kind == Kind::Emoji {
            let entries = if query.is_empty() {
                crate::emoji_completion::catalog().to_vec()
            } else {
                crate::emoji_completion::search(&query)
            };
            for (id, title, groups) in crate::emoji_picker_ui::builtin_categories() {
                let entries: Vec<_> = entries
                    .iter()
                    .filter(|entry| groups.contains(&entry.group))
                    .cloned()
                    .collect();
                if entries.is_empty() {
                    continue;
                }
                self.push_heading(&mut rows, id, title);
                for row in entries.chunks(8) {
                    Self::push_row(&mut rows, PickerRow::Emoji(row.to_vec()));
                }
            }
        }
        if rows.is_empty() {
            Self::push_row(&mut rows, PickerRow::Empty("No matches".into()));
        }
        self.model.splice(0, self.model.n_items(), &rows);
    }

    fn push_heading(&self, rows: &mut Vec<glib::BoxedAnyObject>, id: i64, title: &str) {
        self.anchors.borrow_mut().insert(id, rows.len() as u32);
        Self::push_row(rows, PickerRow::Heading(title.into()));
    }
    fn push_row(rows: &mut Vec<glib::BoxedAnyObject>, row: PickerRow) {
        rows.push(glib::BoxedAnyObject::new(row));
    }

    fn row_widget(self: &Rc<Self>, row: &PickerRow) -> gtk::Widget {
        match row {
            PickerRow::Favorites(entries) => {
                let row = gtk::Box::new(
                    gtk::Orientation::Horizontal,
                    if self.kind == Kind::Emoji { 2 } else { 6 },
                );
                row.set_homogeneous(true);
                let columns = if self.kind == Kind::Emoji { 8 } else { 4 };
                if let Some(ui) = self.ui.upgrade() {
                    for entry in entries {
                        let cell: gtk::Widget = match entry {
                            FavoriteEntry::Emoji(entry) => {
                                let Some(button) =
                                    crate::emoji_picker_ui::builtin_button(&ui, entry)
                                else {
                                    continue;
                                };
                                button.upcast()
                            }
                            FavoriteEntry::Pack(item) => self.pack_cell(item).upcast(),
                        };
                        let weak = Rc::downgrade(self);
                        favorite_order::attach(
                            &ui,
                            &cell,
                            if self.kind == Kind::Emoji {
                                FavoriteKind::Emoji
                            } else {
                                FavoriteKind::Stickers
                            },
                            entry.key(self.library.account()),
                            move || {
                                weak.upgrade()
                                    .map(|page| page.favorite_keys.borrow().clone())
                                    .unwrap_or_default()
                            },
                        );
                        row.append(&crate::widgets::SquareCell::new(&cell));
                    }
                }
                for _ in entries.len()..columns {
                    row.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
                }
                row.upcast()
            }
            PickerRow::Heading(title) | PickerRow::Empty(title) => {
                let label = crate::widgets::text(
                    title,
                    if matches!(row, PickerRow::Heading(_)) {
                        "picker-section-heading"
                    } else {
                        "dim-label"
                    },
                );
                label.set_margin_top(8);
                label.set_margin_bottom(6);
                label.upcast()
            }
            PickerRow::Emoji(entries) => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
                row.set_homogeneous(true);
                if let Some(ui) = self.ui.upgrade() {
                    for entry in entries {
                        if let Some(button) = crate::emoji_picker_ui::builtin_button(&ui, entry) {
                            row.append(&crate::widgets::SquareCell::new(&button));
                        }
                    }
                }
                for _ in entries.len()..8 {
                    row.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
                }
                row.upcast()
            }
            PickerRow::Pack(items) => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                row.set_homogeneous(true);
                let columns = if self.kind == Kind::Emoji { 8 } else { 4 };
                for item in items {
                    row.append(&self.pack_cell(item));
                }
                for _ in items.len()..columns {
                    row.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
                }
                row.upcast()
            }
        }
    }

    fn pack_cell(self: &Rc<Self>, item: &crate::packs::Item) -> gtk::Button {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("pack-item");
        if self.kind == Kind::Emoji {
            button.add_css_class("custom-emoji-item");
        }
        button.set_size_request(
            if self.kind == Kind::Emoji { 42 } else { 64 },
            if self.kind == Kind::Emoji { 42 } else { 64 },
        );
        button.set_halign(gtk::Align::Fill);
        button.set_valign(gtk::Align::Start);
        button.set_tooltip_text(Some(&item.name));
        button.update_property(&[gtk::accessible::Property::Label(&item.name)]);
        let spinner = gtk::Spinner::new();
        spinner.start();
        let picture = gtk::Picture::new();
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.add_css_class("pack-item-picture");
        let overlay = gtk::Overlay::builder().child(&picture).build();
        overlay.add_overlay(&spinner);
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_halign(gtk::Align::End);
        star.set_valign(gtk::Align::Start);
        star.set_margin_top(3);
        star.set_margin_end(3);
        star.set_visible(self.favorites.borrow().contains(&item.id));
        star.add_css_class("pack-favorite");
        overlay.add_overlay(&star);
        let frame = gtk::AspectFrame::builder()
            .ratio(1.)
            .obey_child(false)
            .child(&overlay)
            .build();
        button.set_child(Some(&frame));
        let favorite = gtk::GestureClick::new();
        favorite.set_button(3);
        let weak_favorite = Rc::downgrade(self);
        let favorite_library = self.library.clone();
        let favorite_id = item.id;
        favorite.connect_released(move |_, _, _, _| {
            let Some(page) = weak_favorite.upgrade() else {
                return;
            };
            let enabled = !page.favorites.borrow().contains(&favorite_id);
            let library = favorite_library.clone();
            let weak = Rc::downgrade(&page);
            glib::spawn_future_local(async move {
                if library
                    .mutate(crate::packs::Mutation::Favorite {
                        item: favorite_id,
                        value: enabled,
                    })
                    .await
                    .is_ok()
                    && let Some(page) = weak.upgrade()
                {
                    page.refresh();
                }
            });
        });
        button.add_controller(favorite);
        self.preview_queue.borrow_mut().push(PackPreviewCell {
            button: button.downgrade(),
            picture,
            spinner,
            item_id: item.id,
        });
        let weak = Rc::downgrade(self);
        button.connect_map(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(page) = weak.upgrade() {
                    page.pump_pack_previews();
                }
            });
        });
        button
    }

    fn pump_pack_previews(self: &Rc<Self>) {
        self.preview_queue
            .borrow_mut()
            .retain(|cell| cell.button.upgrade().is_some());
        while self.preview_active.get() < 4 {
            let next = self.preview_queue.borrow().iter().position(|cell| {
                cell.button.upgrade().is_some_and(|button| {
                    button.compute_bounds(&self.scroll).is_some_and(|bounds| {
                        bounds.y() + bounds.height() >= -96.
                            && bounds.y() <= self.scroll.height() as f32 + 96.
                    })
                })
            });
            let Some(index) = next else {
                return;
            };
            let cell = self.preview_queue.borrow_mut().remove(index);
            let epoch = self.preview_epoch.get();
            self.preview_active.set(self.preview_active.get() + 1);
            let weak = Rc::downgrade(self);
            let library = self.library.clone();
            glib::spawn_future_local(async move {
                let result = library.verified_asset(cell.item_id, Job::new()).await;
                let Some(page) = weak.upgrade() else {
                    return;
                };
                page.preview_active
                    .set(page.preview_active.get().saturating_sub(1));
                if page.preview_epoch.get() == epoch
                    && let Some(button) = cell.button.upgrade()
                {
                    page.finish_pack_preview(button, cell.picture, cell.spinner, result);
                }
                page.pump_pack_previews();
            });
        }
    }

    fn finish_pack_preview(
        self: &Rc<Self>,
        button: gtk::Button,
        picture: gtk::Picture,
        spinner: gtk::Spinner,
        result: crate::packs::Result<(Asset, crate::downloads::VerifiedFile)>,
    ) {
        let (asset, verified) = match result {
            Ok(asset) => asset,
            Err(error) => {
                spinner.stop();
                spinner.set_visible(false);
                button.set_icon_name("image-missing-symbolic");
                button.set_tooltip_text(Some(&error.to_string()));
                return;
            }
        };
        let weak = Rc::downgrade(self);
        let recent_library = self.library.clone();
        let selection_asset = asset.clone();
        button.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                let library = recent_library.clone();
                let item = selection_asset.item.id;
                glib::spawn_future_local(async move {
                    let _ = library
                        .mutate(crate::packs::Mutation::Recent { item })
                        .await;
                });
                let selection = match page.kind {
                    Kind::Stickers => PackSelection::Sticker(selection_asset.clone()),
                    Kind::Emoji => PackSelection::CustomEmoji {
                        item_id: selection_asset.item.id,
                        fallback: selection_asset.item.emoji.clone(),
                    },
                };
                (page.on_select)(selection);
            }
        });
        if asset.item.mime == "video/webm" {
            if let Some(player) = crate::video_ui::Playback::new(verified) {
                picture.set_paintable(Some(&player.media));
                spinner.stop();
                spinner.set_visible(false);
                let playback = player.clone();
                picture.connect_map(move |_| playback.media.play());
                picture.connect_unmap(move |_| player.media.pause());
            } else {
                button.set_icon_name("image-missing-symbolic");
                button.set_tooltip_text(Some("Animated sticker preview unavailable"));
            }
        } else if asset.item.mime == "application/x-tgsticker" {
            install_lottie(&picture, &spinner, &self.scroll, verified);
        } else if asset.item.mime.starts_with("image/") {
            let spinner = spinner.downgrade();
            let scroll = self.scroll.downgrade();
            crate::image_loader::play_opened(
                &picture,
                verified,
                192,
                move |picture| {
                    scroll.upgrade().is_some_and(|scroll| {
                        picture.compute_bounds(&scroll).is_some_and(|bounds| {
                            bounds.y() + bounds.height() >= 0.
                                && bounds.y() <= scroll.height() as f32
                        })
                    })
                },
                move |picture, texture| {
                    picture.set_paintable(Some(&texture));
                    if let Some(spinner) = spinner.upgrade() {
                        spinner.stop();
                        spinner.set_visible(false);
                    }
                },
            );
        } else {
            spinner.stop();
            spinner.set_visible(false);
            button.set_icon_name("media-playback-start-symbolic");
        }
    }
}

impl Drop for PackLibraryPage {
    fn drop(&mut self) {
        self.library.cancel();
    }
}

impl Ui {
    pub(crate) fn refresh_favorite_views(self: &Rc<Self>) {
        for page in self.pack_pages.borrow().clone() {
            if page.kind == Kind::Stickers {
                page.rebuild_rows();
            }
        }
        crate::emoji_picker_ui::refresh_emoji_surfaces(self);
        let page = self.gif_page.borrow().clone();
        if let Some(page) = page {
            page.refresh_saved_order();
        }
    }

    fn pack_library_action(
        self: &Rc<Self>,
        page: &Rc<PackLibraryPage>,
        library: Library,
        kind: Kind,
        action: PackLibraryAction,
        selected: Option<i64>,
    ) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        match action {
            PackLibraryAction::ImportXmsp => {
                let chooser = gtk::FileDialog::builder()
                    .title("Import XMSP Pack")
                    .modal(true)
                    .build();
                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.xmsp");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                chooser.set_filters(Some(&filters));
                let window = self.window.clone();
                let weak = Rc::downgrade(self);
                let page = Rc::downgrade(page);
                glib::spawn_future_local(async move {
                    let Ok(file) = chooser.open_future(Some(&window)).await else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == owner
                    }) else {
                        return;
                    };
                    let Some(page) = page.upgrade() else {
                        return;
                    };
                    let job = Job::new();
                    page.track_job(job.clone());
                    match library.import_xmsp(path, job).await {
                        Ok(_) => page.refresh(),
                        Err(error) => ui.notice(&error.to_string()),
                    }
                });
            }
            PackLibraryAction::ImportTelegram => {
                let link = adw::EntryRow::builder().title("Telegram pack link").build();
                let token = adw::PasswordEntryRow::builder()
                    .title("Bot token (only if replacing)")
                    .build();
                let group = adw::PreferencesGroup::builder().description("Imports contact Telegram's official Bot API directly. The token is stored in the system keyring and is never sent to your XMPP server.").build();
                group.add(&link);
                group.add(&token);
                let dialog = adw::AlertDialog::builder()
                    .heading("Import Telegram Pack")
                    .extra_child(&group)
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("import", "Import")]);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let weak = Rc::downgrade(self);
                let page = Rc::downgrade(page);
                dialog.connect_response(Some("import"), move |_, _| {
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == owner
                    }) else {
                        return;
                    };
                    let Some(page) = page.upgrade() else {
                        return;
                    };
                    let library = library.clone();
                    let address = link.text().to_string();
                    let secret = zeroize::Zeroizing::new(token.text().to_string());
                    glib::spawn_future_local(async move {
                        if !secret.is_empty()
                            && let Err(error) = library.set_telegram_token(secret).await
                        {
                            ui.notice(&error.to_string());
                            return;
                        }
                        let job = Job::new();
                        page.track_job(job.clone());
                        match library.import_telegram(address, None, job).await {
                            Ok(_) => page.refresh(),
                            Err(error) => ui.notice(&error.to_string()),
                        }
                    });
                });
                dialog.present(Some(&self.window));
            }
            PackLibraryAction::Create => {
                let name = adw::EntryRow::builder().title("Pack name").build();
                let dialog = adw::AlertDialog::builder()
                    .heading(match kind {
                        Kind::Stickers => "Create Sticker Pack",
                        Kind::Emoji => "Create Custom Emoji Pack",
                    })
                    .extra_child(&name)
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("choose", "Choose Artwork")]);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let weak = Rc::downgrade(self);
                let page = Rc::downgrade(page);
                dialog.connect_response(Some("choose"), move |_, _| {
                    let pack_name = name.text().trim().to_string();
                    if pack_name.is_empty() {
                        return;
                    }
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == owner
                    }) else {
                        return;
                    };
                    let chooser = gtk::FileDialog::builder()
                        .title("Choose Pack Artwork")
                        .modal(true)
                        .build();
                    let window = ui.window.clone();
                    let library = library.clone();
                    let page = page.clone();
                    glib::spawn_future_local(async move {
                        let Ok(files) = chooser.open_multiple_future(Some(&window)).await else {
                            return;
                        };
                        let inputs = files
                            .iter::<gio::File>()
                            .flatten()
                            .filter_map(|file| {
                                let path = file.path()?;
                                let name = path.file_stem()?.to_string_lossy().to_string();
                                Some(crate::packs::Input {
                                    path,
                                    name,
                                    emoji: "🙂".into(),
                                })
                            })
                            .collect::<Vec<_>>();
                        let Some(page) = page.upgrade() else {
                            return;
                        };
                        let job = Job::new();
                        page.track_job(job.clone());
                        match library.create(pack_name, kind, inputs, job).await {
                            Ok(_) => page.refresh(),
                            Err(error) => page.status.set_label(&error.to_string()),
                        }
                    });
                });
                dialog.present(Some(&self.window));
            }
            PackLibraryAction::Manage => {
                let Some(pack) = selected.filter(|id| *id >= 0) else {
                    self.notice("Select a pack first");
                    return;
                };
                let dialog = adw::AlertDialog::builder()
                    .heading("Manage Pack")
                    .body("Choose what to do with this pack.")
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("uninstall", "Uninstall")]);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
                let weak = Rc::downgrade(self);
                let page = Rc::downgrade(page);
                dialog.connect_response(Some("uninstall"), move |_, _| {
                    let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == owner
                    }) else {
                        return;
                    };
                    let Some(page) = page.upgrade() else {
                        return;
                    };
                    let library = library.clone();
                    glib::spawn_future_local(async move {
                        match library
                            .mutate(crate::packs::Mutation::Installed { pack, value: false })
                            .await
                        {
                            Ok(_) => page.refresh(),
                            Err(error) => ui.notice(&error.to_string()),
                        }
                    });
                });
                dialog.present(Some(&self.window));
            }
        }
    }

    pub(crate) fn rebuild_gif_library(self: &Rc<Self>) {
        if let Some(library) = self.gif_library.borrow_mut().take() {
            library.cancel();
        }
        self.gif_page.borrow_mut().take();
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        self.replace_media_page(
            "gifs",
            &adw::StatusPage::builder()
                .title(if owner.is_empty() {
                    "Sign in to use GIFs"
                } else {
                    "Loading saved GIFs…"
                })
                .description("Saved GIFs remain available offline.")
                .build(),
        );
        if owner.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = crate::gifs::Library::open(&owner).await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                if let Ok(library) = result {
                    library.cancel();
                }
                return;
            };
            let library = match result {
                Ok(library) => library,
                Err(error) => {
                    ui.replace_media_page(
                        "gifs",
                        &adw::StatusPage::builder()
                            .title("GIF library unavailable")
                            .description(&error.to_string())
                            .build(),
                    );
                    return;
                }
            };
            let weak = Rc::downgrade(&ui);
            let send_owner = owner.clone();
            let page = GifLibraryPage::new(
                library.clone(),
                &ui.window,
                Rc::downgrade(&ui),
                move |path| {
                    if let Some(ui) = weak.upgrade().filter(|ui| {
                        ui.generation.get() == generation && ui.state.borrow().account == send_owner
                    }) {
                        ui.send_library_file(&path, false);
                    }
                },
            );
            page.bind_search(&ui.media_search);
            ui.replace_media_page("gifs", &page.root);
            ui.gif_library.replace(Some(library));
            ui.gif_page.replace(Some(page));
            if let Some(toggle) = ui.media_toggles.borrow().get("gifs") {
                toggle.set_enabled(true);
            }
        });
    }

    pub(crate) fn rebuild_pack_library(self: &Rc<Self>) {
        if let Some(library) = self.pack_library.borrow_mut().take() {
            library.cancel();
        }
        self.pack_pages.borrow_mut().clear();
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        self.replace_media_page(
            "stickers",
            &adw::StatusPage::builder()
                .title(if owner.is_empty() {
                    "Sign in to use packs"
                } else {
                    "Loading packs…"
                })
                .description("Stickers and custom emoji stay private to this account.")
                .build(),
        );
        if owner.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = Library::open(&owner).await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                if let Ok(library) = result {
                    library.cancel();
                }
                return;
            };
            let library = match result {
                Ok(library) => library,
                Err(error) => {
                    ui.replace_media_page(
                        "stickers",
                        &adw::StatusPage::builder()
                            .title("Packs unavailable")
                            .description(&error.to_string())
                            .build(),
                    );
                    return;
                }
            };
            let make_page = |kind, selector_hosts, on_view: Box<dyn Fn(i64)>| {
                let select_ui = Rc::downgrade(&ui);
                let select_library = library.clone();
                let select_owner = owner.clone();
                let manage_ui = Rc::downgrade(&ui);
                let manage_library = library.clone();
                PackLibraryPage::new(
                    library.clone(),
                    kind,
                    Rc::downgrade(&ui),
                    selector_hosts,
                    move |selection| {
                        let Some(ui) = select_ui.upgrade().filter(|ui| {
                            ui.generation.get() == generation
                                && ui.state.borrow().account == select_owner
                        }) else {
                            return;
                        };
                        match selection {
                            PackSelection::Sticker(asset) => {
                                ui.send_library_file(&asset.path, true);
                            }
                            PackSelection::CustomEmoji { item_id, fallback } => {
                                let weak = Rc::downgrade(&ui);
                                let library = select_library.clone();
                                let owner = select_owner.clone();
                                let peer = ui.state.borrow().current().jid.clone();
                                glib::spawn_future_local(async move {
                                    let Ok(asset) = library.inline_asset(item_id).await else {
                                        return;
                                    };
                                    if let Some(ui) = weak.upgrade().filter(|ui| {
                                        ui.generation.get() == generation
                                            && ui.state.borrow().account == owner
                                            && ui.state.borrow().current().jid == peer
                                    }) {
                                        ui.insert_custom_emoji(asset, &fallback);
                                    }
                                });
                            }
                        }
                    },
                    on_view,
                    move |page, action, selected| {
                        if let Some(ui) = manage_ui.upgrade() {
                            ui.pack_library_action(
                                page,
                                manage_library.clone(),
                                kind,
                                action,
                                selected,
                            );
                        }
                    },
                )
            };
            let stickers = make_page(Kind::Stickers, None, Box::new(|_| {}));
            let emoji_ui = Rc::downgrade(&ui);
            let emoji = make_page(
                Kind::Emoji,
                crate::emoji_picker_ui::emoji_pack_hosts(&ui),
                Box::new(move |id| {
                    if id >= 0
                        && let Some(ui) = emoji_ui.upgrade()
                    {
                        crate::emoji_picker_ui::show_custom_emoji_pack(&ui);
                    }
                }),
            );
            stickers.root.set_margin_start(10);
            stickers.root.set_margin_end(10);
            stickers.root.set_margin_bottom(10);
            let sticker_search = Rc::downgrade(&stickers);
            ui.media_search.connect_search_changed(move |search| {
                if let Some(page) = sticker_search
                    .upgrade()
                    .filter(|page| page.root.is_mapped())
                {
                    page.search(search.text().as_str());
                }
            });
            ui.replace_media_page("stickers", &stickers.root);
            crate::emoji_picker_ui::attach_custom_emoji_page(&ui, &emoji);
            ui.pack_library.replace(Some(library));
            ui.pack_pages.replace(vec![stickers, emoji]);
        });
    }

    fn replace_media_page(&self, name: &str, page: &impl IsA<gtk::Widget>) {
        if let Some(previous) = self.media_stack.child_by_name(name) {
            self.media_stack.remove(&previous);
        }
        self.media_stack.add_named(page, Some(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn until(timeout: Duration, predicate: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    #[ignore = "requires a native display, bubblewrap and rlottie"]
    fn offscreen_tgs_drops_and_lazily_reopens_its_decoder() {
        gtk::init().unwrap();
        let path = std::path::PathBuf::from(
            std::env::var("MONOGRAM_TGS_FIXTURE").expect("real TGS fixture path"),
        );
        let bytes = std::fs::read(&path).unwrap();
        let mut attachment =
            crate::file_share::Attachment::bare("https://fixture.invalid/sticker.tgs").unwrap();
        attachment.size = Some(bytes.len() as u64);
        attachment
            .hashes
            .insert("sha-256".into(), openssl::sha::sha256(&bytes).to_vec());
        let verified = crate::downloads::verified_input(
            &path,
            &attachment,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let picture = gtk::Picture::new();
        picture.set_size_request(120, 120);
        content.append(&picture);
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_size_request(120, 800);
        content.append(&spacer);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .min_content_height(140)
            .max_content_height(140)
            .build();
        let window = gtk::Window::builder()
            .child(&scroll)
            .default_width(180)
            .default_height(140)
            .build();
        let changes = Arc::new(AtomicUsize::new(0));
        let changed = changes.clone();
        picture.connect_notify_local(Some("paintable"), move |_, _| {
            changed.fetch_add(1, Ordering::Relaxed);
        });
        let spinner = gtk::Spinner::new();
        install_lottie(&picture, &spinner, &scroll, verified);
        window.present();
        assert!(until(Duration::from_secs(8), || changes
            .load(Ordering::Relaxed)
            >= 3));
        let adjustment = scroll.vadjustment();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        let _ = until(Duration::from_millis(500), || false);
        let stopped = changes.load(Ordering::Relaxed);
        let _ = until(Duration::from_millis(250), || false);
        assert!(changes.load(Ordering::Relaxed) <= stopped + 1);
        adjustment.set_value(0.);
        assert!(until(Duration::from_secs(8), || {
            changes.load(Ordering::Relaxed) >= stopped + 3
        }));
        window.close();
    }
}

#[cfg(test)]
mod gif_native_tests {
    use super::*;
    use std::time::Instant;

    fn wait(mut ready: impl FnMut() -> bool) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            for _ in 0..50 {
                if !context.pending() {
                    break;
                }
                context.iteration(false);
            }
            if ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert!(ready(), "native GIF page did not settle");
    }
    fn children(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut result = Vec::new();
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            result.push(current);
        }
        result
    }
    #[test]
    #[ignore = "requires an isolated native display, data roots and Secret Service"]
    fn gif_page_virtualizes_and_handles_keyless_paging() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.GifFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .default_width(400)
            .default_height(700)
            .build();
        let context = glib::MainContext::default();
        let library = context
            .block_on(crate::gifs::Library::open("gif-native@fixture.invalid"))
            .unwrap();
        let page = GifLibraryPage::new(library.clone(), &window, std::rc::Weak::new(), |_| {
            panic!("fixture must not send")
        });
        window.set_content(Some(&page.root));
        window.present();
        wait(|| page.add_key.is_visible());
        assert!(!page.configured.get());
        page.add_key.emit_clicked();
        wait(|| window.visible_dialog().is_some());
        assert_eq!(
            window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::AlertDialog>()
                .unwrap()
                .heading()
                .as_deref(),
            Some("GIF Search")
        );
        window.visible_dialog().unwrap().close();
        assert!(!children(&page.root).iter().any(|child| {
            child.downcast_ref::<gtk::Button>().is_some_and(|button| {
                button.icon_name().as_deref() == Some("emblem-system-symbolic")
            })
        }));
        let path = std::path::PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap())
            .join("gif-fixture.gif");
        std::fs::write(&path,b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0\x3b").unwrap();
        let source = context
            .block_on(library.import_local(path, "Fixture GIF".into(), crate::gifs::Request::new()))
            .unwrap();
        let items = (0..2000)
            .map(|index| {
                let mut item = source.clone();
                if index > 0 {
                    item.id = format!("fixture-{index}");
                }
                item.title = format!("Tile {index}");
                item.preview.width = [40, 100, 160, 240, 400][index % 5];
                item.preview.height = 100;
                item
            })
            .collect();
        let start = Instant::now();
        page.render(items, false);
        wait(|| children(&page.grid).len() >= 4);
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "first GIF controls took {:?}",
            start.elapsed()
        );
        let count = children(&page.grid).len();
        assert!(count < 100, "created {count} controls for 2000 items");
        wait(|| children(&page.grid).len() >= 8);
        for child in children(&page.grid) {
            assert!(child.width() >= 64 && child.height() >= 64);
            let overlay = child.downcast::<gtk::Overlay>().unwrap();
            assert!(
                children(&overlay)
                    .iter()
                    .any(|child| child.has_css_class("gif-favorite"))
            );
        }
        let first = children(&page.grid)
            .into_iter()
            .find_map(|child| {
                let overlay = child.downcast::<gtk::Overlay>().ok()?;
                let button = overlay.child()?.downcast::<gtk::Button>().ok()?;
                (button.tooltip_text().as_deref() == Some("Tile 0")).then(|| {
                    children(&overlay)
                        .into_iter()
                        .find_map(|child| child.downcast::<gtk::ToggleButton>().ok())
                        .unwrap()
                })
            })
            .unwrap();
        first.set_active(true);
        wait(|| first.is_sensitive());
        assert!(
            context
                .block_on(library.snapshot())
                .unwrap()
                .favorites
                .iter()
                .any(|item| item.id == source.id)
        );
        if let Some(path) = std::env::var_os("MONOGRAM_GIF_CAPTURE") {
            let snapshot = gtk::Snapshot::new();
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            let renderer = window.renderer().unwrap();
            renderer
                .render_texture(&snapshot.to_node().unwrap(), None)
                .save_to_png(path)
                .unwrap();
        }
        let start = Instant::now();
        page.scroll.vadjustment().set_value(12000.);
        wait(|| {
            children(&page.grid)
                .iter()
                .filter_map(|child| child.compute_bounds(&page.grid))
                .all(|bounds| bounds.y() > 10000.)
        });
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "scroll feedback took {:?}",
            start.elapsed()
        );
        assert!(children(&page.grid).len() < 100);
        let previous = page.items.borrow().len();
        page.render(vec![source.clone()], true);
        assert_eq!(page.items.borrow().len(), previous, "duplicate append");
        // A pending provider page at the edge must start automatically, then stop
        // after a keyless error rather than repeatedly retrying while at the edge.
        page.offset.replace(Some(25));
        page.scroll
            .vadjustment()
            .set_value(page.scroll.vadjustment().upper());
        page.maybe_load_more();
        assert!(page.request.borrow().is_some());
        wait(|| page.failed.get());
        assert!(page.add_key.is_visible());
        let serial = page.request_serial.get();
        page.maybe_load_more();
        assert_eq!(page.request_serial.get(), serial);
        page.offset.replace(None);
        page.failed.set(false);
        page.maybe_load_more();
        assert_eq!(
            page.request_serial.get(),
            serial,
            "exhausted grid requested again"
        );
        context
            .block_on(library.save_key(zeroize::Zeroizing::new("fixture-key-only".into())))
            .unwrap();
        for (offset, next) in [(0, Some(25)), (25, Some(50)), (50, None)] {
            let items = (0..2)
                .map(|index| {
                    let mut item = source.clone();
                    item.id = format!("page-{offset}-{index}");
                    item
                })
                .collect();
            context.block_on(library.fixture_cached_page(
                "fixture-key-only",
                "paging",
                offset,
                crate::gifs::Page {
                    items,
                    next_offset: next,
                },
            ));
        }
        page.query.replace("paging".into());
        page.load(false);
        wait(|| {
            page.items.borrow().len() == 6
                && page.offset.borrow().is_none()
                && page.request.borrow().is_none()
        });
        assert!(!page.failed.get());
        for (query, id) in [("old-query", "stale"), ("new-query", "fresh")] {
            let mut item = source.clone();
            item.id = id.into();
            context.block_on(library.fixture_cached_page(
                "fixture-key-only",
                query,
                0,
                crate::gifs::Page {
                    items: vec![item],
                    next_offset: None,
                },
            ));
        }
        page.query.replace("old-query".into());
        page.load(false);
        page.query.replace("new-query".into());
        page.load(false);
        wait(|| page.request.borrow().is_none());
        assert_eq!(
            page.items
                .borrow()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fresh"]
        );
        println!(
            "GIF PAGING PASS three cached provider pages append automatically, then exhaust; stale query result rejected"
        );
        window.close();
        println!(
            "GIF NATIVE PASS keyless Add key, no gear, 2000-item viewport controls, overlay favorite persistence, <400ms first/scroll feedback, duplicate append and automatic paging/error/exhaustion guards"
        );
    }
    fn descendants(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let mut pending = vec![widget.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = pending.pop() {
            pending.extend(children(&widget));
            found.push(widget);
        }
        found
    }
    fn drag_controls(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut controls: Vec<_> = descendants(root)
            .into_iter()
            .filter(|widget| {
                widget.is_mapped()
                    && (0..widget.observe_controllers().n_items()).any(|i| {
                        widget
                            .observe_controllers()
                            .item(i)
                            .is_some_and(|controller| controller.is::<gtk::DragSource>())
                    })
            })
            .collect();
        controls.sort_by_key(|widget| {
            let bounds = widget.compute_bounds(root).unwrap();
            (bounds.y().round() as i32, bounds.x().round() as i32)
        });
        controls
    }
    fn capture_favorites(window: &adw::ApplicationWindow, name: &str) {
        if let Some(prefix) = std::env::var_os("MONOGRAM_FAVORITE_CAPTURE") {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(window)).snapshot(
                &snapshot,
                window.width() as f64,
                window.height() as f64,
            );
            window
                .renderer()
                .unwrap()
                .render_texture(&snapshot.to_node().unwrap(), None)
                .save_to_png(format!("{}-{name}.png", prefix.to_string_lossy()))
                .unwrap();
        }
    }
    fn physical_drag(
        window: &adw::ApplicationWindow,
        source: &gtk::Widget,
        target: &gtk::Widget,
    ) -> std::thread::JoinHandle<()> {
        let window_id = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
                "--name",
                "Monogram",
            ])
            .output()
            .unwrap();
        assert!(window_id.status.success());
        let window_id = String::from_utf8(window_id.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let center = |widget: &gtk::Widget| {
            let b = widget.compute_bounds(window).unwrap();
            (
                (b.x() + b.width() / 2.).round() as i32,
                (b.y() + b.height() / 2.).round() as i32,
            )
        };
        let (sx, sy) = center(source);
        let (tx, ty) = center(target);
        std::thread::spawn(move || {
            let command = |args: &[String]| {
                assert!(
                    std::process::Command::new("xdotool")
                        .args(args)
                        .status()
                        .unwrap()
                        .success()
                )
            };
            let motion = |x: i32, y: i32| {
                command(&[
                    "mousemove".into(),
                    "--sync".into(),
                    "--window".into(),
                    window_id.clone(),
                    x.to_string(),
                    y.to_string(),
                ])
            };
            motion(sx, sy);
            command(&["mousedown".into(), "1".into()]);
            std::thread::sleep(Duration::from_millis(100));
            for step in 1..=8 {
                motion(sx + (tx - sx) * step / 8, sy + (ty - sy) * step / 8);
                std::thread::sleep(Duration::from_millis(35));
            }
            std::thread::sleep(Duration::from_millis(100));
            command(&["mouseup".into(), "1".into()]);
        })
    }
    #[test]
    #[ignore = "requires isolated native X11 display, data roots, xdotool and Secret Service"]
    fn favorite_drag_reorders_real_emoji_sticker_and_gif_views() {
        const OWNER: &str = "favorite-drag@fixture.invalid";
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.FavoriteFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let ui = Ui::new(&app, false);
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        ui.store
            .save_account(&crate::model::Account {
                jid: OWNER.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let mut state = crate::model::State::live(OWNER, vec![]);
        state.active = state.chat_for("recipient@fixture.invalid").id.clone();
        ui.activate_state(state);
        ui.show_main();
        ui.store.set_emoji_favorite("❤️", true).unwrap();
        ui.store.set_emoji_favorite("😅", true).unwrap();
        ui.window.set_default_size(1400, 900);
        ui.window.present();
        ui.toggle_media();
        wait(|| {
            ui.pack_pages.borrow().len() == 2
                && ui.gif_page.borrow().is_some()
                && ui.favorite_orders.borrow().len() == 3
        });
        let context = glib::MainContext::default();
        let library = ui.pack_library.borrow().as_ref().unwrap().clone();
        context.block_on(async {
            for (kind, count) in [(Kind::Emoji, 1), (Kind::Stickers, 3)] {
                let inputs = (0..count)
                    .map(|index| crate::packs::Input {
                        path: crate::widgets::asset("icons/mobile-emoji/msg_emoji_cat.png"),
                        name: format!("Favorite {index}"),
                        emoji: "🐱".into(),
                    })
                    .collect();
                let pack = library
                    .create("Favorite drag".into(), kind, inputs, Job::new())
                    .await
                    .unwrap();
                for item in pack.items {
                    library
                        .mutate(crate::packs::Mutation::Favorite {
                            item: item.id,
                            value: true,
                        })
                        .await
                        .unwrap();
                }
            }
        });
        for page in ui.pack_pages.borrow().iter() {
            page.refresh();
        }
        let emoji = ui
            .pack_pages
            .borrow()
            .iter()
            .find(|page| page.kind == Kind::Emoji)
            .unwrap()
            .clone();
        let stickers = ui
            .pack_pages
            .borrow()
            .iter()
            .find(|page| page.kind == Kind::Stickers)
            .unwrap()
            .clone();
        wait(|| {
            emoji.favorite_keys.borrow().len() == 3 && stickers.favorite_keys.borrow().len() == 3
        });
        let mut expected = std::collections::HashMap::new();
        for (name, page, category) in [
            ("emoji", emoji.clone(), FavoriteKind::Emoji),
            ("stickers", stickers.clone(), FavoriteKind::Stickers),
        ] {
            ui.media_stack.set_visible_child_name(name);
            wait(|| {
                !ui.media_stack.is_transition_running()
                    && drag_controls(&page.root.clone().upcast()).len() == 3
            });
            let mut keys = page.favorite_keys.borrow().clone();
            let source = keys.remove(0);
            keys.push(source);
            let widgets = drag_controls(&page.root.clone().upcast());
            let pointer = physical_drag(&ui.window, &widgets[0], &widgets[2]);
            wait(|| *page.favorite_keys.borrow() == keys && !ui.favorite_reorder_pending.get());
            pointer.join().unwrap();
            assert!(ui.composer_text().is_empty());
            assert!(ui.state.borrow().current().messages.is_empty());
            capture_favorites(&ui.window, name);
            expected.insert(category, keys);
            println!(
                "FAVORITE DRAG PASS {name}: real pointer drag reorders production view without sending"
            );
        }
        let gif_library = ui.gif_library.borrow().as_ref().unwrap().clone();
        context.block_on(async {
            for index in 0..3 {
                let path=std::path::PathBuf::from(std::env::var_os("XDG_CACHE_HOME").unwrap()).join(format!("favorite-{index}.gif"));
                let mut bytes=b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0\x3b".to_vec();bytes[13]=index*70;std::fs::write(&path,bytes).unwrap();
                let item=gif_library.import_local(path,format!("Favorite GIF {index}"),crate::gifs::Request::new()).await.unwrap();gif_library.set_favorite(item,true,crate::gifs::Request::new()).await.unwrap();
            }
        });
        let gifs = ui.gif_page.borrow().as_ref().unwrap().clone();
        gifs.show_saved();
        ui.media_stack.set_visible_child_name("gifs");
        wait(|| {
            !ui.media_stack.is_transition_running()
                && gifs.items.borrow().len() == 3
                && drag_controls(&gifs.root.clone().upcast()).len() == 3
        });
        let mut ids: Vec<_> = gifs
            .items
            .borrow()
            .iter()
            .map(|item| item.id.clone())
            .collect();
        let source = ids.remove(0);
        ids.push(source);
        let widgets = drag_controls(&gifs.root.clone().upcast());
        let pointer = physical_drag(&ui.window, &widgets[0], &widgets[2]);
        wait(|| {
            gifs.items
                .borrow()
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>()
                == ids
                && !ui.favorite_reorder_pending.get()
        });
        pointer.join().unwrap();
        assert!(ui.attachments.borrow().is_empty());
        assert!(ui.composer_text().is_empty());
        assert!(ui.state.borrow().current().messages.is_empty());
        capture_favorites(&ui.window, "gifs");
        let saved = context.block_on(gif_library.snapshot()).unwrap();
        assert_eq!(saved.favorites.len(), 3);
        assert!(saved.recents.is_empty());
        ui.load_favorite_orders();
        wait(|| ui.favorite_orders.borrow().len() == 3);
        assert_eq!(
            *emoji.favorite_keys.borrow(),
            expected[&FavoriteKind::Emoji]
        );
        assert_eq!(
            *stickers.favorite_keys.borrow(),
            expected[&FavoriteKind::Stickers]
        );
        assert_eq!(
            gifs.items
                .borrow()
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(ui.store.emoji_preferences().unwrap().favorites.len(), 2);
        let snapshot = context.block_on(library.snapshot()).unwrap();
        assert_eq!(snapshot.favorites.len(), 4);
        assert!(snapshot.recent.is_empty());
        ui.window.close();
        println!(
            "FAVORITE DRAG PASS GIFs and actual database reload: membership and recents unchanged, no attachment staged"
        );
    }
}
