//! The picture and crop rectangle shown by the picture editor. All geometry lives in
//! `picture_editor::Editor`; this widget only draws it and turns pointer and keyboard input into
//! editor calls. The picture is drawn rotated with a snapshot transform, so no pixels are copied.
use crate::{
    media_authoring::Rotation,
    picture_editor::{hit, Editor, Handle},
};
use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

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

    fn begin_drag(&self, x: f64, y: f64) {
        self.grab_focus();
        let (Some(crop), Some(start)) = (self.crop_px(), self.imp().editor.borrow().as_ref().map(Editor::rect)) else {
            return;
        };
        if let Some(handle) = hit((x, y), crop, GRAB) {
            self.imp().drag.replace(Some((handle, start)));
        }
    }
    fn update_drag(&self, dx: f64, dy: f64) {
        let Some((handle, start)) = *self.imp().drag.borrow() else { return };
        let scale = self.imp().scale.get().max(f64::EPSILON);
        self.edit(|editor| editor.drag(handle, start, (dx / scale).round() as i64, (dy / scale).round() as i64));
    }
    fn update_cursor(&self, x: f64, y: f64) {
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
