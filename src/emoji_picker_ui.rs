//! Native reaction consent and shared emoji-picker presentation.
use crate::ui::favorite_order::{self, Kind as FavoriteKind};
use crate::{emoji_completion::EmojiEntry, model::Message, ui::Ui, widgets::*};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

thread_local! {
    static EMOJI_SURFACES: RefCell<Vec<(Weak<Ui>, Weak<EmojiSurface>)>> = const { RefCell::new(Vec::new()) };
}

type EmojiAction = dyn Fn(&Rc<Ui>, &str);

struct EmojiCategory {
    icon: &'static str,
    title: &'static str,
    groups: &'static [u32],
}

const EMOJI_CATEGORIES: [EmojiCategory; 8] = [
    EmojiCategory {
        icon: "msg_emoji_smiles.webp",
        title: "Smileys and People",
        groups: &[0, 1, 2],
    },
    EmojiCategory {
        icon: "msg_emoji_cat.webp",
        title: "Animals and Nature",
        groups: &[3],
    },
    EmojiCategory {
        icon: "msg_emoji_food.webp",
        title: "Food and Drink",
        groups: &[4],
    },
    EmojiCategory {
        icon: "msg_emoji_activities.webp",
        title: "Activities",
        groups: &[6],
    },
    EmojiCategory {
        icon: "msg_emoji_travel.webp",
        title: "Travel and Places",
        groups: &[5],
    },
    EmojiCategory {
        icon: "msg_emoji_objects.webp",
        title: "Objects",
        groups: &[7],
    },
    EmojiCategory {
        icon: "msg_emoji_other.webp",
        title: "Symbols",
        groups: &[8],
    },
    EmojiCategory {
        icon: "msg_emoji_flags.webp",
        title: "Flags",
        groups: &[9],
    },
];

#[derive(Clone)]
enum EmojiRow {
    Heading(String),
    Items(Vec<EmojiEntry>, bool),
    Empty(&'static str),
    Quick,
}

struct SharedQuickRow {
    emojis: Vec<String>,
    row: gtk::Box,
    buttons: Vec<(String, gtk::Button)>,
    height: i32,
    slot: RefCell<glib::WeakRef<gtk::Box>>,
    bind: RefCell<Option<Box<dyn Fn(&gtk::Box)>>>,
}

struct EmojiSurface {
    ui: Weak<Ui>,
    root: gtk::Box,
    categories: gtk::Box,
    category_row: gtk::Box,
    quick: RefCell<Option<Rc<SharedQuickRow>>>,
    pack_tabs: gtk::Box,
    settings: gtk::Box,
    group_anchor: RefCell<Option<gtk::ToggleButton>>,
    custom_page: RefCell<Option<Rc<crate::media_library_ui::PackLibraryPage>>>,
    emoji_label: gtk::Label,
    emoji_scroll: gtk::ScrolledWindow,
    model: gio::ListStore,
    list: gtk::ListView,
    columns: usize,
    anchors: RefCell<std::collections::BTreeMap<i32, u32>>,
    favorite_keys: RefCell<Vec<String>>,
    category: Cell<i32>,
    query: RefCell<String>,
    activate: Rc<EmojiAction>,
}

impl EmojiSurface {
    fn new(
        ui: &Rc<Ui>,
        columns: u32,
        height: Option<i32>,
        activate: impl Fn(&Rc<Ui>, &str) + 'static,
    ) -> Rc<Self> {
        let surface = Rc::new_cyclic(|surface: &Weak<EmojiSurface>| {
            let model = gio::ListStore::new::<glib::BoxedAnyObject>();
            let factory = emoji_factory(surface);
            let list = gtk::ListView::new(
                Some(gtk::NoSelection::new(Some(model.clone()))),
                Some(factory),
            );
            list.add_css_class("picker-section-list");

            let categories = bx(false, 2);
            categories.add_css_class("emoji-category-bar");
            let pack_tabs = bx(false, 2);
            let settings = bx(false, 2);
            let root = bx(true, 6);
            root.add_css_class("emoji-picker-surface");
            let category_scroll = horizontal_strip(&categories);
            category_scroll.add_css_class("emoji-category-scroll");
            let category_row = bx(false, 2);
            category_row.add_css_class("emoji-category-row");
            category_scroll.set_hexpand(true);
            category_row.append(&category_scroll);
            category_row.append(&settings);
            root.append(&category_row);
            let emoji_label = text("Favorites", "caption dim-label");
            root.append(&emoji_label);
            let scrolling = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .child(&list)
                .vexpand(height.is_none())
                .build();
            if let Some(height) = height {
                scrolling.set_min_content_height(height);
                scrolling.set_max_content_height(height);
                scrolling.set_propagate_natural_height(true);
            }
            root.append(&scrolling);
            Self {
                ui: Rc::downgrade(ui),
                root,
                categories,
                category_row,
                quick: RefCell::new(None),
                pack_tabs,
                settings,
                group_anchor: RefCell::new(None),
                custom_page: RefCell::new(None),
                emoji_label,
                emoji_scroll: scrolling,
                model,
                list,
                columns: columns as usize,
                anchors: RefCell::new(Default::default()),
                favorite_keys: RefCell::new(Vec::new()),
                category: Cell::new(-1),
                query: RefCell::new(String::new()),
                activate: Rc::new(activate),
            }
        });
        EMOJI_SURFACES.with(|surfaces| {
            surfaces
                .borrow_mut()
                .push((Rc::downgrade(ui), Rc::downgrade(&surface)));
        });
        surface.populate_categories();
        surface.refresh();
        surface
    }

    fn populate_categories(self: &Rc<Self>) {
        let mut previous: Option<gtk::ToggleButton> = None;
        for (index, icon, title) in [
            (-1, "emoji_tabs_faves.webp", "Favorites"),
            (-2, "msg_emoji_recent.webp", "Recent"),
        ] {
            let button = emoji_category_button(icon, title);
            button.add_css_class("flat");
            button.add_css_class("emoji-category");
            button.update_property(&[gtk::accessible::Property::Label(title)]);
            if let Some(previous) = previous.as_ref() {
                button.set_group(Some(previous));
            }
            button.set_active(index == -1);
            let surface = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(surface) = surface.upgrade()
                {
                    surface.category.set(index);
                    if let Some(page) = surface.custom_page.borrow().as_ref().cloned() {
                        page.show_section(i64::from(index));
                        page.root.set_visible(true);
                        return;
                    }
                    surface.jump_category(index);
                }
            });
            self.categories.append(&button);
            if index == -1 {
                self.group_anchor.replace(Some(button.clone()));
            }
            previous = Some(button);
        }
        self.categories.append(&self.pack_tabs);
        for (index, category) in EMOJI_CATEGORIES.iter().enumerate() {
            let button = emoji_category_button(category.icon, category.title);
            button.add_css_class("flat");
            button.add_css_class("emoji-category");
            button.update_property(&[gtk::accessible::Property::Label(category.title)]);
            if let Some(previous) = previous.as_ref() {
                button.set_group(Some(previous));
            }
            let surface = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(surface) = surface.upgrade()
                {
                    surface.category.set(index as i32);
                    if let Some(page) = surface.custom_page.borrow().as_ref().cloned() {
                        page.show_section(-10 - index as i64);
                        return;
                    }
                    surface.jump_category(index as i32);
                }
            });
            self.categories.append(&button);
            previous = Some(button);
        }
    }

    fn bind_search(self: &Rc<Self>, search: &gtk::SearchEntry, keep_alive: bool) {
        if keep_alive {
            let surface = self.clone();
            search.connect_search_changed(move |search| {
                surface.set_query(search.text().as_str());
            });
        } else {
            let surface = Rc::downgrade(self);
            search.connect_search_changed(move |search| {
                if let Some(surface) = surface.upgrade() {
                    surface.set_query(search.text().as_str());
                }
            });
        }
    }

    fn set_query(self: &Rc<Self>, query: &str) {
        if self.query.borrow().as_str() == query {
            return;
        }
        self.query.replace(query.to_owned());
        if let Some(page) = self.custom_page.borrow().as_ref().cloned() {
            page.search(query);
            return;
        }
        let searching = !query.trim().is_empty();
        self.categories.set_sensitive(!searching);
        if searching {
            self.emoji_label.set_visible(true);
            self.emoji_scroll.set_visible(true);
        } else if self.category.get() < -2 {
            self.emoji_label.set_visible(false);
            self.emoji_scroll.set_visible(false);
        }
        if let Some(page) = self.custom_page.borrow().as_ref().cloned() {
            page.root.set_visible(!searching && self.category.get() < 0);
        }
        self.refresh();
    }

    fn refresh(self: &Rc<Self>) {
        if let Some(page) = self.custom_page.borrow().as_ref().cloned() {
            self.emoji_label.set_visible(false);
            self.emoji_scroll.set_visible(false);
            page.root.set_visible(true);
            page.rebuild_rows();
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preferences = ui.store.emoji_preferences().unwrap_or_default();
        if let Some(order) = ui.favorite_orders.borrow().get(&FavoriteKind::Emoji) {
            order.sort(&mut preferences.favorites, |emoji| {
                favorite_order::emoji_key(emoji)
            });
        }
        let query = self.query.borrow().clone();
        let searching = !query.trim().is_empty();
        let catalog = crate::emoji_completion::catalog();
        let favorite_entries = preferences
            .favorites
            .iter()
            .filter_map(|emoji| catalog.iter().find(|entry| entry.text == *emoji).cloned())
            .collect::<Vec<_>>();
        self.favorite_keys.replace(
            favorite_entries
                .iter()
                .map(|entry| favorite_order::emoji_key(&entry.text))
                .collect(),
        );
        let mut rows = Vec::new();
        let mut anchors = std::collections::BTreeMap::new();
        let shared = self.quick.borrow().clone();
        if let Some(shared) = &shared {
            let mut ordered = shared.buttons.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|(emoji, _)| {
                preferences
                    .favorites
                    .iter()
                    .position(|value| value == emoji)
                    .unwrap_or(usize::MAX)
            });
            let mut previous = None::<gtk::Button>;
            for (_, button) in ordered {
                shared.row.reorder_child_after(button, previous.as_ref());
                previous = Some(button.clone());
            }
        }
        let mut sections = Vec::new();
        if searching {
            sections.push((
                -3,
                "Search Results",
                crate::emoji_completion::search(&query),
                false,
            ));
        } else {
            if shared.is_some() {
                anchors.insert(-4, 0);
                rows.push(glib::BoxedAnyObject::new(EmojiRow::Quick));
            }
            sections.push((-1, "Favorites", favorite_entries, true));
            sections.push((
                -2,
                "Recent",
                preferences
                    .recent
                    .iter()
                    .filter_map(|emoji| catalog.iter().find(|entry| entry.text == *emoji).cloned())
                    .collect(),
                false,
            ));
            for (index, category) in EMOJI_CATEGORIES.iter().enumerate() {
                sections.push((
                    index as i32,
                    category.title,
                    catalog
                        .iter()
                        .filter(|entry| category.groups.contains(&entry.group))
                        .cloned()
                        .collect(),
                    false,
                ));
            }
        }
        for (id, title, entries, favorite) in sections {
            anchors.entry(id).or_insert(rows.len() as u32);
            rows.push(glib::BoxedAnyObject::new(EmojiRow::Heading(title.into())));
            if entries.is_empty() {
                rows.push(glib::BoxedAnyObject::new(EmojiRow::Empty(if searching {
                    "No matches"
                } else if favorite {
                    "No favorites yet"
                } else {
                    "No recent items yet"
                })));
            }
            for entries in entries.chunks(self.columns) {
                rows.push(glib::BoxedAnyObject::new(EmojiRow::Items(
                    entries.to_vec(),
                    favorite,
                )));
            }
        }
        self.anchors.replace(anchors);
        self.model.splice(0, self.model.n_items(), &rows);
        self.emoji_label.set_visible(false);
        self.emoji_scroll.set_visible(true);
    }

    fn jump_category(&self, category: i32) {
        if let Some(position) = self.anchors.borrow().get(&category) {
            self.list
                .scroll_to(*position, gtk::ListScrollFlags::NONE, None);
        }
    }

    fn emoji_button(self: &Rc<Self>, emoji: &str, name: &str, favorite: bool) -> gtk::Button {
        let reorder_key = favorite_order::emoji_key(emoji);
        let button = gtk::Button::builder()
            .label(emoji)
            .tooltip_text(format!(
                "{name} · Alt-click or right-click to favorite or pin"
            ))
            .build();
        button.add_css_class("flat");
        button.add_css_class("emoji-picker-item");
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{name}, {emoji}"
        ))]);
        let value = emoji.to_owned();
        let alternate_activation = Rc::new(Cell::new(false));
        let surface = Rc::downgrade(self);
        let emoji = value.clone();
        let skip_activation = alternate_activation.clone();
        button.connect_clicked(move |_| {
            if skip_activation.replace(false) {
                return;
            }
            let Some(surface) = surface.upgrade() else {
                return;
            };
            let Some(ui) = surface.ui.upgrade() else {
                return;
            };
            (surface.activate)(&ui, &emoji);
            let weak = Rc::downgrade(&ui);
            let emoji = emoji.clone();
            glib::spawn_future_local(async move {
                if let Some(ui) = weak.upgrade() {
                    if ui
                        .store
                        .write_snapshot(move |store| store.record_emoji_recent(&emoji))
                        .await
                        .is_ok()
                    {
                        refresh_emoji_surfaces(&ui);
                    }
                }
            });
        });
        let alternate = gtk::GestureClick::new();
        alternate.set_button(gdk::BUTTON_PRIMARY);
        alternate.set_propagation_phase(gtk::PropagationPhase::Capture);
        let surface = Rc::downgrade(self);
        let emoji = value.clone();
        let skip_activation = alternate_activation;
        alternate.connect_pressed(move |gesture, _, _, _| {
            if !gesture
                .current_event_state()
                .contains(gdk::ModifierType::ALT_MASK)
            {
                return;
            }
            skip_activation.set(true);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let Some(surface) = surface.upgrade() else {
                return;
            };
            let Some(ui) = surface.ui.upgrade() else {
                return;
            };
            let preferences = ui.store.emoji_preferences().unwrap_or_default();
            let favorite = preferences.favorites.iter().any(|value| value == &emoji);
            if ui.store.set_emoji_favorite(&emoji, !favorite).is_err() {
                ui.notice("Emoji preferences could not be saved");
            } else {
                refresh_emoji_surfaces(&ui);
            }
        });
        button.add_controller(alternate);
        let secondary = gtk::GestureClick::new();
        secondary.set_button(gdk::BUTTON_SECONDARY);
        let surface = Rc::downgrade(self);
        let target = button.downgrade();
        let emoji = value.clone();
        secondary.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let (Some(surface), Some(button)) = (surface.upgrade(), target.upgrade()) {
                surface.emoji_options(&button, &emoji, x, y);
            }
        });
        button.add_controller(secondary);
        let long_press = gtk::GestureLongPress::new();
        let surface = Rc::downgrade(self);
        let target = button.downgrade();
        let emoji = value.clone();
        long_press.connect_pressed(move |_, x, y| {
            if let (Some(surface), Some(button)) = (surface.upgrade(), target.upgrade()) {
                surface.emoji_options(&button, &emoji, x, y);
            }
        });
        button.add_controller(long_press);
        let keys = gtk::EventControllerKey::new();
        let surface = Rc::downgrade(self);
        let target = button.downgrade();
        let emoji = value;
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if (key == gdk::Key::F10 && modifiers.contains(gdk::ModifierType::SHIFT_MASK))
                || key == gdk::Key::Menu
            {
                if let (Some(surface), Some(button)) = (surface.upgrade(), target.upgrade()) {
                    surface.emoji_options(
                        &button,
                        &emoji,
                        f64::from(button.width()) / 2.,
                        f64::from(button.height()) / 2.,
                    );
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        button.add_controller(keys);
        if favorite && self.custom_page.borrow().is_none() {
            if let Some(ui) = self.ui.upgrade() {
                let surface = Rc::downgrade(self);
                favorite_order::attach(&ui, &button, FavoriteKind::Emoji, reorder_key, move || {
                    surface
                        .upgrade()
                        .filter(|surface| surface.query.borrow().trim().is_empty())
                        .map(|surface| surface.favorite_keys.borrow().clone())
                        .unwrap_or_default()
                });
            }
        }
        button
    }

    fn emoji_options(self: &Rc<Self>, button: &gtk::Button, emoji: &str, x: f64, y: f64) {
        let Some(ui) = self.ui.upgrade() else { return };
        let preferences = ui.store.emoji_preferences().unwrap_or_default();
        let favorite = preferences.favorites.iter().any(|value| value == emoji);
        let pinned = preferences.pinned.iter().any(|value| value == emoji);
        let popover = gtk::Popover::builder().has_arrow(false).build();
        popover.add_css_class("message-menu");
        popover.set_parent(button);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = bx(true, 2);
        menu.add_css_class("message-menu-panel");
        for (label, kind, enabled) in [
            (
                if favorite {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                },
                "favorite",
                favorite,
            ),
            (
                if pinned {
                    "Unpin Quick Reaction"
                } else {
                    "Pin as Quick Reaction"
                },
                "pin",
                pinned,
            ),
        ] {
            let choice = gtk::Button::with_label(label);
            choice.add_css_class("flat");
            let weak = Rc::downgrade(&ui);
            let emoji = emoji.to_owned();
            let popup = popover.downgrade();
            choice.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                let saved = if kind == "favorite" {
                    ui.store.set_emoji_favorite(&emoji, !enabled)
                } else {
                    ui.store.set_emoji_pinned(&emoji, !enabled)
                };
                if saved.is_err() {
                    ui.notice("Emoji preferences could not be saved");
                } else {
                    refresh_emoji_surfaces(&ui);
                }
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                    popup.unparent();
                }
            });
            menu.append(&choice);
        }
        popover.set_child(Some(&menu));
        popover.popup();
    }

    fn install_custom_page(self: &Rc<Self>, page: &Rc<crate::media_library_ui::PackLibraryPage>) {
        if let Some(previous) = self
            .custom_page
            .borrow()
            .as_ref()
            .cloned()
            .filter(|previous| !Rc::ptr_eq(previous, page))
        {
            self.root.remove(&previous.root);
        }
        if page.root.parent().is_none() {
            self.root.append(&page.root);
        }
        self.custom_page.replace(Some(page.clone()));
        self.emoji_label.set_visible(false);
        self.emoji_scroll.set_visible(false);
        page.root.set_visible(true);
    }

    fn show_custom_pack(&self) {
        self.emoji_label.set_visible(false);
        self.emoji_scroll.set_visible(false);
    }
}

fn emoji_category_button(icon: &str, title: &str) -> gtk::ToggleButton {
    let picture = mobile_emoji_icon(icon, 24);
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

pub(crate) fn mobile_emoji_icon(icon: &str, size: i32) -> gtk::Image {
    let picture = gtk::Image::new();
    picture.set_pixel_size(size);
    picture.set_size_request(size, size);
    if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(
        asset(&format!(
            "icons/mobile-emoji/{}",
            icon.replace(".webp", ".png")
        )),
        size,
        size,
        true,
    ) {
        picture.set_from_pixbuf(Some(&pixbuf));
    }
    picture
}

pub(crate) fn builtin_categories() -> Vec<(i64, &'static str, &'static [u32])> {
    EMOJI_CATEGORIES
        .iter()
        .enumerate()
        .map(|(index, category)| (-10 - index as i64, category.title, category.groups))
        .collect()
}
pub(crate) fn builtin_button(ui: &Rc<Ui>, entry: &EmojiEntry) -> Option<gtk::Button> {
    Some(sidebar_surface(ui)?.emoji_button(&entry.text, &entry.name, false))
}

fn sidebar_surface(ui: &Rc<Ui>) -> Option<Rc<EmojiSurface>> {
    EMOJI_SURFACES.with(|registry| {
        registry.borrow().iter().find_map(|(owner, surface)| {
            owner
                .upgrade()
                .filter(|owner| Rc::ptr_eq(owner, ui))
                .and_then(|_| surface.upgrade())
                .filter(|surface| surface.root.has_css_class("emoji-sidebar"))
        })
    })
}

pub(crate) fn emoji_pack_hosts(ui: &Rc<Ui>) -> Option<(gtk::Box, gtk::Box, gtk::ToggleButton)> {
    let surface = sidebar_surface(ui)?;
    Some((
        surface.pack_tabs.clone(),
        surface.settings.clone(),
        surface.group_anchor.borrow().clone()?,
    ))
}

pub(crate) fn attach_custom_emoji_page(
    ui: &Rc<Ui>,
    page: &Rc<crate::media_library_ui::PackLibraryPage>,
) {
    if let Some(surface) = sidebar_surface(ui) {
        surface.install_custom_page(page);
    }
}

pub(crate) fn show_custom_emoji_pack(ui: &Rc<Ui>) {
    if let Some(surface) = sidebar_surface(ui) {
        surface.show_custom_pack();
    }
}

fn emoji_factory(surface: &Weak<EmojiSurface>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let weak = surface.clone();
    factory.connect_bind(move |_, item| {
        let Some(surface) = weak.upgrade() else {
            return;
        };
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(data) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let row = data.borrow::<EmojiRow>();
        let child: gtk::Widget = match &*row {
            EmojiRow::Quick => {
                let slot = bx(true, 0);
                if let Some(quick) = surface.quick.borrow().as_ref() {
                    slot.set_height_request(quick.height);
                    quick.slot.replace(slot.downgrade());
                    if let Some(bind) = quick.bind.borrow().as_ref() {
                        bind(&slot);
                    }
                }
                slot.upcast()
            }
            EmojiRow::Heading(title) => {
                let label = text(title, "picker-section-heading");
                label.set_margin_top(8);
                label.set_margin_bottom(6);
                label.upcast()
            }
            EmojiRow::Empty(title) => text(title, "dim-label").upcast(),
            EmojiRow::Items(entries, favorite) => {
                let row = bx(false, 2);
                row.set_homogeneous(true);
                for entry in entries {
                    row.append(&crate::widgets::SquareCell::new(&surface.emoji_button(
                        &entry.text,
                        &entry.name,
                        *favorite,
                    )));
                }
                for _ in entries.len()..surface.columns {
                    row.append(&bx(false, 0));
                }
                row.upcast()
            }
        };
        item.set_child(Some(&child));
    });
    factory.connect_unbind(|_, item| {
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(gtk::Widget::NONE);
    });
    factory
}

pub(crate) fn refresh_emoji_surfaces(ui: &Rc<Ui>) {
    let surfaces = EMOJI_SURFACES.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry
            .retain(|(owner, surface)| owner.upgrade().is_some() && surface.upgrade().is_some());
        registry
            .iter()
            .filter_map(|(owner, surface)| {
                owner
                    .upgrade()
                    .filter(|owner| Rc::ptr_eq(owner, ui))
                    .and_then(|_| surface.upgrade())
            })
            .collect::<Vec<_>>()
    });
    for surface in surfaces {
        surface.refresh();
    }
}

pub(crate) struct ReactionPicker {
    pub root: gtk::Box,
    morph: gtk::Overlay,
    base: gtk::Box,
    expanded: gtk::Box,
    quick: gtk::Box,
    heading: gtk::Revealer,
    canvas: gtk::Box,
    arrow: gtk::Button,
    actions: gtk::Revealer,
    search: gtk::SearchEntry,
    shared: Rc<SharedQuickRow>,
    surface: Rc<EmojiSurface>,
    is_expanded: Cell<bool>,
    moving: Cell<bool>,
    progress: Cell<f64>,
    full_height: Cell<i32>,
    target: Cell<(f64, f64, i32)>,
    collapsed_radius: Cell<i32>,
    radius: Cell<i32>,
    radius_css: gtk::CssProvider,
    animation: RefCell<Option<adw::TimedAnimation>>,
}
impl ReactionPicker {
    pub fn attach_actions(&self, actions: &impl IsA<gtk::Widget>) {
        let inset = bx(true, 0);
        inset.set_margin_top(8);
        inset.append(actions);
        self.actions.set_child(Some(&inset));
    }
    fn detach_quick(&self) {
        if let Some(parent) = self.quick.parent() {
            if let Some(parent) = parent.downcast_ref::<gtk::Box>() {
                parent.remove(&self.quick);
            } else if let Some(parent) = parent.downcast_ref::<gtk::Fixed>() {
                parent.remove(&self.quick);
            }
        }
    }
    fn dock(&self, slot: &gtk::Box) {
        self.detach_quick();
        self.quick.set_width_request(-1);
        self.quick.remove_css_class("compact");
        slot.append(&self.quick);
        self.canvas.set_visible(false);
    }
    fn radius_styles(radius: i32) -> String {
        (12..=radius.max(12))
            .map(|radius| {
                format!(
                    ".reaction-morph.reaction-radius-{radius} {{ border-radius: {radius}px; }}"
                )
            })
            .collect()
    }
    fn capture_collapsed_radius(&self) {
        if self.progress.get() != 0. || self.moving.get() || self.is_expanded.get() {
            return;
        }
        let height = self.morph.height();
        if height <= 0 {
            return;
        }
        let radius = (height / 2).max(12);
        if radius != self.collapsed_radius.get() {
            self.collapsed_radius.set(radius);
            self.radius_css.load_from_string(&Self::radius_styles(radius));
        }
    }
    fn apply_radius(&self, progress: f64) {
        let collapsed = self.collapsed_radius.get() as f64;
        let radius = (collapsed + (12. - collapsed) * progress).round() as i32;
        let previous = self.radius.replace(radius);
        if previous != radius {
            self.morph
                .remove_css_class(&format!("reaction-radius-{previous}"));
            self.morph
                .add_css_class(&format!("reaction-radius-{radius}"));
        }
    }
    fn frame(&self, progress: f64) {
        self.progress.set(progress);
        self.apply_radius(progress);
        if let Some(slot) = self.shared.slot.borrow().upgrade() {
            if let Some(bounds) = slot.compute_bounds(&self.morph) {
                self.target.set((
                    bounds.x() as f64,
                    bounds.y() as f64,
                    bounds.width().round() as i32,
                ));
            }
        }
        let (x, y, width) = self.target.get();
        self.quick
            .set_width_request((308. + (width as f64 - 308.) * progress).round() as i32);
        self.canvas
            .set_margin_start((6. + (x - 6.) * progress).round() as i32);
        self.canvas
            .set_margin_top((4. + (y - 4.) * progress).round() as i32);
        self.base.set_height_request(
            (48. + (self.full_height.get() as f64 - 48.) * progress).round() as i32,
        );
    }
    fn set_expanded(self: &Rc<Self>, expanded: bool) {
        if self.is_expanded.get() == expanded && self.animation.borrow().is_none() {
            return;
        }
        if let Some(animation) = self.animation.borrow_mut().take() {
            animation.pause();
        }
        if expanded {
            self.capture_collapsed_radius();
        }
        if !expanded {
            self.search.set_text("");
            self.surface.set_query("");
            self.surface.emoji_scroll.vadjustment().set_value(0.);
        }
        self.moving.set(true);
        self.is_expanded.set(expanded);
        self.expanded.set_visible(true);
        self.expanded.set_can_target(expanded);
        if expanded {
            self.expanded.set_opacity(1.);
            self.expanded.set_sensitive(true);
            self.expanded
                .update_state(&[gtk::accessible::State::Hidden(false)]);
        }
        self.detach_quick();
        self.quick.add_css_class("compact");
        self.canvas.set_visible(true);
        self.canvas.append(&self.quick);
        self.arrow.set_visible(!expanded);
        self.heading.set_reveal_child(expanded);
        self.actions.set_reveal_child(!expanded);
        if expanded {
            self.morph.add_css_class("expanded");
        } else {
            self.morph.remove_css_class("expanded");
        }
        self.full_height.set(
            self.expanded
                .measure(gtk::Orientation::Vertical, 320)
                .1
                .max(40),
        );
        self.frame(self.progress.get());
        let weak = Rc::downgrade(self);
        let animation = adw::TimedAnimation::new(
            &self.morph,
            self.progress.get(),
            if expanded { 1. } else { 0. },
            350,
            adw::CallbackAnimationTarget::new(move |value| {
                if let Some(picker) = weak.upgrade() {
                    picker.frame(value);
                }
            }),
        );
        animation.set_easing(adw::Easing::EaseOutCubic);
        let weak = Rc::downgrade(self);
        animation.connect_done(move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.animation.borrow_mut().take();
                picker.moving.set(false);
                if picker.is_expanded.get() {
                    if let Some(slot) = picker.shared.slot.borrow().upgrade() {
                        picker.dock(&slot);
                    }
                    picker.search.grab_focus();
                } else {
                    picker.expanded.set_can_target(false);
                    picker.expanded.set_opacity(0.);
                    picker.expanded.set_sensitive(false);
                    picker
                        .expanded
                        .update_state(&[gtk::accessible::State::Hidden(true)]);
                    picker.quick.set_width_request(308);
                    picker.canvas.set_margin_start(6);
                    picker.canvas.set_margin_top(4);
                }
            }
        });
        self.animation.replace(Some(animation.clone()));
        animation.play();
    }
}

impl Ui {
    pub(crate) fn confirm_public_reaction(self: &Rc<Self>, id: &str, emoji: &str) {
        let state = self.state.borrow();
        let owner = state.account.clone();
        let peer = state.current().jid.clone();
        drop(state);
        if owner.is_empty() {
            return;
        }
        let generation = self.generation.get();
        let dialog = adw::AlertDialog::builder()
            .heading("Allow Public Reactions?")
            .body(format!(
                "Reacting with {emoji} sends the emoji and the target message reference as public XMPP metadata. Your server and this conversation’s server or participants can see it, even when messages are end-to-end encrypted.\n\nAllow public reactions for this conversation?"
            ))
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("allow", "Allow and React")]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        let emoji = emoji.to_owned();
        dialog.connect_response(Some("allow"), move |_, _| {
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.state.borrow().account == owner
                    && ui.state.borrow().current().jid == peer
            }) else {
                return;
            };
            let key = format!("plain-reactions:{owner}\n{peer}");
            if ui.store.set_pref(&key, "true").is_err() {
                ui.notice("Public reaction permission could not be saved");
                return;
            }
            ui.react(&id, &emoji);
        });
        dialog.present(Some(&self.window));
    }

    pub(crate) fn emoji_sidebar(self: &Rc<Self>, search: &gtk::SearchEntry) -> gtk::Widget {
        let surface = EmojiSurface::new(self, 8, None, |ui, emoji| {
            ui.composer.buffer().insert_at_cursor(emoji);
            ui.composer.grab_focus();
        });
        surface.root.add_css_class("emoji-sidebar");
        surface.bind_search(search, true);
        surface.root.clone().upcast()
    }

    pub(crate) fn message_reaction_picker(
        self: &Rc<Self>,
        popover: &gtk::Popover,
        message: &Message,
    ) -> Rc<ReactionPicker> {
        let mut preferences = self.store.emoji_preferences().unwrap_or_default();
        if let Some(order) = self.favorite_orders.borrow().get(&FavoriteKind::Emoji) {
            order.sort(&mut preferences.favorites, |emoji| {
                favorite_order::emoji_key(emoji)
            });
        }
        let emojis = preferences
            .quick_reactions(&message.reactions)
            .into_iter()
            .take(7)
            .collect::<Vec<_>>();
        let popup = popover.downgrade();
        let id = message.id.clone();
        let surface = EmojiSurface::new(self, 8, Some(330), move |ui, emoji| {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
                popup.unparent();
            }
            ui.react(&id, emoji);
        });
        let quick = bx(true, 0);
        quick.add_css_class("shared-reaction-bundle");
        quick.add_css_class("compact");
        let title = if emojis
            .iter()
            .all(|emoji| preferences.favorites.contains(emoji))
        {
            "Favorites"
        } else {
            "Quick Reactions"
        };
        let label = text(title, "picker-section-heading");
        label.set_xalign(0.5);
        label.set_margin_top(8);
        label.set_margin_bottom(6);
        let heading_height = label.measure(gtk::Orientation::Vertical, 300).1;
        let heading = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(350)
            .child(&label)
            .build();
        quick.append(&heading);
        let choices = bx(false, 2);
        choices.set_homogeneous(true);
        choices.set_height_request(40);
        let mut buttons = Vec::new();
        for emoji in &emojis {
            let name = crate::emoji_completion::catalog()
                .iter()
                .find(|entry| entry.text == *emoji)
                .map_or(emoji.as_str(), |entry| entry.name.as_str());
            let button = surface.emoji_button(emoji, name, preferences.favorites.contains(emoji));
            button.add_css_class("reaction-choice");
            button.set_size_request(36, 36);
            button.set_halign(gtk::Align::Center);
            button.set_valign(gtk::Align::Center);
            choices.append(&button);
            buttons.push((emoji.clone(), button));
        }
        for _ in emojis.len()..8 {
            choices.append(&bx(false, 0));
        }
        quick.append(&choices);
        let shared = Rc::new(SharedQuickRow {
            emojis,
            row: choices.clone(),
            buttons,
            height: 40 + heading_height,
            slot: RefCell::new(glib::WeakRef::new()),
            bind: RefCell::new(None),
        });
        surface.quick.replace(Some(shared.clone()));
        surface.refresh();
        let expanded = bx(true, 6);
        expanded.add_css_class("expanded-reactions");
        expanded.set_valign(gtk::Align::Start);
        expanded.set_halign(gtk::Align::Fill);
        expanded.set_visible(true);
        expanded.set_can_target(false);
        expanded.set_opacity(0.);
        expanded.set_sensitive(false);
        expanded.update_state(&[gtk::accessible::State::Hidden(true)]);
        let header = bx(false, 6);
        header.add_css_class("expanded-reaction-header");
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .hexpand(true)
            .build();
        header.append(&search);
        let collapse = icon_button("go-up-symbolic", "Back to quick reactions");
        header.append(&collapse);
        expanded.append(&header);
        surface.bind_search(&search, false);
        expanded.append(&surface.root);
        let base = bx(true, 0);
        base.set_size_request(320, 48);
        let morph = gtk::Overlay::new();
        morph.add_css_class("reaction-morph");
        morph.add_css_class("reaction-radius-24");
        let radius_css = gtk::CssProvider::new();
        radius_css.load_from_string(&ReactionPicker::radius_styles(24));
        morph.style_context().add_provider(
            &radius_css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        morph.set_overflow(gtk::Overflow::Hidden);
        morph.set_child(Some(&base));
        morph.add_overlay(&expanded);
        morph.set_measure_overlay(&expanded, false);
        morph.set_clip_overlay(&expanded, true);
        let canvas = bx(true, 0);
        canvas.set_halign(gtk::Align::Start);
        canvas.set_valign(gtk::Align::Start);
        morph.add_overlay(&canvas);
        morph.set_measure_overlay(&canvas, false);
        morph.set_clip_overlay(&canvas, true);
        canvas.append(&quick);
        quick.set_width_request(308);
        canvas.set_margin_start(6);
        canvas.set_margin_top(4);
        let arrow = icon_button("go-down-symbolic", "Show all reactions");
        arrow.add_css_class("reaction-expand");
        arrow.set_halign(gtk::Align::End);
        arrow.set_valign(gtk::Align::Center);
        arrow.set_size_request(36, 36);
        arrow.set_margin_end(6);
        morph.add_overlay(&arrow);
        morph.set_measure_overlay(&arrow, false);
        let actions = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(350)
            .reveal_child(true)
            .build();
        let root = bx(true, 0);
        root.append(&morph);
        root.append(&actions);
        let picker = Rc::new(ReactionPicker {
            root,
            morph,
            base,
            expanded,
            quick,
            heading,
            canvas,
            arrow,
            actions,
            search,
            shared: shared.clone(),
            surface,
            is_expanded: Cell::new(false),
            moving: Cell::new(false),
            progress: Cell::new(0.),
            full_height: Cell::new(380),
            target: Cell::new((6., 46., 308)),
            collapsed_radius: Cell::new(24),
            radius: Cell::new(24),
            radius_css,
            animation: RefCell::new(None),
        });
        let weak = Rc::downgrade(&picker);
        *shared.bind.borrow_mut() = Some(Box::new(move |slot| {
            if let Some(picker) = weak.upgrade() {
                if picker.is_expanded.get() && !picker.moving.get() {
                    picker.dock(slot);
                }
            }
        }));
        let weak = Rc::downgrade(&picker);
        picker.arrow.connect_clicked(move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.set_expanded(true);
            }
        });
        let weak = Rc::downgrade(&picker);
        collapse.connect_clicked(move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.set_expanded(false);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&picker);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape
                && let Some(picker) = weak.upgrade()
                && picker.is_expanded.get()
            {
                picker.set_expanded(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        picker.root.add_controller(keys);
        let keep = picker.clone();
        popover.connect_closed(move |_| {
            let _ = &keep;
        });
        picker
    }
}
