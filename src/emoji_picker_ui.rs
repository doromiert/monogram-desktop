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

// Track only factory-bound rows; category navigation must not mount the catalog.
pub(crate) struct SectionScroll {
    list: gtk::ListView,
    scroll: gtk::ScrolledWindow,
    rows: RefCell<Vec<glib::WeakRef<gtk::ListItem>>>,
    target: Cell<Option<(u32, u8)>>,
    scheduled: Cell<bool>,
    on_visible: RefCell<Option<Box<dyn Fn(u32)>>>,
}
impl SectionScroll {
    pub(crate) fn new(list: &gtk::ListView, scroll: &gtk::ScrolledWindow) -> Rc<Self> {
        let tracker = Rc::new(Self {
            list: list.clone(),
            scroll: scroll.clone(),
            rows: RefCell::new(Default::default()),
            target: Cell::new(None),
            scheduled: Cell::new(false),
            on_visible: RefCell::new(None),
        });
        for changed in [false, true] {
            let weak = Rc::downgrade(&tracker);
            let callback = move |_: &gtk::Adjustment| {
                if let Some(tracker) = weak.upgrade() {
                    tracker.schedule();
                }
            };
            if changed {
                scroll.vadjustment().connect_changed(callback);
            } else {
                scroll.vadjustment().connect_value_changed(callback);
            }
        }
        tracker
    }
    pub(crate) fn watch(&self, callback: impl Fn(u32) + 'static) {
        self.on_visible.replace(Some(Box::new(callback)));
    }
    pub(crate) fn bind(self: &Rc<Self>, item: &gtk::ListItem) {
        self.rows.borrow_mut().push(item.downgrade());
        self.schedule();
    }
    pub(crate) fn unbind(&self, item: &gtk::ListItem) {
        self.rows
            .borrow_mut()
            .retain(|row| row.upgrade().is_some_and(|row| row != *item));
    }
    pub(crate) fn jump(self: &Rc<Self>, position: u32) {
        self.target.set(Some((position, 0)));
        self.list
            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
        self.schedule();
    }
    fn bounds(&self, widget: &gtk::Widget) -> Option<gtk::graphene::Rect> {
        widget
            .parent()
            .unwrap_or_else(|| widget.clone())
            .compute_bounds(&self.scroll)
    }
    fn schedule(self: &Rc<Self>) {
        if self.scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.list.add_tick_callback(move |_, _| {
            let Some(tracker) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if let Some((position, frames)) = tracker.target.get() {
                let bounds = tracker
                    .rows
                    .borrow()
                    .iter()
                    .filter_map(|row| row.upgrade())
                    .find(|row| row.position() == position)
                    .and_then(|row| row.child())
                    .and_then(|widget| tracker.bounds(&widget));
                if let Some(bounds) = bounds.filter(|_| frames > 0) {
                    let adjustment = tracker.scroll.vadjustment();
                    let before = adjustment.value();
                    adjustment.set_value(before + f64::from(bounds.y()));
                    if frames > 0
                        && ((bounds.y().abs() < 1.) || (adjustment.value() - before).abs() < 0.5)
                    {
                        tracker.target.set(None);
                    }
                }
                if tracker.target.get().is_some() {
                    if frames < 12 {
                        tracker.target.set(Some((position, frames + 1)));
                        return glib::ControlFlow::Continue;
                    }
                    tracker.target.set(None);
                }
            }
            let first = tracker
                .rows
                .borrow()
                .iter()
                .filter_map(|row| {
                    let row = row.upgrade()?;
                    let widget = row.child()?;
                    let bounds = tracker.bounds(&widget)?;
                    (widget.is_mapped()
                        && bounds.y() + bounds.height() > 1.
                        && bounds.y() < tracker.scroll.height() as f32)
                        .then_some(row.position())
                })
                .min();
            tracker.scheduled.set(false);
            if let Some(position) = first
                && let Some(callback) = tracker.on_visible.borrow().as_ref()
            {
                callback(position);
            }
            glib::ControlFlow::Break
        });
    }
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
    category_row: gtk::Overlay,
    quick: RefCell<Option<Rc<SharedQuickRow>>>,
    pack_tabs: gtk::Box,
    settings: gtk::Box,
    _settings_children: gio::ListModel,
    group_anchor: RefCell<Option<gtk::ToggleButton>>,
    custom_page: RefCell<Option<Rc<crate::media_library_ui::PackLibraryPage>>>,
    emoji_label: gtk::Label,
    emoji_scroll: gtk::ScrolledWindow,
    model: gio::ListStore,
    list: gtk::ListView,
    columns: usize,
    anchors: RefCell<std::collections::BTreeMap<i32, u32>>,
    favorite_keys: RefCell<Vec<String>>,
    favorite_preview: favorite_order::Preview,
    category: Cell<i32>,
    category_buttons: RefCell<std::collections::BTreeMap<i32, gtk::ToggleButton>>,
    section_scroll: Rc<SectionScroll>,
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
            settings.add_css_class("emoji-category-settings");
            settings.set_halign(gtk::Align::End);
            settings.set_valign(gtk::Align::Center);
            settings.set_visible(false);
            let root = bx(true, 6);
            root.add_css_class("emoji-picker-surface");
            let category_scroll = horizontal_strip(&categories);
            category_scroll.add_css_class("emoji-category-scroll");
            let category_row = gtk::Overlay::new();
            category_row.add_css_class("emoji-category-row");
            category_scroll.set_hexpand(true);
            category_row.set_child(Some(&category_scroll));
            category_row.add_overlay(&settings);
            let settings_children = settings.observe_children();
            let settings_weak = settings.downgrade();
            let categories_weak = categories.downgrade();
            settings_children.connect_items_changed(move |_, _, _, _| {
                if let (Some(settings), Some(categories)) =
                    (settings_weak.upgrade(), categories_weak.upgrade())
                {
                    let visible = settings.first_child().is_some();
                    settings.set_visible(visible);
                    let width = if visible {
                        settings.measure(gtk::Orientation::Horizontal, -1).1 + 2
                    } else {
                        0
                    };
                    categories.set_margin_end(width);
                }
            });
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
            let section_scroll = SectionScroll::new(&list, &scrolling);
            let favorite_preview = favorite_order::Preview::new(&scrolling);
            root.append(&favorite_preview);
            Self {
                ui: Rc::downgrade(ui),
                root,
                categories,
                category_row,
                quick: RefCell::new(None),
                pack_tabs,
                settings,
                _settings_children: settings_children,
                group_anchor: RefCell::new(None),
                custom_page: RefCell::new(None),
                emoji_label,
                emoji_scroll: scrolling,
                model,
                list,
                columns: columns as usize,
                anchors: RefCell::new(Default::default()),
                favorite_keys: RefCell::new(Vec::new()),
                favorite_preview,
                category: Cell::new(-1),
                category_buttons: RefCell::new(Default::default()),
                section_scroll,
                query: RefCell::new(String::new()),
                activate: Rc::new(activate),
            }
        });
        EMOJI_SURFACES.with(|surfaces| {
            surfaces
                .borrow_mut()
                .push((Rc::downgrade(ui), Rc::downgrade(&surface)));
        });
        let weak = Rc::downgrade(&surface);
        surface.section_scroll.watch(move |position| {
            if let Some(surface) = weak.upgrade() {
                let category = surface
                    .anchors
                    .borrow()
                    .iter()
                    .filter(|(_, start)| **start <= position)
                    .max_by_key(|(_, start)| **start)
                    .map(|(id, _)| *id);
                if let Some(category) = category {
                    surface.select_category(category);
                }
            }
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
            button.connect_clicked(move |button| {
                button.set_active(true);
                if let Some(surface) = surface.upgrade() {
                    surface.category.set(index);
                    if let Some(page) = surface.custom_page.borrow().as_ref().cloned() {
                        page.show_section(i64::from(index));
                        page.root.set_visible(true);
                        return;
                    }
                    surface.jump_category(index);
                }
            });
            self.category_buttons
                .borrow_mut()
                .insert(index, button.clone());
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
            button.connect_clicked(move |button| {
                button.set_active(true);
                if let Some(surface) = surface.upgrade() {
                    surface.category.set(index as i32);
                    if let Some(page) = surface.custom_page.borrow().as_ref().cloned() {
                        page.show_section(-10 - index as i64);
                        return;
                    }
                    surface.jump_category(index as i32);
                }
            });
            self.category_buttons
                .borrow_mut()
                .insert(index as i32, button.clone());
            self.categories.append(&button);
            previous = Some(button);
        }
    }

    fn select_category(&self, category: i32) {
        let category = if category == -4 { -1 } else { category };
        self.category.set(category);
        if let Some(button) = self.category_buttons.borrow().get(&category) {
            button.set_active(true);
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
            .filter_map(|emoji| crate::emoji_completion::catalog_entry(emoji).cloned())
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
                let emoji = crate::emoji_completion::emoji_id(emoji);
                preferences
                    .favorites
                    .iter()
                    .position(|value| crate::emoji_completion::emoji_id(value) == emoji)
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
                    .filter_map(|emoji| crate::emoji_completion::catalog_entry(emoji).cloned())
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
            self.section_scroll.jump(*position);
        }
    }

    fn emoji_button(self: &Rc<Self>, emoji: &str, name: &str, favorite: bool) -> gtk::Button {
        let reorder_key = favorite_order::emoji_key(emoji);
        let display = crate::emoji_completion::catalog_entry(emoji)
            .map(|entry| entry.text.as_str())
            .unwrap_or(emoji);
        let button = gtk::Button::builder()
            .label(display)
            .tooltip_text(format!(
                "{name} · Alt-click or right-click to favorite or pin"
            ))
            .build();
        button.add_css_class("flat");
        button.add_css_class("emoji-picker-item");
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{name}, {display}"
        ))]);
        let value = crate::emoji_completion::canonical(emoji);
        let activation = display.to_owned();
        let alternate_activation = Rc::new(Cell::new(false));
        let surface = Rc::downgrade(self);
        let emoji = activation;
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
            let id = crate::emoji_completion::emoji_id(&emoji);
            let favorite = preferences
                .favorites
                .iter()
                .any(|value| crate::emoji_completion::emoji_id(value) == id);
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
                favorite_order::attach(
                    &ui,
                    &button,
                    &self.favorite_preview,
                    FavoriteKind::Emoji,
                    reorder_key,
                    move || {
                        surface
                            .upgrade()
                            .filter(|surface| surface.query.borrow().trim().is_empty())
                            .map(|surface| surface.favorite_keys.borrow().clone())
                            .unwrap_or_default()
                    },
                );
            }
        }
        button
    }

    fn emoji_options(self: &Rc<Self>, button: &gtk::Button, emoji: &str, x: f64, y: f64) {
        let Some(ui) = self.ui.upgrade() else { return };
        let emoji = crate::emoji_completion::canonical(emoji);
        let id = crate::emoji_completion::emoji_id(&emoji);
        let preferences = ui.store.emoji_preferences().unwrap_or_default();
        let favorite = preferences
            .favorites
            .iter()
            .any(|value| crate::emoji_completion::emoji_id(value) == id);
        let pinned = preferences
            .pinned
            .iter()
            .any(|value| crate::emoji_completion::emoji_id(value) == id);
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
            let emoji = emoji.clone();
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

pub(crate) fn sync_emoji_section(ui: &Rc<Ui>, id: i64) {
    if let Some(surface) = sidebar_surface(ui) {
        if id == -1 || id == -2 {
            surface.select_category(id as i32);
        } else if id <= -10 {
            surface.select_category((-10 - id) as i32);
        }
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
                    let cell = crate::widgets::SquareCell::new(&surface.emoji_button(
                        &entry.text,
                        &entry.name,
                        *favorite,
                    ));
                    if *favorite {
                        surface
                            .favorite_preview
                            .register(favorite_order::emoji_key(&entry.text), &cell);
                    }
                    row.append(&cell);
                }
                for _ in entries.len()..surface.columns {
                    row.append(&bx(false, 0));
                }
                row.upcast()
            }
        };
        item.set_child(Some(&child));
        surface.section_scroll.bind(item);
    });
    let weak = surface.clone();
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(surface) = weak.upgrade() {
            surface.section_scroll.unbind(item);
        }
        item.set_child(gtk::Widget::NONE);
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

mod reaction_clip_imp {
    use super::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct ReactionClip {
        pub child: RefCell<Option<gtk::Widget>>,
        pub radius: Cell<f32>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for ReactionClip {
        const NAME: &'static str = "MonogramReactionClip";
        type Type = super::ReactionClip;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for ReactionClip {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for ReactionClip {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child
                .borrow()
                .as_ref()
                .map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
        }
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if let Some(child) = self.child.borrow().as_ref() {
                let bounds =
                    gtk::graphene::Rect::new(0., 0., obj.width() as f32, obj.height() as f32);
                snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(
                    bounds,
                    self.radius.get(),
                ));
                obj.snapshot_child(child, snapshot);
                snapshot.pop();
            }
        }
    }
}
glib::wrapper! {
    pub struct ReactionClip(ObjectSubclass<reaction_clip_imp::ReactionClip>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
impl ReactionClip {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        use gtk::subclass::prelude::*;
        let clip: Self = glib::Object::new();
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(&clip);
        clip.imp().child.replace(Some(child));
        clip.imp().radius.set(24.);
        clip
    }
    pub(crate) fn radius(&self) -> f32 {
        use gtk::subclass::prelude::*;
        self.imp().radius.get()
    }
    fn set_radius(&self, radius: f32) {
        use gtk::subclass::prelude::*;
        if self.radius() != radius {
            self.imp().radius.set(radius);
            // Drawing the clip avoids restyling every virtual emoji on each frame.
            self.queue_draw();
        }
    }
}

pub(crate) struct ReactionPicker {
    pub root: gtk::Box,
    morph: gtk::Overlay,
    clip: ReactionClip,
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
    fn capture_collapsed_radius(&self) {
        if self.progress.get() != 0. || self.moving.get() || self.is_expanded.get() {
            return;
        }
        let height = self.morph.height();
        if height <= 0 {
            return;
        }
        self.collapsed_radius.set((height / 2).max(12));
    }
    fn apply_radius(&self, progress: f64) {
        let collapsed = self.collapsed_radius.get() as f64;
        self.clip
            .set_radius((collapsed + (12. - collapsed) * progress) as f32);
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
            self.expanded.set_can_focus(true);
            self.expanded
                .update_state(&[gtk::accessible::State::Hidden(false)]);
        }
        let reparent = self.quick.parent().as_ref() != Some(self.canvas.upcast_ref());
        if reparent {
            self.detach_quick();
        }
        self.quick.add_css_class("compact");
        self.canvas.set_visible(true);
        if reparent {
            self.canvas.append(&self.quick);
        }
        // Keep both input widgets alive under a stationary pointer.
        if let Some(image) = self.arrow.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if expanded {
                "go-up-symbolic"
            } else {
                "go-down-symbolic"
            }));
        }
        let title = if expanded {
            "Back to quick reactions"
        } else {
            "Show all reactions"
        };
        self.arrow.set_tooltip_text(Some(title));
        self.arrow
            .update_property(&[gtk::accessible::Property::Label(title)]);
        self.heading.set_reveal_child(expanded);
        self.actions.set_reveal_child(!expanded);
        self.full_height.set(
            self.expanded
                .measure(gtk::Orientation::Vertical, 320)
                .1
                .max(40),
        );
        self.frame(self.progress.get());
        if reparent && self.morph.width() > 0 && self.morph.height() > 0 {
            // Reparenting clears allocation; restore it before a popup paint.
            let (width, height) = (self.morph.width(), self.morph.height());
            self.morph.measure(gtk::Orientation::Horizontal, -1);
            self.morph.measure(gtk::Orientation::Vertical, width);
            self.morph.allocate(width, height, -1, None);
        }
        let duration = (200. * (self.progress.get() - if expanded { 1. } else { 0. }).abs())
            .round()
            .max(1.) as u32;
        let weak = Rc::downgrade(self);
        let animation = adw::TimedAnimation::new(
            &self.morph,
            self.progress.get(),
            if expanded { 1. } else { 0. },
            duration,
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
                    picker.expanded.set_can_focus(false);
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
        let title = if emojis.iter().all(|emoji| {
            let emoji = crate::emoji_completion::emoji_id(emoji);
            preferences
                .favorites
                .iter()
                .any(|value| crate::emoji_completion::emoji_id(value) == emoji)
        }) {
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
            .transition_duration(200)
            .child(&label)
            .build();
        quick.append(&heading);
        let choices = bx(false, 2);
        choices.set_homogeneous(true);
        choices.set_height_request(40);
        let mut buttons = Vec::new();
        for emoji in &emojis {
            let name = crate::emoji_completion::catalog_entry(emoji)
                .map_or(emoji.as_str(), |entry| entry.name.as_str());
            let key = crate::emoji_completion::emoji_id(emoji);
            let favorite = preferences
                .favorites
                .iter()
                .any(|value| crate::emoji_completion::emoji_id(value) == key);
            let button = surface.emoji_button(emoji, name, favorite);
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
        expanded.set_can_focus(false);
        expanded.update_state(&[gtk::accessible::State::Hidden(true)]);
        let header = bx(false, 6);
        header.add_css_class("expanded-reaction-header");
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .hexpand(true)
            .build();
        header.append(&search);
        let toggle_space = bx(false, 0);
        toggle_space.set_width_request(36);
        header.append(&toggle_space);
        expanded.append(&header);
        surface.bind_search(&search, false);
        expanded.append(&surface.root);
        let base = bx(true, 0);
        base.set_size_request(320, 48);
        let morph = gtk::Overlay::new();
        morph.add_css_class("reaction-morph");
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
        arrow.set_valign(gtk::Align::Start);
        arrow.set_margin_top(6);
        arrow.set_size_request(36, 36);
        arrow.set_margin_end(6);
        morph.add_overlay(&arrow);
        morph.set_measure_overlay(&arrow, false);
        let actions = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(200)
            .reveal_child(true)
            .build();
        let root = bx(true, 0);
        let clip = ReactionClip::new(&morph);
        root.append(&clip);
        root.append(&actions);
        let picker = Rc::new(ReactionPicker {
            root,
            morph,
            clip,
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
                picker.set_expanded(!picker.is_expanded.get());
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

#[cfg(test)]
mod native_regressions {
    use super::*;
    use std::time::{Duration, Instant};

    fn descendants(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
        let mut pending = vec![widget.clone().upcast::<gtk::Widget>()];
        let mut result = Vec::new();
        while let Some(widget) = pending.pop() {
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                pending.push(next);
            }
            result.push(widget);
        }
        result
    }

    #[test]
    #[ignore = "requires isolated native 60Hz X11 display, xdotool, data roots and Secret Service"]
    fn reaction_motion_keeps_frame_budget_and_text_selection() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramReactionMotionTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let css = gtk::CssProvider::new();
        css.load_from_path(asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut state = crate::model::State::live("", vec![]);
        let chat = state.chat_for("recipient@fixture.invalid");
        chat.messages.push(Message {
            id: "motion".into(),
            text: "Selectable message text".into(),
            kind: "text".into(),
            ..Default::default()
        });
        state.active = chat.id.clone();
        ui.state.replace(state);
        let bubble = bx(true, 0);
        bubble.set_valign(gtk::Align::Center);
        bubble.set_halign(gtk::Align::Center);
        let label = gtk::Label::new(Some("Selectable message text"));
        label.set_selectable(true);
        bubble.append(&label);
        ui.window.set_content(Some(&bubble));
        ui.window.set_default_size(1000, 1000);
        ui.window.present();
        glib::MainContext::default().block_on(async {
            glib::timeout_future(Duration::from_millis(100)).await;
            label.select_region(0, 0);
            ui.message_menu(&bubble, "motion", 12., 12.);
            glib::timeout_future(Duration::from_millis(70)).await;
            let morph = descendants(&bubble)
                .into_iter()
                .find(|w| w.has_css_class("reaction-morph"))
                .unwrap();
            let button = |title: &str| {
                descendants(&morph)
                    .into_iter()
                    .find_map(|w| {
                        w.downcast::<gtk::Button>()
                            .ok()
                            .filter(|b| b.tooltip_text().as_deref() == Some(title))
                    })
                    .unwrap()
            };
            let expand = button("Show all reactions");
            let collapse = expand.clone();
            let frames = Rc::new(RefCell::new(Vec::new()));
            let recorded = frames.clone();
            let moving = morph.clone();
            let quick = descendants(&morph)
                .into_iter()
                .find(|widget| widget.has_css_class("shared-reaction-bundle"))
                .unwrap();
            let arrow = expand.clone();
            let arrow_icon = arrow.child().unwrap();
            let arrow_bounds = arrow.compute_bounds(&morph).unwrap();
            let tick = ui.window.add_tick_callback(move |_, _| {
                assert!(
                    quick.width() > 0 && quick.height() > 0,
                    "quick reactions lost allocation during handoff"
                );
                assert!(arrow.is_mapped(), "toggle disappeared during animation");
                assert_eq!(
                    arrow.child().as_ref(),
                    Some(&arrow_icon),
                    "toggle icon widget was replaced"
                );
                assert_eq!(
                    arrow.compute_bounds(&moving).unwrap(),
                    arrow_bounds,
                    "toggle moved during animation"
                );
                recorded
                    .borrow_mut()
                    .push((Instant::now(), moving.height()));
                glib::ControlFlow::Continue
            });
            for _ in 0..3 {
                frames.borrow_mut().clear();
                let start = Instant::now();
                expand.emit_clicked();
                glib::timeout_future(Duration::from_millis(270)).await;
                let mut gaps = frames
                    .borrow()
                    .windows(2)
                    .filter(|pair| {
                        pair[0].0.duration_since(start) >= Duration::from_millis(60)
                            && pair[1].0.duration_since(start) <= Duration::from_millis(210)
                    })
                    .map(|pair| pair[1].0.duration_since(pair[0].0))
                    .collect::<Vec<_>>();
                gaps.sort();
                assert!(gaps.len() >= 4, "too few delivered animation frames");
                assert!(
                    gaps[gaps.len() / 2] < Duration::from_millis(25),
                    "animation median frame gap {:?}",
                    gaps[gaps.len() / 2]
                );
                collapse.emit_clicked();
                glib::timeout_future(Duration::from_millis(230)).await;
            }
            tick.remove();
            let output = std::process::Command::new("xdotool")
                .args([
                    "search",
                    "--all",
                    "--onlyvisible",
                    "--pid",
                    &std::process::id().to_string(),
                    "--name",
                    "^Monogram$",
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            let window_id = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .last()
                .unwrap()
                .to_owned();
            let bounds = expand.compute_bounds(&ui.window).unwrap();
            let x = (bounds.x() + bounds.width() / 2.).round().to_string();
            let y = (bounds.y() + bounds.height() / 2.).round().to_string();
            gio::spawn_blocking(move || {
                assert!(
                    std::process::Command::new("xdotool")
                        .args([
                            "mousemove",
                            "--sync",
                            "--window",
                            &window_id,
                            &x,
                            &y,
                            "click",
                            "1"
                        ])
                        .status()
                        .unwrap()
                        .success()
                );
            })
            .await
            .unwrap();
            glib::timeout_future(Duration::from_millis(270)).await;
            assert!(morph.height() > 200);
            gio::spawn_blocking(|| {
                assert!(
                    std::process::Command::new("xdotool")
                        .args(["click", "1"])
                        .status()
                        .unwrap()
                        .success()
                );
            })
            .await
            .unwrap();
            glib::timeout_future(Duration::from_millis(230)).await;
            assert_eq!(
                morph.height(),
                48,
                "stationary pointer click did not collapse"
            );
            let pop = morph
                .ancestor(gtk::Popover::static_type())
                .unwrap()
                .downcast::<gtk::Popover>()
                .unwrap();
            for selection in [(0, 0), (2, 9)] {
                label.select_region(selection.0, selection.1);
                // Reopen so the saved range comes from before the modal menu.
                pop.popdown();
                glib::timeout_future(Duration::from_millis(30)).await;
                label.select_region(selection.0, selection.1);
                ui.message_menu(&bubble, "motion", 12., 12.);
                glib::timeout_future(Duration::from_millis(50)).await;
                let expand = descendants(&bubble)
                    .into_iter()
                    .find_map(|w| {
                        w.downcast::<gtk::Button>()
                            .ok()
                            .filter(|b| b.tooltip_text().as_deref() == Some("Show all reactions"))
                    })
                    .unwrap();
                expand.emit_clicked();
                glib::timeout_future(Duration::from_millis(230)).await;
                let pop = expand
                    .ancestor(gtk::Popover::static_type())
                    .unwrap()
                    .downcast::<gtk::Popover>()
                    .unwrap();
                pop.popdown();
                let expected = (selection.0 != selection.1).then_some(selection);
                let painted = Rc::new(Cell::new(false));
                let observed = painted.clone();
                let painted_label = label.clone();
                ui.window.add_tick_callback(move |_, _| {
                    assert_eq!(
                        painted_label.selection_bounds(),
                        expected,
                        "select-all flashed in a frame"
                    );
                    observed.set(true);
                    glib::ControlFlow::Break
                });
                ui.window.queue_draw();
                glib::timeout_future(Duration::from_millis(50)).await;
                assert!(painted.get());
                assert_eq!(label.selection_bounds(), expected);
            }
        });
        ui.window.close();
    }
}
