//! Galeria's pointer-anchored zoom and bounded panning, shared by every media type.
use super::*;

impl FlyFrame {
    pub(super) fn media_bounds(&self) -> Option<graphene::Rect> {
        let picture = self.first_child()?.downcast::<gtk::Picture>().ok()?;
        let paintable = picture.paintable()?;
        let bounds = picture.compute_bounds(self)?;
        let ratio = paintable.intrinsic_aspect_ratio();
        if ratio <= 0. || bounds.width() <= 0. || bounds.height() <= 0. {
            return None;
        }
        let width = bounds.width().min(bounds.height() * ratio as f32);
        let height = bounds.height().min(bounds.width() / ratio as f32);
        Some(graphene::Rect::new(
            bounds.x() + (bounds.width() - width) / 2.,
            bounds.y() + (bounds.height() - height) / 2.,
            width,
            height,
        ))
    }
    pub(super) fn clamp_pan(&self) {
        let Some(media) = self.media_bounds() else {
            return;
        };
        let Some(viewport) = self.first_child().and_then(|c| c.compute_bounds(self)) else {
            return;
        };
        let zoom = self.imp().zoom.get();
        let x_limit = (f64::from(media.width()) * zoom - f64::from(viewport.width())).max(0.) / 2.;
        let y_limit =
            (f64::from(media.height()) * zoom - f64::from(viewport.height())).max(0.) / 2.;
        let (x, y) = self.imp().pan.get();
        self.imp()
            .pan
            .set((x.clamp(-x_limit, x_limit), y.clamp(-y_limit, y_limit)));
        self.queue_draw();
    }
}

impl MediaViewer {
    fn zoom_frame(&self) -> Option<FlyFrame> {
        self.frames.borrow().get(self.index.get()).cloned()
    }
    pub(super) fn is_zoomed(&self) -> bool {
        self.zoom_frame()
            .is_some_and(|f| f.imp().zoom.get() > 1.001)
    }
    fn zoom_usable(&self) -> bool {
        !self.closing.get() && self.progress.get() >= 1.
    }
    fn controls_at(&self, x: f64, y: f64) -> bool {
        let mut target = self.root.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = target {
            if widget.is::<gtk::Button>()
                || widget.is::<gtk::Range>()
                || widget.is::<gtk::MediaControls>()
                || widget.is::<adw::HeaderBar>()
            {
                return true;
            }
            if widget == self.root {
                break;
            }
            target = widget.parent();
        }
        false
    }
    fn zoom_to(&self, value: f64, pointer: Option<(f64, f64)>) {
        if !self.zoom_usable() || !value.is_finite() {
            return;
        }
        let Some(frame) = self.zoom_frame() else {
            return;
        };
        let Some(bounds) = frame.media_bounds() else {
            return;
        };
        let cx = f64::from(bounds.x() + bounds.width() / 2.);
        let cy = f64::from(bounds.y() + bounds.height() / 2.);
        let anchor = pointer
            .and_then(|(x, y)| {
                self.root
                    .compute_point(&frame, &graphene::Point::new(x as f32, y as f32))
            })
            .map(|p| (f64::from(p.x()), f64::from(p.y())))
            .unwrap_or((cx, cy));
        let value = value.clamp(1., 8.);
        let ratio = value / frame.imp().zoom.get();
        let (x, y) = frame.imp().pan.get();
        frame.imp().pan.set((
            (anchor.0 - cx) * (1. - ratio) + x * ratio,
            (anchor.1 - cy) * (1. - ratio) + y * ratio,
        ));
        frame.imp().zoom.set(value);
        frame.clamp_pan();
        self.zoom_reset.set_label(&format!("{:.0}%", value * 100.));
        self.zoom_reset.set_visible(value > 1.001);
    }
    pub(super) fn zoom_by(&self, factor: f64) {
        if let Some(frame) = self.zoom_frame() {
            self.zoom_to(frame.imp().zoom.get() * factor, None);
        }
    }
    pub(super) fn reset_zoom(&self) {
        for frame in self.frames.borrow().iter() {
            frame.imp().zoom.set(1.);
            frame.imp().pan.set((0., 0.));
            frame.queue_draw();
        }
        self.zoom_reset.set_visible(false);
    }
    pub(super) fn media_contains(&self, x: f64, y: f64) -> bool {
        let Some(frame) = self.zoom_frame() else {
            return false;
        };
        let Some(bounds) = frame.media_bounds() else {
            return false;
        };
        let Some(point) = self
            .root
            .compute_point(&frame, &graphene::Point::new(x as f32, y as f32))
        else {
            return false;
        };
        let (pan_x, pan_y) = frame.imp().pan.get();
        let zoom = frame.imp().zoom.get();
        (f64::from(point.x() - bounds.x() - bounds.width() / 2.) - pan_x).abs()
            <= f64::from(bounds.width()) * zoom / 2.
            && (f64::from(point.y() - bounds.y() - bounds.height() / 2.) - pan_y).abs()
                <= f64::from(bounds.height()) * zoom / 2.
    }
    pub(super) fn install_zoom(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.zoom_reset.connect_clicked(move |_| {
            if let Some(viewer) = weak.upgrade() {
                viewer.reset_zoom()
            }
        });
        let pointer = Rc::new(Cell::new(None));
        let motion = gtk::EventControllerMotion::new();
        let position = pointer.clone();
        motion.connect_motion(move |_, x, y| position.set(Some((x, y))));
        let position = pointer.clone();
        motion.connect_leave(move |_| position.set(None));
        self.root.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |controller, _, dy| {
            let Some(viewer) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !viewer.zoom_usable() || pointer.get().is_some_and(|(x, y)| viewer.controls_at(x, y))
            {
                return glib::Propagation::Proceed;
            }
            let rate = if controller.unit() == gdk::ScrollUnit::Surface {
                0.003
            } else {
                0.12
            };
            if let Some(frame) = viewer.zoom_frame() {
                viewer.zoom_to(
                    frame.imp().zoom.get() * (-dy.clamp(-100., 100.) * rate).exp(),
                    pointer.get(),
                );
            }
            glib::Propagation::Stop
        });
        self.root.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        pinch.set_propagation_phase(gtk::PropagationPhase::Capture);
        let initial = Rc::new(Cell::new(1.));
        let start = initial.clone();
        let weak = Rc::downgrade(self);
        pinch.connect_begin(move |gesture, _| {
            let Some(viewer) = weak.upgrade() else { return };
            if !viewer.zoom_usable()
                || gesture
                    .bounding_box_center()
                    .is_some_and(|(x, y)| viewer.controls_at(x, y))
            {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            if let Some(frame) = viewer.zoom_frame() {
                start.set(frame.imp().zoom.get());
            }
        });
        let weak = Rc::downgrade(self);
        pinch.connect_scale_changed(move |gesture, scale| {
            if let Some(viewer) = weak.upgrade() {
                viewer.zoom_to(initial.get() * scale, gesture.bounding_box_center());
            }
        });
        self.root.add_controller(pinch);

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pan = Rc::new(Cell::new(None));
        let start = pan.clone();
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |gesture, x, y| {
            start.set(None);
            let Some(viewer) = weak.upgrade() else { return };
            if viewer.zoom_usable() && viewer.is_zoomed() && !viewer.controls_at(x, y) {
                if let Some(frame) = viewer.zoom_frame() {
                    start.set(Some(frame.imp().pan.get()));
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            } else {
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |_, dx, dy| {
            if let (Some(viewer), Some((x, y))) = (weak.upgrade(), pan.get()) {
                if let Some(frame) = viewer.zoom_frame() {
                    frame.imp().pan.set((x + dx, y + dy));
                    frame.clamp_pan();
                }
            }
        });
        self.root.add_controller(drag);
    }
}
