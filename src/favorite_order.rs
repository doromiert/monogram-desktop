//! Local favorite ordering; a drag changes order, never membership or recents.
use super::Ui;
use adw::prelude::*;
use gtk::{gdk, glib};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Kind {
    Emoji,
    Stickers,
    Gifs,
}
impl Kind {
    fn preference(self) -> &'static str {
        match self {
            Self::Emoji => "picker:favorites:emoji",
            Self::Stickers => "picker:favorites:stickers",
            Self::Gifs => "picker:favorites:gifs",
        }
    }
}
#[derive(Default)]
pub(crate) struct Order {
    keys: Vec<String>,
    ranks: HashMap<String, usize>,
}
impl Order {
    fn new(keys: Vec<String>) -> Self {
        let ranks = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (key.clone(), i))
            .collect();
        Self { keys, ranks }
    }
    pub(crate) fn sort<T>(&self, items: &mut [T], key: impl Fn(&T) -> String) {
        items.sort_by_cached_key(|item| self.ranks.get(&key(item)).copied().unwrap_or(usize::MAX));
    }
}
pub(crate) fn emoji_key(emoji: &str) -> String {
    format!(
        "emoji:{}",
        crate::message_actions::canonical_emoji(emoji).unwrap_or_else(|| emoji.into())
    )
}
pub(crate) fn pack_key(owner: &str, id: i64) -> String {
    format!("pack:{owner}:{id}")
}
pub(crate) fn gif_key(owner: &str, id: &str) -> String {
    format!("gif:{owner}\n{id}")
}
fn read(db: &Connection, kind: Kind) -> rusqlite::Result<Order> {
    let saved: Option<String> = db
        .query_row(
            "SELECT value FROM preferences WHERE key=?",
            [kind.preference()],
            |row| row.get(0),
        )
        .optional()?;
    let keys = match saved {
        None => Vec::new(),
        Some(value) => {
            if value.len() > 4 * 1024 * 1024 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            serde_json::from_str::<Vec<String>>(&value)
                .map_err(|_| rusqlite::Error::InvalidQuery)?
        }
    };
    let mut seen = HashSet::new();
    Ok(Order::new(
        keys.into_iter()
            .filter(|key| !key.is_empty() && key.len() <= 16384 && seen.insert(key.clone()))
            .collect(),
    ))
}
fn load(db: &Connection) -> rusqlite::Result<HashMap<Kind, Order>> {
    [Kind::Emoji, Kind::Stickers, Kind::Gifs]
        .into_iter()
        .map(|kind| read(db, kind).map(|order| (kind, order)))
        .collect()
}
fn save_move(
    db: &Connection,
    kind: Kind,
    mut actual: Vec<String>,
    source: &str,
    target: &str,
) -> rusqlite::Result<Order> {
    let transaction = db.unchecked_transaction()?;
    let old = read(&transaction, kind)?;
    let mut unique = HashSet::new();
    actual.retain(|key| key.len() <= 16384 && unique.insert(key.clone()));
    old.sort(&mut actual, Clone::clone);
    let from = actual
        .iter()
        .position(|key| key == source)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let to = actual
        .iter()
        .position(|key| key == target)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let moved = actual.remove(from);
    actual.insert(to, moved);
    // Keep slots belonging to other accounts; their favorites are absent from this view.
    let mut ordered = actual.iter();
    let mut keys: Vec<_> = old
        .keys
        .into_iter()
        .map(|key| {
            if unique.contains(&key) {
                ordered.next().unwrap().clone()
            } else {
                key
            }
        })
        .collect();
    keys.extend(ordered.cloned());
    let value = serde_json::to_string(&keys).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if value.len() > 4 * 1024 * 1024 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    transaction.execute("INSERT INTO preferences(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![kind.preference(),value])?;
    transaction.commit()?;
    Ok(Order::new(keys))
}
impl Ui {
    pub(crate) fn load_favorite_orders(self: &Rc<Self>) {
        self.favorite_orders.borrow_mut().clear();
        self.favorite_reorder_pending.set(false);
        let generation = self.generation.get();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let result = ui.store.read_snapshot(|store| load(&store.db)).await;
            if ui.generation.get() != generation {
                return;
            }
            match result {
                Ok(orders) => {
                    ui.favorite_orders.replace(orders);
                    ui.refresh_favorite_views();
                }
                Err(_) => {
                    ui.notice("Favorite order could not be loaded. Reopen the account to retry.")
                }
            }
        });
    }
    fn move_favorite(
        self: &Rc<Self>,
        kind: Kind,
        keys: Vec<String>,
        source: String,
        target: String,
    ) -> bool {
        if source == target {
            return true;
        }
        if !self.favorite_orders.borrow().contains_key(&kind) {
            self.notice("Loading favorite order…");
            return false;
        }
        if !keys.contains(&source)
            || !keys.contains(&target)
            || self.favorite_reorder_pending.replace(true)
        {
            return false;
        }
        let generation = self.generation.get();
        let ui = self.clone();
        let feedback = adw::Toast::new("Saving favorite order…");
        feedback.set_timeout(0);
        self.toast.add_toast(feedback.clone());
        glib::spawn_future_local(async move {
            let result = ui
                .store
                .write_snapshot(move |store| save_move(&store.db, kind, keys, &source, &target))
                .await;
            feedback.dismiss();
            if ui.generation.get() != generation {
                return;
            }
            ui.favorite_reorder_pending.set(false);
            match result {
                Ok(order) => {
                    ui.favorite_orders.borrow_mut().insert(kind, order);
                    ui.refresh_favorite_views();
                }
                Err(_) => ui.notice("Favorite order could not be saved. Your favorites are kept."),
            }
        });
        true
    }
}

struct Drag {
    owner: String,
    generation: u64,
    kind: Kind,
    key: String,
}
pub(crate) fn attach(
    ui: &Rc<Ui>,
    widget: &impl IsA<gtk::Widget>,
    kind: Kind,
    key: String,
    keys: impl Fn() -> Vec<String> + 'static,
) {
    let widget: &gtk::Widget = widget.as_ref();
    let owner = ui.state.borrow().account.clone();
    let generation = ui.generation.get();
    widget.update_property(&[gtk::accessible::Property::Description(
        "Drag or use Alt+Arrow keys to reorder favorites",
    )]);
    let keys = Rc::new(keys);
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    let weak = Rc::downgrade(ui);
    let drag_key = key.clone();
    let current = keys.clone();
    let source_owner = owner.clone();
    source.connect_prepare(move |source, _, _| {
        let ui = weak.upgrade()?;
        if ui.generation.get() != generation || ui.state.borrow().account != source_owner {
            source.set_state(gtk::EventSequenceState::Claimed);
            return None;
        }
        if !ui.favorite_orders.borrow().contains_key(&kind) {
            source.set_state(gtk::EventSequenceState::Claimed);
            ui.notice("Loading favorite order…");
            return None;
        }
        if ui.favorite_reorder_pending.get() || !current().contains(&drag_key) {
            source.set_state(gtk::EventSequenceState::Claimed);
            return None;
        }
        Some(gdk::ContentProvider::for_value(
            &glib::BoxedAnyObject::new(Drag {
                owner: ui.state.borrow().account.clone(),
                generation: ui.generation.get(),
                kind,
                key: drag_key.clone(),
            })
            .to_value(),
        ))
    });
    let target_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    source.connect_drag_begin(move |_, _| {
        if let Some(widget) = target_widget.upgrade() {
            let host = widget
                .parent()
                .filter(|parent| parent.has_css_class("gif-library-tile"))
                .unwrap_or(widget);
            host.add_css_class("favorite-dragging");
        }
    });
    let target_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    source.connect_drag_end(move |_, _, _| {
        if let Some(widget) = target_widget.upgrade() {
            let host = widget
                .parent()
                .filter(|parent| parent.has_css_class("gif-library-tile"))
                .unwrap_or(widget);
            host.remove_css_class("favorite-dragging");
            if let Some(parent) = host.parent() {
                parent.queue_allocate();
            }
        }
    });
    widget.add_controller(source);
    let keyboard = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    let current = keys.clone();
    let keyboard_key = key.clone();
    let keyboard_owner = owner.clone();
    keyboard.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let offset = match key {
            gdk::Key::Left | gdk::Key::Up => -1isize,
            gdk::Key::Right | gdk::Key::Down => 1,
            _ => return glib::Propagation::Proceed,
        };
        if let Some(ui) = weak.upgrade().filter(|ui| {
            ui.generation.get() == generation && ui.state.borrow().account == keyboard_owner
        }) {
            let keys = current();
            if let Some(index) = keys.iter().position(|key| key == &keyboard_key) {
                if let Some(target) = index
                    .checked_add_signed(offset)
                    .and_then(|index| keys.get(index))
                    .cloned()
                {
                    ui.move_favorite(kind, keys, keyboard_key.clone(), target);
                }
            }
        }
        glib::Propagation::Stop
    });
    widget.add_controller(keyboard);
    let target = gtk::DropTarget::new(glib::BoxedAnyObject::static_type(), gdk::DragAction::MOVE);
    let weak_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    target.connect_enter(move |_, _, _| {
        if let Some(widget) = weak_widget.upgrade() {
            widget.add_css_class("favorite-drop-target");
        }
        gdk::DragAction::MOVE
    });
    let weak_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    target.connect_leave(move |_| {
        if let Some(widget) = weak_widget.upgrade() {
            widget.remove_css_class("favorite-drop-target");
        }
    });
    let weak = Rc::downgrade(ui);
    let weak_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    target.connect_drop(move |_, value, _, _| {
        if let Some(widget) = weak_widget.upgrade() {
            widget.remove_css_class("favorite-drop-target");
        }
        let Ok(value) = value.get::<glib::BoxedAnyObject>() else {
            return false;
        };
        let Ok(drag) = value.try_borrow::<Drag>() else {
            return false;
        };
        let Some(ui) = weak.upgrade() else {
            return false;
        };
        if drag.kind != kind
            || generation != ui.generation.get()
            || owner != ui.state.borrow().account
            || drag.generation != ui.generation.get()
            || drag.owner != ui.state.borrow().account
        {
            return false;
        }
        ui.move_favorite(kind, keys(), drag.key.clone(), key.clone())
    });
    widget.add_controller(target);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order_moves_preserve_hidden_accounts_and_never_change_membership() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE preferences(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
        let items = vec![
            "emoji:❤️".into(),
            "pack:first:1".into(),
            "pack:other:2".into(),
            "emoji:😅".into(),
        ];
        let first = save_move(&db, Kind::Emoji, items.clone(), &items[0], &items[3]).unwrap();
        assert_eq!(
            first.keys,
            ["pack:first:1", "pack:other:2", "emoji:😅", "emoji:❤️"]
        );
        let second = save_move(
            &db,
            Kind::Emoji,
            vec![items[0].clone(), items[1].clone(), items[3].clone()],
            &items[0],
            &items[1],
        )
        .unwrap();
        assert_eq!(
            second.keys,
            ["emoji:❤️", "pack:other:2", "pack:first:1", "emoji:😅"]
        );
        assert!(save_move(&db, Kind::Emoji, items.clone(), "not-a-favorite", &items[0]).is_err());
        assert_eq!(read(&db, Kind::Emoji).unwrap().keys, second.keys);
        assert!(read(&db, Kind::Stickers).unwrap().keys.is_empty());
    }
}
