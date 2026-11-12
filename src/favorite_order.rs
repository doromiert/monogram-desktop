//! Local favorite ordering; a drag changes order, never membership or recents.
use super::Ui;
use adw::prelude::*;
use gtk::{gdk, glib, graphene, subclass::prelude::*};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    cell::{Cell, RefCell},
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

struct PreviewItem {
    key: String,
    widget: glib::WeakRef<gtk::Widget>,
    node: gtk::gsk::RenderNode,
    from: graphene::Rect,
    draw: graphene::Rect,
    opacity: f64,
}
struct PreviewDrag {
    source: String,
    hotspot: graphene::Point,
    items: Vec<PreviewItem>,
}
type PreviewLayout = Box<dyn Fn(&[String], &Preview) -> Option<Vec<graphene::Rect>>>;
mod preview_imp {
    use super::*;
    #[derive(Default)]
    pub struct Preview {
        pub(super) child: RefCell<Option<gtk::Widget>>,
        pub(super) cells: RefCell<HashMap<String, glib::WeakRef<gtk::Widget>>>,
        pub(super) armed: RefCell<Option<(String, graphene::Point)>>,
        pub(super) drag: RefCell<Option<PreviewDrag>>,
        pub(super) layout: RefCell<Option<PreviewLayout>>,
        pub(super) cancelled: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Preview {
        const NAME: &'static str = "MonogramFavoritePreview";
        type Type = super::Preview;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Preview {
        fn dispose(&self) {
            self.obj().finish();
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for Preview {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child
                .borrow()
                .as_ref()
                .map_or(gtk::SizeRequestMode::ConstantSize, |child| {
                    child.request_mode()
                })
        }
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
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
            let drag = self.drag.borrow();
            let Some(drag) = drag.as_ref() else { return };
            for item in drag
                .items
                .iter()
                .filter(|item| item.key != drag.source)
                .chain(drag.items.iter().filter(|item| item.key == drag.source))
            {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(item.draw.x(), item.draw.y()));
                snapshot.scale(
                    item.draw.width() / item.from.width().max(1.),
                    item.draw.height() / item.from.height().max(1.),
                );
                snapshot.append_node(&item.node);
                snapshot.restore();
            }
        }
    }
}
glib::wrapper! {
    pub struct Preview(ObjectSubclass<preview_imp::Preview>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
impl Preview {
    pub(crate) fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let preview: Self = glib::Object::new();
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(&preview);
        preview.imp().child.replace(Some(child));
        preview.set_hexpand(true);
        preview.set_vexpand(true);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = preview.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape
                && let Some(preview) = weak.upgrade()
                && preview.imp().drag.borrow().is_some()
            {
                preview.cancel();
            }
            glib::Propagation::Proceed
        });
        preview.add_controller(keys);
        let weak = preview.downgrade();
        preview.connect_unmap(move |_| {
            if let Some(preview) = weak.upgrade() {
                preview.finish();
            }
        });
        preview
    }
    pub(crate) fn register(&self, key: String, widget: &impl IsA<gtk::Widget>) {
        self.imp()
            .cells
            .borrow_mut()
            .insert(key, widget.clone().upcast::<gtk::Widget>().downgrade());
    }
    pub(crate) fn set_layout(
        &self,
        layout: impl Fn(&[String], &Preview) -> Option<Vec<graphene::Rect>> + 'static,
    ) {
        self.imp().layout.replace(Some(Box::new(layout)));
    }
    fn arm(&self, key: &str, widget: &gtk::Widget, x: f64, y: f64) {
        let point = widget
            .compute_point(self, &graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| graphene::Point::new(x as f32, y as f32));
        self.imp().armed.replace(Some((key.to_owned(), point)));
    }
    fn begin(&self, key: &str, keys: &[String]) {
        let armed = self.imp().armed.take();
        self.finish();
        let Some((armed, pointer)) = armed.filter(|(armed, _)| armed == key) else {
            return;
        };
        let mut items = Vec::new();
        let cells = self.imp().cells.borrow();
        for key in keys {
            let Some(widget) = cells.get(key).and_then(glib::WeakRef::upgrade) else {
                continue;
            };
            let Some(bounds) = widget.compute_bounds(self) else {
                continue;
            };
            let paintable = gtk::WidgetPaintable::new(Some(&widget));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(bounds.width()),
                f64::from(bounds.height()),
            );
            let Some(node) = snapshot.to_node() else {
                continue;
            };
            let opacity = widget.opacity();
            widget.set_opacity(0.);
            items.push(PreviewItem {
                key: key.clone(),
                widget: widget.downgrade(),
                node,
                from: bounds,
                draw: bounds,
                opacity,
            });
        }
        let Some(source) = items.iter().find(|item| item.key == armed) else {
            for item in items {
                if let Some(widget) = item.widget.upgrade() {
                    widget.set_opacity(item.opacity);
                }
            }
            return;
        };
        let hotspot =
            graphene::Point::new(pointer.x() - source.from.x(), pointer.y() - source.from.y());
        self.imp().drag.replace(Some(PreviewDrag {
            source: armed,
            hotspot,
            items,
        }));
        self.imp().cancelled.set(false);
        self.queue_draw();
    }
    fn motion(&self, widget: &gtk::Widget, target: &str, x: f64, y: f64) {
        let Some(pointer) = widget.compute_point(self, &graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        let mut state = self.imp().drag.borrow_mut();
        let Some(drag) = state.as_mut() else { return };
        let Some(from) = drag.items.iter().position(|item| item.key == drag.source) else {
            return;
        };
        let Some(to) = drag.items.iter().position(|item| item.key == target) else {
            return;
        };
        let mut order: Vec<_> = (0..drag.items.len()).collect();
        let source = order.remove(from);
        order.insert(to, source);
        let ordered_keys: Vec<_> = order
            .iter()
            .map(|index| drag.items[*index].key.clone())
            .collect();
        let destinations = self
            .imp()
            .layout
            .borrow()
            .as_ref()
            .and_then(|layout| layout(&ordered_keys, self))
            .filter(|rects| rects.len() == drag.items.len())
            .unwrap_or_else(|| drag.items.iter().map(|item| item.from).collect());
        for (slot, item_index) in order.into_iter().enumerate() {
            let destination = destinations[slot];
            let item = &mut drag.items[item_index];
            item.draw = destination;
        }
        if let Some(source) = drag.items.iter_mut().find(|item| item.key == drag.source) {
            source.draw = graphene::Rect::new(
                pointer.x() - drag.hotspot.x(),
                pointer.y() - drag.hotspot.y(),
                source.from.width(),
                source.from.height(),
            );
        }
        drop(state);
        self.queue_draw();
    }
    #[cfg(test)]
    pub(crate) fn held_metrics(&self) -> Option<(f32, usize, bool)> {
        let drag = self.imp().drag.borrow();
        let drag = drag.as_ref()?;
        let source = drag.items.iter().find(|item| item.key == drag.source)?;
        let source_distance = ((source.draw.x() - source.from.x()).powi(2)
            + (source.draw.y() - source.from.y()).powi(2))
        .sqrt();
        let shifted: Vec<_> = drag
            .items
            .iter()
            .filter(|item| item.key != drag.source)
            .filter(|item| {
                (item.draw.x() - item.from.x()).abs() > 1.
                    || (item.draw.y() - item.from.y()).abs() > 1.
            })
            .collect();
        let no_overlap = shifted.iter().enumerate().all(|(index, item)| {
            shifted[index + 1..].iter().all(|other| {
                item.draw.x() + item.draw.width() <= other.draw.x()
                    || other.draw.x() + other.draw.width() <= item.draw.x()
                    || item.draw.y() + item.draw.height() <= other.draw.y()
                    || other.draw.y() + other.draw.height() <= item.draw.y()
            })
        });
        Some((source_distance, shifted.len(), no_overlap))
    }
    #[cfg(test)]
    pub(crate) fn held_neighbors(&self) -> Option<Vec<(String, graphene::Rect)>> {
        let drag = self.imp().drag.borrow();
        let drag = drag.as_ref()?;
        Some(
            drag.items
                .iter()
                .filter(|item| item.key != drag.source)
                .map(|item| (item.key.clone(), item.draw))
                .collect(),
        )
    }
    fn restore(&self) {
        if let Some(drag) = self.imp().drag.take() {
            for item in drag.items {
                if let Some(widget) = item.widget.upgrade() {
                    widget.set_opacity(item.opacity);
                }
            }
        }
        self.imp().armed.take();
        self.queue_draw();
    }
    fn cancel(&self) {
        self.restore();
        self.imp().cancelled.set(true);
    }
    fn finish(&self) {
        self.restore();
        self.imp().cancelled.set(false);
    }
}
pub(crate) fn attach(
    ui: &Rc<Ui>,
    widget: &impl IsA<gtk::Widget>,
    preview: &Preview,
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
    let armed = preview.clone();
    let armed_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    let armed_key = drag_key.clone();
    source.connect_prepare(move |source, x, y| {
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
        if let Some(widget) = armed_widget.upgrade() {
            armed.arm(&armed_key, &widget, x, y);
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
    let live = preview.clone();
    let begin_keys = keys.clone();
    let begin_key = key.clone();
    source.connect_drag_begin(move |source, _| {
        let blank = gtk::WidgetPaintable::new(gtk::Widget::NONE);
        source.set_icon(Some(&blank), 0, 0);
        live.begin(&begin_key, &begin_keys());
        if let Some(widget) = target_widget.upgrade() {
            let host = widget
                .parent()
                .filter(|parent| parent.has_css_class("gif-library-tile"))
                .unwrap_or(widget);
            host.add_css_class("favorite-dragging");
        }
    });
    let target_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    let live = preview.clone();
    source.connect_drag_end(move |_, _, _| {
        live.finish();
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
    let live = preview.clone();
    let hover_key = key.clone();
    target.connect_enter(move |_, _, _| {
        if let Some(widget) = weak_widget.upgrade() {
            widget.add_css_class("favorite-drop-target");
        }
        gdk::DragAction::MOVE
    });
    let weak_widget = widget.clone().upcast::<gtk::Widget>().downgrade();
    target.connect_motion(move |_, x, y| {
        if let Some(widget) = weak_widget.upgrade() {
            live.motion(&widget, &hover_key, x, y);
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
    let live = preview.clone();
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
        if live.imp().cancelled.get() {
            return false;
        }
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
