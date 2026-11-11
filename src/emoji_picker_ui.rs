//! Native reaction consent and shared emoji-picker presentation.
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
        icon: "😀",
        title: "Smileys and People",
        groups: &[0, 1, 2],
    },
    EmojiCategory {
        icon: "🐻",
        title: "Animals and Nature",
        groups: &[3],
    },
    EmojiCategory {
        icon: "🍔",
        title: "Food and Drink",
        groups: &[4],
    },
    EmojiCategory {
        icon: "🚗",
        title: "Travel and Places",
        groups: &[5],
    },
    EmojiCategory {
        icon: "⚽",
        title: "Activities",
        groups: &[6],
    },
    EmojiCategory {
        icon: "💡",
        title: "Objects",
        groups: &[7],
    },
    EmojiCategory {
        icon: "🔣",
        title: "Symbols",
        groups: &[8],
    },
    EmojiCategory {
        icon: "🏳️",
        title: "Flags",
        groups: &[9],
    },
];

struct EmojiSurface {
    ui: Weak<Ui>,
    root: gtk::Box,
    categories: gtk::Box,
    favorites_label: gtk::Label,
    favorites: gtk::ScrolledWindow,
    favorites_model: gio::ListStore,
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
            let favorites_model = gio::ListStore::new::<glib::BoxedAnyObject>();
            let factory = emoji_factory(surface);
            let grid = gtk::GridView::new(
                Some(gtk::NoSelection::new(Some(model.clone()))),
                Some(factory),
            );
            grid.set_min_columns(columns);
            grid.set_max_columns(columns);
            grid.set_single_click_activate(false);
            grid.add_css_class("emoji-picker-grid");

            let favorites_label = text("Favorites", "caption dim-label");
            let favorites_grid = gtk::GridView::new(
                Some(gtk::NoSelection::new(Some(favorites_model.clone()))),
                Some(emoji_factory(surface)),
            );
            favorites_grid.set_min_columns(columns);
            favorites_grid.set_max_columns(columns);
            favorites_grid.set_single_click_activate(false);
            favorites_grid.add_css_class("emoji-picker-grid");
            favorites_grid.add_css_class("emoji-favorites-grid");
            let favorites = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .min_content_height(36)
                .max_content_height(108)
                .propagate_natural_height(true)
                .child(&favorites_grid)
                .build();
            let categories = bx(false, 2);
            categories.add_css_class("emoji-category-bar");
            let root = bx(true, 6);
            root.add_css_class("emoji-picker-surface");
            let category_scroll = horizontal_strip(&categories);
            category_scroll.add_css_class("emoji-category-scroll");
            root.append(&category_scroll);
            root.append(&favorites_label);
            root.append(&favorites);
            let emoji_label = text("Smileys and People", "caption dim-label");
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
                favorites_label,
                favorites,
                favorites_model,
                emoji_label,
                emoji_scroll: scrolling,
                model,
                category: Cell::new(0),
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
        for (index, icon, title) in std::iter::once((-1, "★", "Favorites")).chain(
            EMOJI_CATEGORIES
                .iter()
                .enumerate()
                .map(|(index, category)| (index as i32, category.icon, category.title)),
        ) {
            let button = gtk::ToggleButton::builder()
                .label(icon)
                .tooltip_text(title)
                .build();
            button.add_css_class("flat");
            button.add_css_class("emoji-category");
            button.update_property(&[gtk::accessible::Property::Label(title)]);
            if let Some(previous) = previous.as_ref() {
                button.set_group(Some(previous));
            }
            button.set_active(index == 0);
            let surface = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(surface) = surface.upgrade()
                {
                    surface.category.set(index);
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
        self.categories.set_sensitive(query.trim().is_empty());
        self.refresh();
    }

    fn refresh(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let preferences = ui.store.emoji_preferences().unwrap_or_default();
        let query = self.query.borrow().clone();
        let searching = !query.trim().is_empty();
        let category = self.category.get();
        let entries: Vec<_> = if searching {
            crate::emoji_completion::search(&query)
        } else if category < 0 {
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
        self.favorites_model.remove_all();
        let visible: std::collections::HashSet<_> =
            entries.iter().map(|entry| entry.text.as_str()).collect();
        let mut favorite_count = 0;
        for emoji in &preferences.favorites {
            if !query.trim().is_empty() && !visible.contains(emoji.as_str()) {
                continue;
            }
            let Some(entry) = crate::emoji_completion::catalog()
                .iter()
                .find(|entry| entry.text == *emoji)
                .cloned()
            else {
                continue;
            };
            self.favorites_model
                .append(&glib::BoxedAnyObject::new(entry));
            favorite_count += 1;
        }
        let show_favorites = category >= 0 && favorite_count > 0;
        self.favorites_label.set_visible(show_favorites);
        self.favorites.set_visible(show_favorites);
        self.emoji_label.set_text(if searching {
            "Search Results"
        } else if category < 0 {
            "Favorites"
        } else {
            EMOJI_CATEGORIES[category as usize].title
        });
        self.emoji_scroll.set_visible(!entries.is_empty());
    }

    fn emoji_button(self: &Rc<Self>, emoji: &str, name: &str) -> gtk::Button {
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
        button
    }

    fn emoji_options(self: &Rc<Self>, button: &gtk::Button, emoji: &str, x: f64, y: f64) {
        let Some(ui) = self.ui.upgrade() else { return };
        let preferences = ui.store.emoji_preferences().unwrap_or_default();
        let favorite = preferences.favorites.iter().any(|value| value == emoji);
        let pinned = preferences.pinned.iter().any(|value| value == emoji);
        let popover = gtk::Popover::builder().has_arrow(false).build();
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
        item.set_child(Some(&surface.emoji_button(&entry.text, &entry.name)));
    });
    factory.connect_unbind(|_, item| {
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(gtk::Widget::NONE);
    });
    factory
}

fn refresh_emoji_surfaces(ui: &Rc<Ui>) {
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
        let surface = EmojiSurface::new(self, 6, None, |ui, emoji| {
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
        let preferences = self.store.emoji_preferences().unwrap_or_default();
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
