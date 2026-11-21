//! The picture and crop rectangle shown by the picture editor. All geometry lives in
//! `picture_editor::Editor`; this widget only draws it and turns pointer and keyboard input into
//! editor calls. The picture is drawn rotated with a snapshot transform, so no pixels are copied.
use crate::{
    media_authoring::{Rect, Rotation},
    picture_editor::{hit, Editor, Handle, Weight, INKS},
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
}

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
        pub pending: Cell<Option<Rect>>,
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
                gtk::Orientation::Horizontal => (240, 480, -1, -1),
                _ => (200, 360, -1, -1),
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
        imp.pending.set(None);
        imp.drag.replace(None);
        self.set_cursor_from_name(Some(if tool == Tool::Square { "crosshair" } else { "default" }));
        self.queue_draw();
    }
    /// The colour and line thickness new squares get.
    pub fn set_pen(&self, color: [u8; 3], weight: Weight) {
        self.imp().ink.set(Some(color));
        self.imp().weight.set(Some(weight));
        self.queue_draw();
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
        if self.imp().tool.get() == Tool::Square {
            self.imp().sketch.set(self.view_point(x, y));
            self.imp().pending.set(None);
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
            let (x0, y0, x1, y1) = (sx.min(ex).round(), sy.min(ey).round(), sx.max(ex).round(), sy.max(ey).round());
            self.imp().pending.set(Some(Rect { x: x0 as u32, y: y0 as u32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 }));
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
        if let Some(rect) = imp.pending.take() {
            let (color, weight) = self.pen();
            self.edit(|editor| {
                editor.add_square(rect, color, weight);
            });
        }
        self.queue_draw();
    }
    fn update_cursor(&self, x: f64, y: f64) {
        if self.imp().tool.get() == Tool::Square {
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
        let mut outlines: Vec<(Rect, [u8; 3], f64)> =
            editor.marks().iter().map(|mark| (mark.rect, mark.color, f64::from(mark.stroke))).collect();
        if let Some(rect) = self.imp().pending.get() {
            let (color, weight) = self.pen();
            outlines.push((rect, color, f64::from(editor.stroke_for(weight))));
        }
        for (mark, color, stroke) in outlines {
            let colour = gdk::RGBA::new(f32::from(color[0]) / 255., f32::from(color[1]) / 255., f32::from(color[2]) / 255., 1.);
            let (mx, my) = (left + f64::from(mark.x) * scale, top + f64::from(mark.y) * scale);
            let (mw, mh) = (f64::from(mark.width) * scale, f64::from(mark.height) * scale);
            let line = (stroke * scale).clamp(1.5, mw.min(mh) / 2.).max(1.);
            let outline = gsk::RoundedRect::from_rect(rect(mx, my, mw, mh), 0.);
            snapshot.append_border(&outline, &[line as f32; 4], &[colour, colour, colour, colour]);
        }

        let Some((x, y, w, h)) = self.crop_px() else { return };
        // Dim everything outside the crop.
        let dim = gdk::RGBA::new(0., 0., 0., 0.55);
        snapshot.append_color(&dim, &rect(left, top, shown_w, y - top));
        snapshot.append_color(&dim, &rect(left, y + h, shown_w, top + shown_h - (y + h)));
        snapshot.append_color(&dim, &rect(left, y, x - left, h));
        snapshot.append_color(&dim, &rect(x + w, y, left + shown_w - (x + w), h));

        let white = gdk::RGBA::new(1., 1., 1., 1.);
        let thirds = gdk::RGBA::new(1., 1., 1., 0.35);
        for i in 1..3 {
            let step = f64::from(i) / 3.;
            snapshot.append_color(&thirds, &rect(x + w * step - 0.5, y, 1., h));
            snapshot.append_color(&thirds, &rect(x, y + h * step - 0.5, w, 1.));
        }
        // A thicker frame while focused shows where keyboard input goes.
        let border = if self.has_focus() { 3. } else { 2. };
        let outline = gsk::RoundedRect::from_rect(rect(x, y, w, h), 0.);
        snapshot.append_border(&outline, &[border as f32; 4], &[white, white, white, white]);
        // Handles at the corners and edge midpoints.
        let shade = gdk::RGBA::new(0., 0., 0., 0.6);
        for (hx, hy) in [(x, y), (x + w / 2., y), (x + w, y), (x, y + h / 2.), (x + w, y + h / 2.), (x, y + h), (x + w / 2., y + h), (x + w, y + h)] {
            snapshot.append_color(&shade, &rect(hx - 6., hy - 6., 12., 12.));
            snapshot.append_color(&white, &rect(hx - 5., hy - 5., 10., 10.));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture_editor::{Editor, Weight, MIN_MARK};

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
        area.editor().unwrap().marks().iter().map(|m| (m.rect, m.color, m.stroke)).collect()
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
        assert!(area.imp().pending.get().is_some());
        area.set_tool(Tool::Crop);
        area.finish_sketch();
        assert!(marks(&area).is_empty());
        assert!(area.imp().pending.get().is_none());
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
}
