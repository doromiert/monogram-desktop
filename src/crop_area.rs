//! The picture and crop rectangle shown by the picture editor. All geometry lives in
//! `picture_editor::Editor`; this widget only draws it and turns pointer and keyboard input into
//! editor calls. The picture is drawn rotated with a snapshot transform, so no pixels are copied.
use crate::{
    media_authoring::{arrow_segments, Point, Rect, Rotation, Shape},
    picture_editor::{hit, Editor, Handle, Kind, ViewMark, ViewShape, Weight, INKS},
};
use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

/// What a drag on the picture does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Move and resize the crop.
    #[default]
    Crop,
    /// Draw a square outline.
    Square,
    /// Draw an ellipse outline.
    Circle,
    /// Draw an arrow.
    Arrow,
    /// Draw freehand.
    Pen,
}
impl Tool {
    /// What this tool draws; `None` for the crop.
    pub fn kind(self) -> Option<Kind> {
        match self {
            Self::Crop => None,
            Self::Square => Some(Kind::Square),
            Self::Circle => Some(Kind::Circle),
            Self::Arrow => Some(Kind::Arrow),
            Self::Pen => Some(Kind::Freehand),
        }
    }
}

/// Most points a stroke in progress keeps, so a long drag cannot grow without bound.
const MAX_SKETCH_POINTS: usize = 4096;

const MARGIN: f64 = 14.;
/// Pointer reach of a handle, in widget pixels.
const GRAB: f64 = 12.;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CropArea {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub editor: RefCell<Option<Editor>>,
        /// The handle being dragged and the crop when the drag began.
        pub drag: RefCell<Option<(Handle, crate::media_authoring::Rect)>>,
        pub changed: RefCell<Option<Box<dyn Fn()>>>,
        pub scale: Cell<f64>,
        pub tool: Cell<Tool>,
        pub ink: Cell<Option<[u8; 3]>>,
        pub weight: Cell<Option<Weight>>,
        /// Where a square drag began, in view pixels, and the square so far.
        pub sketch: Cell<Option<(f64, f64)>>,
        pub pending: RefCell<Option<ViewShape>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CropArea {
        const NAME: &'static str = "MonogramCropArea";
        type Type = super::CropArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CropArea {
        fn constructed(&self) {
            self.parent_constructed();
            let area = self.obj();
            area.set_focusable(true);
            area.set_can_focus(true);
            area.set_hexpand(true);
            area.set_vexpand(true);
            area.add_css_class("crop-area");
            area.set_accessible_role(gtk::AccessibleRole::Application);
            area.update_property(&[
                gtk::accessible::Property::Label("Crop area"),
                gtk::accessible::Property::Description(
                    "Arrow keys move the crop. Shift and arrow keys resize it from the bottom right.",
                ),
            ]);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let weak = area.downgrade();
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(area) = weak.upgrade() {
                    area.begin_drag(x, y);
                }
            });
            let weak = area.downgrade();
            drag.connect_drag_update(move |_, dx, dy| {
                if let Some(area) = weak.upgrade() {
                    area.update_drag(dx, dy);
                }
            });
            let weak = area.downgrade();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(area) = weak.upgrade() {
                    area.imp().drag.replace(None);
                    area.finish_sketch();
                }
            });
            area.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            let weak = area.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(area) = weak.upgrade() {
                    area.update_cursor(x, y);
                }
            });
            area.add_controller(motion);

            let keys = gtk::EventControllerKey::new();
            let weak = area.downgrade();
            keys.connect_key_pressed(move |_, key, _, state| {
                weak.upgrade()
                    .map_or(glib::Propagation::Proceed, |area| area.key(key, state))
            });
            area.add_controller(keys);
        }
        fn dispose(&self) {
            self.texture.take();
        }
    }

    impl WidgetImpl for CropArea {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // Small enough for a narrow window, comfortable by default.
            match orientation {
                gtk::Orientation::Horizontal => (200, 480, -1, -1),
                _ => (160, 360, -1, -1),
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct CropArea(ObjectSubclass<imp::CropArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for CropArea {
    fn default() -> Self {
        Self::new()
    }
}

impl CropArea {
    pub fn new() -> Self {
        glib::Object::new()
    }
    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        self.imp().texture.replace(texture);
        self.queue_draw();
    }
    pub fn set_editor(&self, editor: Editor) {
        self.imp().editor.replace(Some(editor));
        self.queue_draw();
    }
    pub fn tool(&self) -> Tool {
        self.imp().tool.get()
    }
    pub fn set_tool(&self, tool: Tool) {
        let imp = self.imp();
        imp.tool.set(tool);
        imp.sketch.set(None);
        imp.pending.replace(None);
        imp.drag.replace(None);
        self.set_cursor_from_name(Some(if tool == Tool::Crop { "default" } else { "crosshair" }));
        self.queue_draw();
    }
    /// The colour and line thickness new squares get.
    pub fn set_pen(&self, color: [u8; 3], weight: Weight) {
        self.imp().ink.set(Some(color));
        self.imp().weight.set(Some(weight));
        self.queue_draw();
    }
    /// The colour and thickness new marks get now.
    pub fn pen_choice(&self) -> ([u8; 3], Weight) {
        self.pen()
    }
    fn pen(&self) -> ([u8; 3], Weight) {
        (self.imp().ink.get().unwrap_or(INKS[0].1), self.imp().weight.get().unwrap_or(Weight::Medium))
    }
    pub fn editor(&self) -> Option<Editor> {
        self.imp().editor.borrow().clone()
    }
    /// Edit the model in place (rotate, reset, typed numbers) and redraw.
    pub fn edit(&self, change: impl FnOnce(&mut Editor)) {
        if let Some(editor) = self.imp().editor.borrow_mut().as_mut() {
            change(editor);
        }
        self.queue_draw();
        self.notify_changed();
    }
    pub fn connect_changed(&self, callback: impl Fn() + 'static) {
        self.imp().changed.replace(Some(Box::new(callback)));
    }
    fn notify_changed(&self) {
        if let Some(callback) = self.imp().changed.borrow().as_ref() {
            callback();
        }
    }

    /// The picture's box in widget pixels (origin and scale), centred with a margin.
    fn layout(&self) -> Option<(f64, f64, f64)> {
        let editor = self.imp().editor.borrow();
        let (view_w, view_h) = editor.as_ref()?.view_size();
        let (width, height) = (f64::from(self.width()) - 2. * MARGIN, f64::from(self.height()) - 2. * MARGIN);
        if width <= 0. || height <= 0. {
            return None;
        }
        let scale = (width / f64::from(view_w)).min(height / f64::from(view_h));
        let (shown_w, shown_h) = (f64::from(view_w) * scale, f64::from(view_h) * scale);
        self.imp().scale.set(scale);
        Some((
            (f64::from(self.width()) - shown_w) / 2.,
            (f64::from(self.height()) - shown_h) / 2.,
            scale,
        ))
    }

    /// The crop rectangle in widget pixels.
    fn crop_px(&self) -> Option<(f64, f64, f64, f64)> {
        let (left, top, scale) = self.layout()?;
        let rect = self.imp().editor.borrow().as_ref()?.rect();
        Some((
            left + f64::from(rect.x) * scale,
            top + f64::from(rect.y) * scale,
            f64::from(rect.width) * scale,
            f64::from(rect.height) * scale,
        ))
    }

    /// A point in widget pixels as a point in the picture, in view pixels, kept inside it.
    fn view_point(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (left, top, scale) = self.layout()?;
        let (view_w, view_h) = self.imp().editor.borrow().as_ref()?.view_size();
        Some((((x - left) / scale).clamp(0., f64::from(view_w)), ((y - top) / scale).clamp(0., f64::from(view_h))))
    }
    fn begin_drag(&self, x: f64, y: f64) {
        self.grab_focus();
        if self.imp().tool.get() != Tool::Crop {
            let start = self.view_point(x, y);
            self.imp().sketch.set(start);
            self.imp().pending.replace(None);
            return;
        }
        let (Some(crop), Some(start)) = (self.crop_px(), self.imp().editor.borrow().as_ref().map(Editor::rect)) else {
            return;
        };
        if let Some(handle) = hit((x, y), crop, GRAB) {
            self.imp().drag.replace(Some((handle, start)));
        }
    }
    fn update_drag(&self, dx: f64, dy: f64) {
        if let Some((sx, sy)) = self.imp().sketch.get() {
            let scale = self.imp().scale.get().max(f64::EPSILON);
            let Some((view_w, view_h)) = self.imp().editor.borrow().as_ref().map(Editor::view_size) else { return };
            let (ex, ey) = ((sx + dx / scale).clamp(0., f64::from(view_w)), (sy + dy / scale).clamp(0., f64::from(view_h)));
            let whole = |value: f64| value.round() as u32;
            let shape = match self.imp().tool.get() {
                Tool::Crop => return,
                Tool::Square | Tool::Circle => {
                    let (x0, y0, x1, y1) = (sx.min(ex).round(), sy.min(ey).round(), sx.max(ex).round(), sy.max(ey).round());
                    let rect = Rect { x: x0 as u32, y: y0 as u32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
                    ViewShape::Rect(if self.imp().tool.get() == Tool::Circle { Shape::Circle } else { Shape::Square }, rect)
                }
                Tool::Arrow => ViewShape::Arrow((whole(sx), whole(sy)), (whole(ex), whole(ey))),
                Tool::Pen => {
                    let mut path = match self.imp().pending.take() {
                        Some(ViewShape::Freehand(path)) => path,
                        _ => vec![(whole(sx), whole(sy))],
                    };
                    let here = (whole(ex), whole(ey));
                    if path.last() != Some(&here) && path.len() < MAX_SKETCH_POINTS {
                        path.push(here);
                    }
                    ViewShape::Freehand(path)
                }
            };
            self.imp().pending.replace(Some(shape));
            self.queue_draw();
            return;
        }
        let Some((handle, start)) = *self.imp().drag.borrow() else { return };
        let scale = self.imp().scale.get().max(f64::EPSILON);
        self.edit(|editor| editor.drag(handle, start, (dx / scale).round() as i64, (dy / scale).round() as i64));
    }
    /// The pointer was released: keep the square if it is big enough to be one.
    fn finish_sketch(&self) {
        let imp = self.imp();
        imp.sketch.set(None);
        if let Some(shape) = imp.pending.take() {
            let (color, weight) = self.pen();
            self.edit(|editor| {
                match shape {
                    ViewShape::Rect(shape, rect) => editor.add_mark(shape, rect, color, weight),
                    ViewShape::Arrow(from, to) => editor.add_arrow(from, to, color, weight),
                    ViewShape::Freehand(points) => editor.add_path(&points, color, weight),
                };
            });
        }
        self.queue_draw();
    }
    fn update_cursor(&self, x: f64, y: f64) {
        if self.imp().tool.get() != Tool::Crop {
            self.set_cursor_from_name(Some("crosshair"));
            return;
        }
        let name = self.crop_px().and_then(|crop| hit((x, y), crop, GRAB)).map(|handle| match handle {
            Handle::Move => "move",
            Handle::TopLeft | Handle::BottomRight => "nwse-resize",
            Handle::TopRight | Handle::BottomLeft => "nesw-resize",
            Handle::Top | Handle::Bottom => "ns-resize",
            Handle::Left | Handle::Right => "ew-resize",
        });
        self.set_cursor_from_name(name.or(Some("default")));
    }
    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let (dx, dy) = match key {
            gdk::Key::Left | gdk::Key::KP_Left => (-1, 0),
            gdk::Key::Right | gdk::Key::KP_Right => (1, 0),
            gdk::Key::Up | gdk::Key::KP_Up => (0, -1),
            gdk::Key::Down | gdk::Key::KP_Down => (0, 1),
            _ => return glib::Propagation::Proceed,
        };
        let step = self.imp().editor.borrow().as_ref().map_or(1, |e| i64::from((e.view_size().0 / 100).max(1)));
        let (dx, dy) = (dx * step, dy * step);
        self.edit(|editor| {
            if state.contains(gdk::ModifierType::SHIFT_MASK) {
                let start = editor.rect();
                editor.drag(Handle::BottomRight, start, dx, dy);
            } else {
                editor.move_by(dx, dy);
            }
        });
        glib::Propagation::Stop
    }

    /// One mark, drawn the way the helper will paint it: outlines for squares and circles, round
    /// strokes for the arrow and the freehand line.
    fn draw_mark(&self, snapshot: &gtk::Snapshot, mark: &ViewMark, (left, top, scale): (f64, f64, f64)) {
        let colour = gdk::RGBA::new(f32::from(mark.color[0]) / 255., f32::from(mark.color[1]) / 255., f32::from(mark.color[2]) / 255., 1.);
        let line = f64::from(mark.stroke) * scale;
        let at = |p: &Point| (left + f64::from(p.0) * scale, top + f64::from(p.1) * scale);
        let stroke_path = |segments: &[((f64, f64), (f64, f64))]| {
            let path = gsk::PathBuilder::new();
            for (a, b) in segments {
                path.move_to(a.0 as f32, a.1 as f32);
                path.line_to(b.0 as f32, b.1 as f32);
            }
            let pen = gsk::Stroke::new(line.max(1.5) as f32);
            pen.set_line_cap(gsk::LineCap::Round);
            pen.set_line_join(gsk::LineJoin::Round);
            snapshot.append_stroke(&path.to_path(), &pen, &colour);
        };
        match &mark.shape {
            ViewShape::Rect(shape, rect) => {
                let (mx, my) = (left + f64::from(rect.x) * scale, top + f64::from(rect.y) * scale);
                let (mw, mh) = (f64::from(rect.width) * scale, f64::from(rect.height) * scale);
                let width = line.clamp(1.5, (mw.min(mh) / 2.).max(1.5)) as f32;
                let bounds = graphene::Rect::new(mx as f32, my as f32, mw as f32, mh as f32);
                let outline = if *shape == Shape::Circle {
                    let corner = graphene::Size::new((mw / 2.) as f32, (mh / 2.) as f32);
                    gsk::RoundedRect::new(bounds, corner, corner, corner, corner)
                } else {
                    gsk::RoundedRect::from_rect(bounds, 0.)
                };
                snapshot.append_border(&outline, &[width; 4], &[colour, colour, colour, colour]);
            }
            ViewShape::Arrow(from, to) => {
                let segments: Vec<_> = arrow_segments(*from, *to, mark.stroke)
                    .iter()
                    .map(|(a, b)| ((left + a.0 * scale, top + a.1 * scale), (left + b.0 * scale, top + b.1 * scale)))
                    .collect();
                stroke_path(&segments);
            }
            ViewShape::Freehand(points) => {
                let segments: Vec<_> = points.windows(2).map(|pair| (at(&pair[0]), at(&pair[1]))).collect();
                stroke_path(&segments);
            }
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let (Some((left, top, scale)), Some(editor)) = (self.layout(), self.editor()) else { return };
        let (view_w, view_h) = editor.view_size();
        let (shown_w, shown_h) = (f64::from(view_w) * scale, f64::from(view_h) * scale);
        let rect = |x: f64, y: f64, w: f64, h: f64| graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);

        if let Some(texture) = self.imp().texture.borrow().as_ref() {
            // The texture is the unrotated picture; turn it about the centre of its box.
            let angle = match editor.rotation() {
                Rotation::None => 0.,
                Rotation::Clockwise90 => 90.,
                Rotation::Half => 180.,
                Rotation::Counterclockwise90 => 270.,
            };
            let (source_w, source_h) = if matches!(editor.rotation(), Rotation::Clockwise90 | Rotation::Counterclockwise90) {
                (shown_h, shown_w)
            } else {
                (shown_w, shown_h)
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new((left + shown_w / 2.) as f32, (top + shown_h / 2.) as f32));
            snapshot.rotate(angle);
            snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &rect(-source_w / 2., -source_h / 2., source_w, source_h));
            snapshot.restore();
        }

        // Marks sit on the picture, under the dimming, so what a crop would cut away stays dim.
        let mut marks: Vec<ViewMark> = editor.marks().to_vec();
        if let Some(shape) = self.imp().pending.borrow().clone() {
            let (color, weight) = self.pen();
            marks.push(ViewMark { shape, color, stroke: editor.stroke_for(weight) });
        }
        for mark in &marks {
            self.draw_mark(snapshot, mark, (left, top, scale));
        }

        let Some((x, y, w, h)) = self.crop_px() else { return };
        // Dim everything outside the crop.
        let dim = gdk::RGBA::new(0., 0., 0., 0.55);
        snapshot.append_color(&dim, &rect(left, top, shown_w, y - top));
        snapshot.append_color(&dim, &rect(left, y + h, shown_w, top + shown_h - (y + h)));
        snapshot.append_color(&dim, &rect(left, y, x - left, h));
        snapshot.append_color(&dim, &rect(x + w, y, left + shown_w - (x + w), h));

        let white = gdk::RGBA::new(1., 1., 1., 1.);
        let shade = gdk::RGBA::new(0., 0., 0., 0.45);
        // A thin frame, thicker while focused so keyboard users can see where input goes.
        let border = if self.has_focus() { 2. } else { 1. };
        let outline = gsk::RoundedRect::from_rect(rect(x, y, w, h), 0.);
        snapshot.append_border(&outline, &[border as f32; 4], &[white, white, white, white]);
        // Only while cropping: corner brackets and edge grips to drag, each with a dark halo so
        // they stay visible on a light picture.
        if self.imp().tool.get() == Tool::Crop {
            let (arm, thick) = (18.0_f64.min(w / 2.).min(h / 2.), 4.);
            let bracket = |cx: f64, cy: f64, right: bool, down: bool| {
                let ox = if right { cx - arm } else { cx };
                let oy = if down { cy - thick } else { cy };
                let ix = if right { cx - thick } else { cx };
                let iy = if down { cy - arm } else { cy };
                for (px, py, pw, ph) in [(ox, oy, arm, thick), (ix, iy, thick, arm)] {
                    snapshot.append_color(&shade, &rect(px - 1., py - 1., pw + 2., ph + 2.));
                    snapshot.append_color(&white, &rect(px, py, pw, ph));
                }
            };
            bracket(x, y, false, false);
            bracket(x + w, y, true, false);
            bracket(x, y + h, false, true);
            bracket(x + w, y + h, true, true);
            let grip = 14.0_f64.min(w / 3.).min(h / 3.);
            for (gx, gy, gw, gh) in [
                (x + w / 2. - grip / 2., y - thick / 2., grip, thick),
                (x + w / 2. - grip / 2., y + h - thick / 2., grip, thick),
                (x - thick / 2., y + h / 2. - grip / 2., thick, grip),
                (x + w - thick / 2., y + h / 2. - grip / 2., thick, grip),
            ] {
                snapshot.append_color(&shade, &rect(gx - 1., gy - 1., gw + 2., gh + 2.));
                snapshot.append_color(&white, &rect(gx, gy, gw, gh));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture_editor::{Editor, Weight, MIN_MARK};
    use crate::picture_editor::ViewShape;

    fn settle() {
        let context = glib::MainContext::default();
        for _ in 0..20 {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    /// An area showing a 200x100 picture, in a window big enough to lay it out.
    fn shown() -> (CropArea, gtk::Window) {
        gtk::init().expect("display");
        let area = CropArea::new();
        area.set_editor(Editor::new(200, 100));
        let window = gtk::Window::builder().default_width(500).default_height(400).child(&area).build();
        window.present();
        settle();
        (area, window)
    }
    /// Press at picture point `(x, y)`, move by `(dx, dy)` picture pixels, release.
    fn drag(area: &CropArea, from: (f64, f64), by: (f64, f64)) {
        let (left, top, scale) = area.layout().expect("laid out");
        area.begin_drag(left + from.0 * scale, top + from.1 * scale);
        area.update_drag(by.0 * scale, by.1 * scale);
        area.finish_sketch();
    }
    fn marks(area: &CropArea) -> Vec<(Rect, [u8; 3], u32)> {
        area.editor().unwrap().marks().iter().map(|m| (m.bounds(), m.color, m.stroke)).collect()
    }
    fn near(rect: Rect, x: u32, y: u32, width: u32, height: u32) -> bool {
        let close = |a: u32, b: u32| a.abs_diff(b) <= 1;
        close(rect.x, x) && close(rect.y, y) && close(rect.width, width) && close(rect.height, height)
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn dragging_with_the_square_tool_draws_a_square_where_the_pointer_went() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        drag(&area, (20., 10.), (60., 40.));
        let made = marks(&area);
        assert_eq!(made.len(), 1);
        assert!(near(made[0].0, 20, 10, 60, 40), "{:?}", made[0].0);
        assert_eq!(area.editor().unwrap().rect(), Rect { x: 0, y: 0, width: 200, height: 100 }, "the crop did not move");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_drag_up_and_to_the_left_makes_the_same_kind_of_square() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        drag(&area, (100., 60.), (-50., -30.));
        assert!(near(marks(&area)[0].0, 50, 30, 50, 30), "{:?}", marks(&area)[0].0);
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_square_dragged_past_the_picture_stops_at_its_edge() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        drag(&area, (150., 70.), (500., 500.));
        assert!(near(marks(&area)[0].0, 150, 70, 50, 30), "{:?}", marks(&area)[0].0);
        drag(&area, (30., 20.), (-500., -500.));
        assert!(near(marks(&area)[1].0, 0, 0, 30, 20), "{:?}", marks(&area)[1].0);
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_click_or_a_tiny_drag_does_not_make_a_mark() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        drag(&area, (50., 50.), (0., 0.));
        drag(&area, (50., 50.), (f64::from(MIN_MARK) - 2., 40.));
        assert!(marks(&area).is_empty());
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_crop_tool_moves_the_crop_and_never_draws() {
        let (area, _window) = shown();
        area.edit(|editor| editor.set_rect(40, 20, 100, 50));
        assert_eq!(area.tool(), Tool::Crop);
        drag(&area, (90., 45.), (20., 10.));
        assert!(marks(&area).is_empty());
        assert_eq!(area.editor().unwrap().rect(), Rect { x: 60, y: 30, width: 100, height: 50 }, "the crop moved");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn switching_tools_drops_a_square_that_was_still_being_dragged() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        let (left, top, scale) = area.layout().unwrap();
        area.begin_drag(left + 10. * scale, top + 10. * scale);
        area.update_drag(50. * scale, 30. * scale);
        assert!(area.imp().pending.borrow().is_some());
        area.set_tool(Tool::Crop);
        area.finish_sketch();
        assert!(marks(&area).is_empty());
        assert!(area.imp().pending.borrow().is_none());
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn new_squares_take_the_chosen_colour_and_thickness() {
        let (area, _window) = shown();
        // A larger picture, so thin, medium and thick are different pixel counts.
        area.set_editor(Editor::new(2000, 1000));
        area.set_tool(Tool::Square);
        drag(&area, (100., 100.), (400., 300.));
        area.set_pen([1, 2, 3], Weight::Thick);
        drag(&area, (600., 100.), (400., 300.));
        let made = marks(&area);
        assert_eq!(made[0].1, crate::picture_editor::INKS[0].1, "red unless told otherwise");
        assert_eq!(made[1].1, [1, 2, 3]);
        assert!(made[1].2 > made[0].2, "thick is thicker than the default");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn squares_turn_with_the_picture_and_the_area_keeps_drawing_after_it_turns() {
        let (area, _window) = shown();
        area.set_tool(Tool::Square);
        drag(&area, (10., 10.), (40., 20.));
        area.edit(|editor| editor.rotate(true));
        assert_eq!(area.editor().unwrap().view_size(), (100, 200));
        assert_eq!(marks(&area)[0].0, Rect { x: 100 - 10 - 20, y: 10, width: 20, height: 40 }, "turned a quarter with the picture");
        drag(&area, (10., 100.), (30., 30.));
        assert_eq!(marks(&area).len(), 2);
    }

    fn shapes(area: &CropArea) -> Vec<ViewShape> {
        area.editor().unwrap().marks().iter().map(|m| m.shape.clone()).collect()
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_circle_tool_makes_a_circle_in_the_dragged_box() {
        let (area, _window) = shown();
        area.set_tool(Tool::Circle);
        drag(&area, (20., 10.), (60., 40.));
        let ViewShape::Rect(shape, rect) = shapes(&area)[0].clone() else { panic!("a rectangle shape") };
        assert_eq!(shape, Shape::Circle);
        assert!(near(rect, 20, 10, 60, 40), "{rect:?}");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_arrow_tool_points_from_where_the_drag_began_to_where_it_ended() {
        let (area, _window) = shown();
        area.set_tool(Tool::Arrow);
        drag(&area, (30., 70.), (100., -50.));
        let ViewShape::Arrow(from, to) = shapes(&area)[0].clone() else { panic!("an arrow") };
        let close = |a: (u32, u32), b: (u32, u32)| a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1;
        assert!(close(from, (30, 70)) && close(to, (130, 20)), "{from:?} -> {to:?}");
        // The arrow is not normalised: the tip is where the pointer let go, even up and to the left.
        drag(&area, (100., 80.), (-60., -40.));
        let ViewShape::Arrow(from, to) = shapes(&area)[1].clone() else { panic!("an arrow") };
        assert!(close(from, (100, 80)) && close(to, (40, 40)), "{from:?} -> {to:?}");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn the_pen_follows_every_update_of_the_drag() {
        let (area, _window) = shown();
        area.set_tool(Tool::Pen);
        let (left, top, scale) = area.layout().unwrap();
        area.begin_drag(left + 10. * scale, top + 10. * scale);
        for (dx, dy) in [(20., 0.), (40., 10.), (40., 30.), (10., 30.)] {
            area.update_drag(dx * scale, dy * scale);
        }
        area.finish_sketch();
        let ViewShape::Freehand(points) = shapes(&area)[0].clone() else { panic!("a stroke") };
        let close = |a: (u32, u32), b: (u32, u32)| a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1;
        assert_eq!(points.len(), 5, "the start and four moves: {points:?}");
        for (got, want) in points.iter().zip([(10, 10), (30, 10), (50, 20), (50, 40), (20, 40)]) {
            assert!(close(*got, want), "{points:?}");
        }
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_pen_click_and_a_jittering_pen_leave_no_stroke_and_a_new_drag_starts_fresh() {
        let (area, _window) = shown();
        area.set_tool(Tool::Pen);
        drag(&area, (50., 50.), (0., 0.));
        drag(&area, (50., 50.), (2., 1.));
        assert!(marks(&area).is_empty());
        drag(&area, (20., 20.), (50., 0.));
        drag(&area, (20., 60.), (50., 0.));
        let strokes = shapes(&area);
        assert_eq!(strokes.len(), 2);
        let ViewShape::Freehand(second) = &strokes[1] else { panic!("a stroke") };
        assert!(second.iter().all(|p| p.1.abs_diff(60) <= 1), "the second stroke did not inherit the first's points: {second:?}");
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn a_very_long_drag_stays_bounded_and_still_draws() {
        let (area, _window) = shown();
        area.set_editor(Editor::new(5000, 100));
        area.set_tool(Tool::Pen);
        let (left, top, scale) = area.layout().unwrap();
        area.begin_drag(left, top);
        for step in 1..=6000 {
            area.update_drag(f64::from(step) * 0.7 * scale, f64::from(step % 90) * scale);
        }
        assert!(area.imp().pending.borrow().as_ref().is_some_and(|shape| matches!(shape, ViewShape::Freehand(p) if p.len() <= MAX_SKETCH_POINTS)));
        area.finish_sketch();
        let ViewShape::Freehand(points) = shapes(&area)[0].clone() else { panic!("a stroke") };
        assert!(points.len() <= crate::picture_editor::MAX_PATH_POINTS);
    }

    #[gtk::test]
    #[ignore = "requires a native display"]
    fn each_tool_sets_the_matching_cursor_and_the_crop_tool_restores_it() {
        let (area, _window) = shown();
        for tool in [Tool::Square, Tool::Circle, Tool::Arrow, Tool::Pen] {
            area.set_tool(tool);
            assert_eq!(area.cursor().and_then(|c| c.name()).as_deref(), Some("crosshair"), "{tool:?}");
            assert!(tool.kind().is_some());
        }
        area.set_tool(Tool::Crop);
        assert_eq!(area.cursor().and_then(|c| c.name()).as_deref(), Some("default"));
        assert!(Tool::Crop.kind().is_none());
    }
}
