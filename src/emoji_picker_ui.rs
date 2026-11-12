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

struct EmojiSurface {
    ui: Weak<Ui>,
    root: gtk::Box,
    categories: gtk::Box,
    pack_tabs: gtk::Box,
    settings: gtk::Box,
    group_anchor: RefCell<Option<gtk::ToggleButton>>,
    custom_page: RefCell<Option<Rc<crate::media_library_ui::PackLibraryPage>>>,
    emoji_label: gtk::Label,
    emoji_scroll: gtk::ScrolledWindow,
    model: gio::ListStore,
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
            let grid = gtk::GridView::new(
                Some(gtk::NoSelection::new(Some(model.clone()))),
                Some(factory),
            );
            grid.set_min_columns(columns);
            grid.set_max_columns(columns);
            grid.set_single_click_activate(false);
            grid.add_css_class("emoji-picker-grid");

            let categories = bx(false, 2);
            categories.add_css_class("emoji-category-bar");
            let pack_tabs = bx(false, 2);
            let settings = bx(false, 2);
            let root = bx(true, 6);
            root.add_css_class("emoji-picker-surface");
            let category_scroll = horizontal_strip(&categories);
            category_scroll.add_css_class("emoji-category-scroll");
            let category_row = bx(false, 2);
            category_scroll.set_hexpand(true);
            category_row.append(&category_scroll);
            category_row.append(&settings);
            root.append(&category_row);
            let emoji_label = text("Favorites", "caption dim-label");
            root.append(&emoji_label);
            let scrolling = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .child(&grid)
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
                pack_tabs,
                settings,
                group_anchor: RefCell::new(None),
                custom_page: RefCell::new(None),
                emoji_label,
                emoji_scroll: scrolling,
                model,
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
                    surface.emoji_label.set_visible(true);
                    surface.emoji_scroll.set_visible(true);
                    surface.refresh();
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
                    surface.emoji_label.set_visible(true);
                    surface.emoji_scroll.set_visible(true);
                    if let Some(page) = surface.custom_page.borrow().as_ref().cloned() {
                        page.root.set_visible(false);
                    }
                    surface.refresh();
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
        let category = self.category.get();
        let combined_section = !searching && matches!(category, -1 | -2);
        self.emoji_scroll.set_vexpand(!combined_section);
        self.emoji_scroll
            .set_propagate_natural_height(combined_section);
        self.emoji_scroll
            .set_max_content_height(if combined_section { 96 } else { -1 });
        let entries: Vec<_> = if searching {
            crate::emoji_completion::search(&query)
        } else if category == -1 {
            preferences
                .favorites
                .iter()
                .filter_map(|emoji| {
                    crate::emoji_completion::catalog()
                        .iter()
                        .find(|entry| entry.text == *emoji)
                        .cloned()
                })
                .collect()
        } else if category == -2 {
            preferences
                .recent
                .iter()
                .filter_map(|emoji| {
                    crate::emoji_completion::catalog()
                        .iter()
                        .find(|entry| entry.text == *emoji)
                        .cloned()
                })
                .collect()
        } else if category < -2 {
            Vec::new()
        } else {
            let groups = EMOJI_CATEGORIES[category as usize].groups;
            crate::emoji_completion::catalog()
                .iter()
                .filter(|entry| groups.contains(&entry.group))
                .cloned()
                .collect()
        };
        self.model.remove_all();
        for entry in &entries {
            self.model.append(&glib::BoxedAnyObject::new(entry.clone()));
        }
        self.emoji_label.set_text(if searching {
            "Search Results"
        } else if category == -1 {
            "Favorites"
        } else if category == -2 {
            "Recent"
        } else if category < -2 {
            "Custom Emoji"
        } else {
            EMOJI_CATEGORIES[category as usize].title
        });
        self.emoji_scroll
            .set_visible(searching || matches!(category, -1 | -2) || !entries.is_empty());
    }

    fn emoji_button(self: &Rc<Self>, emoji: &str, name: &str) -> gtk::Button {
        let reorder_key = favorite_order::emoji_key(emoji);
        let button = gtk::Button::builder()
            .label(emoji)
            .tooltip_text(format!("{name} · right-click to favorite or pin"))
            .build();
        button.add_css_class("flat");
        button.add_css_class("emoji-picker-item");
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{name}, {emoji}"
        ))]);
        let value = emoji.to_owned();
        let surface = Rc::downgrade(self);
        let emoji = value.clone();
        button.connect_clicked(move |_| {
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
        if self.category.get() == -1
            && self.query.borrow().trim().is_empty()
            && self.custom_page.borrow().is_none()
        {
            if let Some(ui) = self.ui.upgrade() {
                let surface = Rc::downgrade(self);
                favorite_order::attach(&ui, &button, FavoriteKind::Emoji, reorder_key, move || {
                    let Some(surface) = surface.upgrade().filter(|surface| {
                        surface.category.get() == -1 && surface.query.borrow().trim().is_empty()
                    }) else {
                        return Vec::new();
                    };
                    (0..surface.model.n_items())
                        .filter_map(|index| {
                            surface
                                .model
                                .item(index)
                                .and_downcast::<glib::BoxedAnyObject>()
                                .map(|entry| {
                                    favorite_order::emoji_key(&entry.borrow::<EmojiEntry>().text)
                                })
                        })
                        .collect()
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
    Some(sidebar_surface(ui)?.emoji_button(&entry.text, &entry.name))
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
        let entry = data.borrow::<EmojiEntry>().clone();
        let cell = crate::widgets::SquareCell::new(&surface.emoji_button(&entry.text, &entry.name));
        item.set_child(Some(&cell));
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
    pub root: gtk::Stack,
    actions: RefCell<glib::WeakRef<gtk::Widget>>,
    animation: RefCell<Option<adw::TimedAnimation>>,
    _surface: Rc<EmojiSurface>,
}

impl ReactionPicker {
    pub fn attach_actions(&self, actions: &impl IsA<gtk::Widget>) {
        self.actions.replace(actions.as_ref().downgrade());
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
        let quick = preferences.quick_reactions(&message.reactions);
        let collapsed = bx(false, 0);
        collapsed.add_css_class("message-menu-panel");
        for emoji in quick.into_iter().take(7) {
            let button = gtk::Button::with_label(&emoji);
            button.add_css_class("flat");
            button.add_css_class("reaction-choice");
            let weak = Rc::downgrade(self);
            let popup = popover.downgrade();
            let id = message.id.clone();
            button.connect_clicked(move |_| {
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                    popup.unparent();
                }
                if let Some(ui) = weak.upgrade() {
                    ui.react(&id, &emoji);
                }
            });
            collapsed.append(&button);
        }
        let expand = icon_button("go-down-symbolic", "Show all reactions");
        expand.add_css_class("circular");
        expand.add_css_class("reaction-expand");
        collapsed.append(&expand);

        let expanded = bx(true, 6);
        expanded.add_css_class("expanded-reactions");
        expanded.add_css_class("message-menu-panel");
        expanded.set_width_request(300);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .build();
        expanded.append(&search);
        let popup = popover.downgrade();
        let id = message.id.clone();
        let surface = EmojiSurface::new(self, 8, Some(330), move |ui, emoji| {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
                popup.unparent();
            }
            ui.react(&id, emoji);
        });
        surface.bind_search(&search, false);
        expanded.append(&surface.root);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(180)
            .vhomogeneous(false)
            .hhomogeneous(false)
            .build();
        stack.add_named(&collapsed, Some("quick"));
        stack.add_named(&expanded, Some("all"));
        stack.set_visible_child_name("quick");
        let picker = Rc::new(ReactionPicker {
            root: stack,
            actions: RefCell::new(glib::WeakRef::new()),
            animation: RefCell::new(None),
            _surface: surface,
        });
        let weak = Rc::downgrade(&picker);
        expand.connect_clicked(move |_| {
            let Some(picker) = weak.upgrade() else { return };
            if let Some(actions) = picker.actions.borrow().upgrade() {
                actions.set_visible(false);
            }
            let from = picker.root.allocated_height().max(1);
            let (_, to, _, _) = expanded.measure(gtk::Orientation::Vertical, 300);
            picker.root.set_height_request(from);
            picker.root.set_visible_child_name("all");
            let stack = picker.root.downgrade();
            let animation = adw::TimedAnimation::new(
                &picker.root,
                from as f64,
                to.max(from) as f64,
                220,
                adw::CallbackAnimationTarget::new(move |height| {
                    if let Some(stack) = stack.upgrade() {
                        stack.set_height_request(height.round() as i32);
                    }
                }),
            );
            let stack = picker.root.downgrade();
            animation.connect_done(move |_| {
                if let Some(stack) = stack.upgrade() {
                    stack.set_height_request(-1);
                }
            });
            animation.play();
            picker.animation.replace(Some(animation));
            search.grab_focus();
        });
        let keep_alive = picker.clone();
        popover.connect_closed(move |_| {
            let _ = &keep_alive;
        });
        picker
    }
}
