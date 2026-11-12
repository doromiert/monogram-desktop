// SPDX-License-Identifier: LicenseRef-NAPL-1.1
//! Rust adaptation of Galeria's layout.RowLayout and viewport-managed Mosaic.
//! Source and license attribution: docs/GIF_GRID.md, LICENSES/NAPL-1.1.txt.
use gtk::{glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
struct Row {
    start: usize,
    end: usize,
    y: i32,
    height: i32,
    closed: bool,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Rect {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
#[derive(Default)]
struct RowLayout {
    width: i32,
    ratios: Vec<f64>,
    rows: Vec<Row>,
    height: i32,
    max_height: i32,
    minimum: i32,
}
impl RowLayout {
    fn new(ratios: &[f64], width: i32) -> Self {
        let mut layout = Self {
            width,
            ratios: ratios
                .iter()
                .map(|ratio| {
                    if ratio.is_finite() {
                        ratio.max(0.05)
                    } else {
                        1.
                    }
                })
                .collect(),
            minimum: 64.min(width).max(1),
            ..Default::default()
        };
        if ratios.is_empty() || width <= 0 {
            return layout;
        }
        let capacity = ((width + 3) / (layout.minimum + 3)).max(1) as usize;
        let mut start = 0;
        let mut total = 0.;
        let mut y = 0;
        for index in 0..ratios.len() {
            total += layout.ratios[index];
            let count = index - start + 1;
            let closed =
                count >= capacity || total * 154. + 3. * (count - 1) as f64 >= width as f64;
            if closed || index + 1 == ratios.len() {
                let height = (((width - 3 * (count - 1) as i32) as f64 / total)
                    .min(154.)
                    .round_ties_even() as i32)
                    .max(layout.minimum);
                layout.rows.push(Row {
                    start,
                    end: index + 1,
                    y,
                    height,
                    closed,
                });
                layout.max_height = layout.max_height.max(height);
                y += height + 3;
                start = index + 1;
                total = 0.;
            }
        }
        layout.height = y - 3;
        layout
    }
    fn visible(&self, low: i32, high: i32) -> Vec<Rect> {
        let first = self
            .rows
            .partition_point(|row| row.y < low - self.max_height);
        let last = self.rows.partition_point(|row| row.y <= high);
        let mut rects = Vec::new();
        for row in &self.rows[first..last] {
            if row.y + row.height < low {
                continue;
            }
            let available = self.width - 3 * (row.end - row.start - 1) as i32;
            let widths: Vec<_> = self.ratios[row.start..row.end]
                .iter()
                .map(|ratio| {
                    ((row.height as f64 * ratio).round_ties_even() as i32).max(self.minimum)
                })
                .collect();
            let used = if row.closed {
                available
            } else {
                available.min(widths.iter().sum())
            };
            let mut weights: Vec<_> = widths.iter().map(|width| width - self.minimum).collect();
            if weights.iter().all(|weight| *weight == 0) {
                weights.fill(1);
            }
            let total: i64 = weights.iter().map(|weight| i64::from(*weight)).sum();
            let extra = used - self.minimum * weights.len() as i32;
            let (mut cumulative, mut previous, mut x) = (0i64, 0, 0);
            for (index, weight) in (row.start..row.end).zip(weights) {
                cumulative += i64::from(weight);
                let allocated =
                    (extra as f64 * cumulative as f64 / total as f64).round_ties_even() as i32;
                let width = self.minimum + allocated - previous;
                previous = allocated;
                rects.push(Rect {
                    index,
                    x,
                    y: row.y,
                    width,
                    height: row.height,
                });
                x += width + 3;
            }
        }
        rects
    }
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct Mosaic {
        pub(super) ratios: RefCell<Vec<f64>>,
        pub(super) layout: RefCell<RowLayout>,
        pub(super) cells: RefCell<BTreeMap<usize, gtk::Widget>>,
        pub(super) visible: RefCell<Vec<Rect>>,
        pub(super) factory: RefCell<Option<Box<dyn Fn(usize) -> gtk::Widget>>>,
        pub(super) build_tick: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Mosaic {
        const NAME: &'static str = "MonogramGifMosaic";
        type Type = super::Mosaic;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Mosaic {
        fn dispose(&self) {
            for (_, child) in self.cells.take() {
                child.unparent();
            }
            self.factory.take();
        }
    }
    impl WidgetImpl for Mosaic {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                return (64, 400, -1, -1);
            }
            self.ensure_layout(for_size.max(64));
            let height = self.layout.borrow().height;
            (height, height, -1, -1)
        }
        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            self.ensure_layout(width);
            let (top, page) = self.obj().visible_range();
            let rects = self
                .layout
                .borrow()
                .visible(top, top.saturating_add(page.saturating_mul(2)));
            let active: HashSet<_> = rects.iter().map(|rect| rect.index).collect();
            self.cells.borrow_mut().retain(|index, child| {
                if active.contains(index) || child.has_css_class("favorite-dragging") {
                    true
                } else {
                    child.unparent();
                    false
                }
            });
            let started = Instant::now();
            let mut created = 0;
            let mut pending = false;
            for rect in &rects {
                if !self.cells.borrow().contains_key(&rect.index) {
                    if created >= 2 || created > 0 && started.elapsed() > Duration::from_millis(3) {
                        pending = true;
                        continue;
                    }
                    if let Some(factory) = self.factory.borrow().as_ref() {
                        let child = factory(rect.index);
                        child.set_parent(&*self.obj());
                        self.cells.borrow_mut().insert(rect.index, child);
                        created += 1;
                    }
                }
                if let Some(child) = self.cells.borrow().get(&rect.index) {
                    child.measure(gtk::Orientation::Horizontal, -1);
                    child.measure(gtk::Orientation::Vertical, rect.width);
                    let transform = gtk::gsk::Transform::new()
                        .translate(&gtk::graphene::Point::new(rect.x as f32, rect.y as f32));
                    child.allocate(rect.width, rect.height, -1, Some(transform));
                }
            }
            self.visible.replace(rects);
            if pending && !self.build_tick.replace(true) {
                self.obj().add_tick_callback(|widget, _| {
                    widget.imp().build_tick.set(false);
                    widget.queue_allocate();
                    glib::ControlFlow::Break
                });
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let color = gtk::gdk::RGBA::new(0.20, 0.20, 0.21, 1.);
            for rect in self.visible.borrow().iter() {
                if let Some(child) = self.cells.borrow().get(&rect.index) {
                    self.obj().snapshot_child(child, snapshot);
                } else {
                    snapshot.append_color(
                        &color,
                        &gtk::graphene::Rect::new(
                            rect.x as f32,
                            rect.y as f32,
                            rect.width as f32,
                            rect.height as f32,
                        ),
                    );
                }
            }
        }
    }
    impl Mosaic {
        fn ensure_layout(&self, width: i32) {
            if self.layout.borrow().width != width {
                self.layout
                    .replace(RowLayout::new(&self.ratios.borrow(), width));
            }
        }
    }
}
glib::wrapper! {
    pub struct Mosaic(ObjectSubclass<imp::Mosaic>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
impl Mosaic {
    pub(super) fn new(factory: impl Fn(usize) -> gtk::Widget + 'static) -> Self {
        let grid: Self = glib::Object::new();
        grid.set_hexpand(true);
        grid.imp().factory.replace(Some(Box::new(factory)));
        grid
    }
    pub(super) fn set_items(&self, ratios: Vec<f64>, append: bool) {
        if !append {
            for (_, child) in self.imp().cells.take() {
                child.unparent();
            }
        }
        self.imp().ratios.replace(ratios);
        self.imp().layout.replace(RowLayout::default());
        self.queue_resize();
    }
    pub(super) fn prospective_rects(&self, ratios: &[f64]) -> Vec<Rect> {
        let layout = RowLayout::new(ratios, self.width().max(64));
        layout.visible(0, layout.height)
    }
    pub(super) fn reorder_items(&self, ratios: Vec<f64>, old_index_for_new: &[usize]) -> bool {
        if ratios.len() != old_index_for_new.len()
            || old_index_for_new.iter().any(|index| *index >= ratios.len())
            || old_index_for_new
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                != ratios.len()
        {
            return false;
        }
        let mut old = self.imp().cells.take();
        let mut reordered = BTreeMap::new();
        for (new_index, old_index) in old_index_for_new.iter().copied().enumerate() {
            if let Some(child) = old.remove(&old_index) {
                reordered.insert(new_index, child);
            }
        }
        debug_assert!(old.is_empty());
        self.imp().cells.replace(reordered);
        self.imp().ratios.replace(ratios);
        self.imp().layout.replace(RowLayout::default());
        self.queue_resize();
        true
    }
    pub(super) fn set_factory(&self, factory: impl Fn(usize) -> gtk::Widget + 'static) {
        self.imp().factory.replace(Some(Box::new(factory)));
    }
    pub(super) fn refresh(&self) {
        self.queue_allocate();
    }
    #[cfg(test)]
    pub(super) fn visible_rects(&self) -> Vec<Rect> {
        self.imp().visible.borrow().clone()
    }
    fn visible_range(&self) -> (i32, i32) {
        let mut parent = self.parent();
        while let Some(widget) = parent {
            if let Ok(scroll) = widget.clone().downcast::<gtk::ScrolledWindow>() {
                let adjustment = scroll.vadjustment();
                return (
                    adjustment.value().floor().max(0.) as i32,
                    adjustment.page_size().ceil().max(1.) as i32,
                );
            }
            parent = widget.parent();
        }
        (0, self.height().max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gallery_rows_fit_and_bound_extreme_tiles() {
        for width in [64, 120, 200, 376, 900, 1920] {
            let layout = RowLayout::new(&[0.01, 1., 1.78, 0.72, 20., 0.05, 1.5, 1.], width);
            let rects = layout.visible(0, layout.height);
            assert_eq!(rects.len(), 8);
            for rect in &rects {
                assert!(rect.width >= 64 && rect.height >= 64);
                assert!(rect.x + rect.width <= width);
            }
            for row in &layout.rows {
                let members: Vec<_> = rects.iter().filter(|rect| rect.y == row.y).collect();
                if row.closed {
                    assert_eq!(
                        members.last().unwrap().x + members.last().unwrap().width,
                        width
                    );
                }
                for pair in members.windows(2) {
                    assert_eq!(pair[1].x - pair[0].x - pair[0].width, 3);
                }
            }
            let band = layout.visible(200, 500);
            assert!(
                band.iter()
                    .all(|rect| rect.y <= 500 && rect.y + rect.height >= 200)
            );
        }
    }
}
