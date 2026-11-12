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

pub(crate) struct GifLibraryPage {
    pub(crate) root: gtk::Box,
    library: crate::gifs::Library,
    status: gtk::Label,
    grid: gtk::FlowBox,
    scroll: gtk::ScrolledWindow,
    items: RefCell<Vec<crate::gifs::GifItem>>,
    rendered: Cell<usize>,
    more: gtk::Button,
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
        on_send: impl Fn(std::path::PathBuf) + 'static,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("gif-library-page");
        let settings = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("GIF search settings")
            .halign(gtk::Align::End)
            .build();
        settings.add_css_class("flat");
        root.append(&settings);
        let status = gtk::Label::builder().xalign(0.).wrap(true).build();
        status.add_css_class("dim-label");
        root.append(&status);
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(2)
            .max_children_per_line(2)
            .homogeneous(true)
            .column_spacing(6)
            .row_spacing(6)
            .build();
        grid.add_css_class("gif-library-grid");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&grid)
            .vexpand(true)
            .build();
        root.append(&scroll);
        let more = gtk::Button::with_label("Load More");
        more.set_visible(false);
        root.append(&more);
        let page = Rc::new(Self {
            root,
            library,
            status,
            grid,
            scroll: scroll.clone(),
            items: RefCell::new(Vec::new()),
            rendered: Cell::new(0),
            more,
            request: RefCell::new(None),
            request_serial: Cell::new(0),
            query: RefCell::new(String::new()),
            offset: RefCell::new(None),
            on_send: Box::new(on_send),
        });
        {
            let weak = Rc::downgrade(&page);
            scroll
                .vadjustment()
                .connect_value_changed(move |adjustment| {
                    if adjustment.value() + adjustment.page_size() + 180. >= adjustment.upper() {
                        if let Some(page) = weak.upgrade() {
                            page.render_next_gifs();
                        }
                    }
                });
        }
        {
            let weak = Rc::downgrade(&page);
            scroll.vadjustment().connect_changed(move |adjustment| {
                if adjustment.upper() <= adjustment.page_size() + 180.
                    && let Some(page) = weak.upgrade()
                {
                    page.render_next_gifs();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            scroll.connect_map(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.render_next_gifs();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            page.more.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.load(true);
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            let parent = parent.downgrade();
            settings.connect_clicked(move |_| {
                if let (Some(page), Some(parent)) = (weak.upgrade(), parent.upgrade()) {
                    page.settings(&parent);
                }
            });
        }
        page.show_saved();
        page
    }

    pub(crate) fn bind_search(self: &Rc<Self>, search: &gtk::SearchEntry) {
        let weak = Rc::downgrade(self);
        search.connect_search_changed(move |search| {
            let Some(page) = weak.upgrade() else {
                return;
            };
            if !page.root.is_mapped() {
                return;
            }
            page.query.replace(search.text().trim().to_string());
            page.offset.replace(None);
            page.load(false);
        });
    }

    fn show_saved(self: &Rc<Self>) {
        self.status.set_label("Loading saved GIFs…");
        let library = self.library.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = library.snapshot().await;
            let Some(page) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(snapshot) => {
                    let mut items = snapshot.favorites;
                    let saved = items
                        .iter()
                        .map(|item| item.id.clone())
                        .collect::<std::collections::HashSet<_>>();
                    items.extend(
                        snapshot
                            .recents
                            .into_iter()
                            .filter(|item| !saved.contains(&item.id)),
                    );
                    page.status.set_label(if items.is_empty() {
                        "Saved GIFs appear here. Add a GIPHY key to search online."
                    } else {
                        "Saved GIFs"
                    });
                    page.render(items, false);
                }
                Err(error) => page.status.set_label(&error.to_string()),
            }
        });
    }

    fn load(self: &Rc<Self>, append: bool) {
        if let Some(request) = self.request.borrow_mut().take() {
            request.cancel();
        }
        let offset = append.then(|| *self.offset.borrow()).flatten();
        if append && offset.is_none() {
            return;
        }
        self.status.set_label(if self.query.borrow().is_empty() {
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
            let result = library.search(query, offset, request.clone()).await;
            let Some(page) = weak
                .upgrade()
                .filter(|page| page.request_serial.get() == serial)
            else {
                return;
            };
            page.request.take();
            match result {
                Ok(result) => {
                    page.offset.replace(result.next_offset);
                    page.more.set_visible(result.next_offset.is_some());
                    page.status.set_label(if result.items.is_empty() {
                        "No GIFs found"
                    } else {
                        "Powered by GIPHY"
                    });
                    page.render(result.items, append);
                }
                Err(error) => page.status.set_label(&error.to_string()),
            }
        });
    }

    fn render(self: &Rc<Self>, items: Vec<crate::gifs::GifItem>, append: bool) {
        if !append {
            while let Some(child) = self.grid.first_child() {
                self.grid.remove(&child);
            }
            self.items.replace(items);
            self.rendered.set(0);
        } else {
            self.items.borrow_mut().extend(items);
        }
        self.render_next_gifs();
    }

    fn render_next_gifs(self: &Rc<Self>) {
        let start = self.rendered.get();
        let end = (start + 10).min(self.items.borrow().len());
        if start >= end {
            return;
        }
        let items = self.items.borrow()[start..end].to_vec();
        self.rendered.set(end);
        for item in items {
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 2);
            tile.add_css_class("gif-library-tile");
            let button = gtk::Button::builder().tooltip_text(&item.title).build();
            button.add_css_class("gif-library-item");
            let spinner = gtk::Spinner::new();
            spinner.start();
            button.set_child(Some(&spinner));
            tile.append(&button);
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
            tile.append(&favorite);
            self.grid.insert(&tile, -1);
            let weak = Rc::downgrade(self);
            let preview_library = self.library.clone();
            let preview_item = item.clone();
            let button_weak = button.downgrade();
            glib::spawn_future_local(async move {
                let request = crate::gifs::Request::new();
                let Ok(asset) = preview_library.preview(preview_item.clone(), request).await else {
                    return;
                };
                let (Some(path), Some(file)) = (asset.path.clone(), asset.attachment()) else {
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
                if let (Some(page), Some(button), Ok(Ok(image))) =
                    (weak.upgrade(), button_weak.upgrade(), decoded)
                {
                    let picture = gtk::Picture::for_paintable(&image.texture);
                    picture.set_content_fit(gtk::ContentFit::Cover);
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
            let favorite_library = self.library.clone();
            let favorite_item = item.clone();
            favorite.connect_toggled(move |button| {
                let enabled = button.is_active();
                button.set_icon_name(if enabled {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                });
                let library = favorite_library.clone();
                let item = favorite_item.clone();
                glib::spawn_future_local(async move {
                    let _ = library
                        .set_favorite(item, enabled, crate::gifs::Request::new())
                        .await;
                });
            });
            let weak = Rc::downgrade(self);
            let library = self.library.clone();
            button.connect_clicked(move |_| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                let library = library.clone();
                let item = item.clone();
                let weak = Rc::downgrade(&page);
                glib::spawn_future_local(async move {
                    let request = crate::gifs::Request::new();
                    let Ok(asset) = library.original(item.clone(), request.clone()).await else {
                        return;
                    };
                    let Some(path) = asset.path else {
                        return;
                    };
                    let _ = library.remember(item, crate::gifs::Request::new()).await;
                    if let Some(page) = weak.upgrade() {
                        (page.on_send)(path);
                    }
                });
            });
        }
    }

    fn settings(self: &Rc<Self>, parent: &adw::ApplicationWindow) {
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
            glib::spawn_future_local(async move {
                let result = if remove {
                    library.remove_key().await
                } else {
                    library.save_key(value).await
                };
                if let Err(error) = result {
                    page.status.set_label(&error.to_string());
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
        .build();
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
    grid: gtk::FlowBox,
    scroll: gtk::ScrolledWindow,
    items: RefCell<Vec<crate::packs::Item>>,
    favorites: RefCell<std::collections::HashSet<i64>>,
    rendered: Cell<usize>,
    preview_active: Cell<usize>,
    preview_epoch: Cell<u64>,
    preview_queue: RefCell<Vec<PackPreviewCell>>,
    selected: RefCell<Option<i64>>,
    sections: RefCell<std::collections::HashMap<i64, Vec<crate::packs::Item>>>,
    job: RefCell<Option<Job>>,
    on_select: Box<dyn Fn(PackSelection)>,
    on_view: Box<dyn Fn(i64)>,
    on_manage: Box<dyn Fn(&Rc<PackLibraryPage>, PackLibraryAction, Option<i64>)>,
}

impl PackLibraryPage {
    pub(crate) fn new(
        library: Library,
        kind: Kind,
        selector_hosts: Option<(gtk::Box, gtk::Box, gtk::ToggleButton)>,
        on_select: impl Fn(PackSelection) + 'static,
        on_view: impl Fn(i64) + 'static,
        on_manage: impl Fn(&Rc<PackLibraryPage>, PackLibraryAction, Option<i64>) + 'static,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("pack-library-page");
        root.set_vexpand(true);
        let external_selector = selector_hosts.is_some();
        let (selector, settings, group_anchor) = if let Some(hosts) = selector_hosts {
            (hosts.0, hosts.1, Some(hosts.2))
        } else {
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            bar.add_css_class("pack-selector");
            let selector = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let settings = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            bar.append(&selector);
            bar.append(&settings);
            let selector_scroll = crate::widgets::horizontal_strip(&bar);
            selector_scroll.set_hexpand(true);
            root.append(&selector_scroll);
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
            menu.append(button);
        }
        let popover = gtk::Popover::builder().child(&menu).build();
        let settings_icon =
            crate::emoji_picker_ui::mobile_emoji_icon("smiles_tab_settings.webp", 24);
        settings_icon.add_css_class("emoji-category-icon");
        let gear = gtk::MenuButton::builder()
            .child(&settings_icon)
            .tooltip_text("Pack settings")
            .popover(&popover)
            .build();
        gear.add_css_class("flat");
        gear.add_css_class("emoji-category");
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
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(4)
            .max_children_per_line(4)
            .column_spacing(6)
            .row_spacing(6)
            .homogeneous(true)
            .build();
        grid.add_css_class("pack-library-grid");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
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
            grid,
            scroll: scroll.clone(),
            items: RefCell::new(Vec::new()),
            favorites: RefCell::new(std::collections::HashSet::new()),
            rendered: Cell::new(0),
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
            scroll
                .vadjustment()
                .connect_value_changed(move |adjustment| {
                    if adjustment.value() + adjustment.page_size() + 160. >= adjustment.upper() {
                        if let Some(page) = weak.upgrade() {
                            page.render_more(16);
                            page.pump_pack_previews();
                        }
                    }
                });
        }
        {
            let weak = Rc::downgrade(&page);
            scroll.vadjustment().connect_changed(move |adjustment| {
                if let Some(page) = weak.upgrade() {
                    if adjustment.upper() <= adjustment.page_size() + 160. {
                        page.render_more(16);
                    }
                    page.pump_pack_previews();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            scroll.connect_map(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.render_more(16);
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
                .map(|(id, _, items)| (*id, items.clone()))
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
                let items = items.clone();
                button.connect_toggled(move |button| {
                    if button.is_active()
                        && let Some(page) = weak.upgrade()
                    {
                        page.selected.replace(Some(id));
                        page.show_items(items.clone());
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
                    let Ok((asset, _)) = library.verified_asset(item, Job::new()).await else {
                        return;
                    };
                    let Some(button) = preview.upgrade() else {
                        return;
                    };
                    let picture = gtk::Image::new();
                    if let Ok(pixbuf) =
                        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(asset.path, 28, 28, true)
                    {
                        picture.set_from_pixbuf(Some(&pixbuf));
                    }
                    button.set_child(Some(&picture));
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
            self.show_pack(packs.iter().find(|pack| pack.id == selected));
        } else {
            self.show_section(selected);
        }
    }

    fn show_pack(self: &Rc<Self>, pack: Option<&Pack>) {
        self.show_items(pack.map(|pack| pack.items.clone()).unwrap_or_default());
    }

    pub(crate) fn show_section(self: &Rc<Self>, id: i64) {
        self.selected.replace(Some(id));
        self.show_items(self.sections.borrow().get(&id).cloned().unwrap_or_default());
    }

    fn show_items(self: &Rc<Self>, items: Vec<crate::packs::Item>) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.preview_epoch
            .set(self.preview_epoch.get().wrapping_add(1));
        self.preview_queue.borrow_mut().clear();
        self.items.replace(items);
        self.rendered.set(0);
        let empty = self.items.borrow().is_empty();
        self.status.set_visible(empty);
        if empty {
            self.status.set_label(match *self.selected.borrow() {
                Some(-1) => "No favorites yet.",
                Some(-2) => "No recent items yet.",
                _ => match self.kind {
                    Kind::Stickers => "This sticker pack is empty.",
                    Kind::Emoji => "This custom emoji pack is empty.",
                },
            });
        }
        self.render_more(16);
    }

    fn render_more(self: &Rc<Self>, count: usize) {
        let start = self.rendered.get();
        let end = (start + count).min(self.items.borrow().len());
        if start >= end {
            return;
        }
        let items = self.items.borrow()[start..end].to_vec();
        self.rendered.set(end);
        for item in items {
            let button = gtk::Button::new();
            button.add_css_class("flat");
            button.add_css_class("pack-item");
            button.set_size_request(64, 64);
            button.set_halign(gtk::Align::Start);
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
            self.grid.insert(&button, -1);
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
        }
        self.pump_pack_previews();
    }

    fn pump_pack_previews(self: &Rc<Self>) {
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
                let Some(pack) = selected else {
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
        let peer = self.state.borrow().current().jid.clone();
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
            let page = GifLibraryPage::new(library.clone(), &ui.window, move |path| {
                if let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation
                        && ui.state.borrow().account == send_owner
                        && ui.state.borrow().current().jid == peer
                }) {
                    ui.send_library_file(&path, false);
                }
            });
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
