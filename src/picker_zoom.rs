//! Hold still to preview; motion after opening browses other picker tiles.
use crate::ui::Ui;
use adw::prelude::*;
use gtk::{gdk, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
pub(crate) struct PickerZoom {
    pub root: gtk::Box,
    picture: gtk::Picture,
    active: Cell<bool>,
    binding: RefCell<Option<glib::Binding>>,
}
impl PickerZoom {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_can_target(false);
        root.set_visible(false);
        root.add_css_class("picker-zoom");
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_target(false);
        picture.set_size_request(240, 240);
        root.append(&picture);
        Rc::new(Self {
            root,
            picture,
            active: Cell::new(false),
            binding: RefCell::new(None),
        })
    }
    pub fn is_active(&self) -> bool {
        self.active.get()
    }
    fn show(&self, widget: &gtk::Widget) {
        if let Some(binding) = self.binding.borrow_mut().take() {
            binding.unbind();
        }
        fn source_picture(widget: &gtk::Widget) -> Option<gtk::Picture> {
            if let Some(picture) = widget
                .downcast_ref::<gtk::Picture>()
                .filter(|p| p.paintable().is_some())
            {
                return Some(picture.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                if let Some(p) = source_picture(&current) {
                    return Some(p);
                }
            }
            None
        }
        if let Some(source) = source_picture(widget) {
            self.binding.replace(Some(
                source
                    .bind_property("paintable", &self.picture, "paintable")
                    .sync_create()
                    .build(),
            ));
            self.active.set(true);
            self.root.set_visible(true);
            return;
        }

        fn artwork(widget: &gtk::Widget) -> Option<gdk::Paintable> {
            if let Some(picture) = widget.downcast_ref::<gtk::Picture>() {
                return picture.paintable();
            }
            if let Some(image) = widget.downcast_ref::<crate::widgets::AspectImage>() {
                return image.paintable();
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                if let Some(paintable) = artwork(&current) {
                    return Some(paintable);
                }
            }
            None
        }
        let content = widget.first_child().unwrap_or_else(|| widget.clone());
        self.picture.set_paintable(Some(
            &artwork(&content)
                .unwrap_or_else(|| gtk::WidgetPaintable::new(Some(&content)).upcast()),
        ));
        self.active.set(true);
        self.root.set_visible(true);
    }
    pub fn hide(&self) {
        if let Some(binding) = self.binding.borrow_mut().take() {
            binding.unbind();
        }
        self.active.set(false);
        self.root.set_visible(false);
        self.picture.set_paintable(gdk::Paintable::NONE);
    }
    pub fn install(self: &Rc<Self>, ui: &Rc<Ui>) {
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(ui);
        motion.connect_motion(move |controller, x, y| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.picker_zoom.is_active() {
                return;
            }
            if !controller
                .current_event_state()
                .contains(gdk::ModifierType::BUTTON1_MASK)
            {
                ui.picker_zoom.hide();
                return;
            }
            let Some(mut widget) = ui.window.pick(x, y, gtk::PickFlags::DEFAULT) else {
                return;
            };
            loop {
                if widget.has_css_class("picker-previewable") {
                    ui.picker_zoom.show(&widget);
                    return;
                }
                let Some(parent) = widget.parent() else {
                    return;
                };
                widget = parent;
            }
        });
        ui.window.add_controller(motion);
        let release = gtk::EventControllerLegacy::new();
        release.set_propagation_phase(gtk::PropagationPhase::Capture);
        let zoom = Rc::downgrade(self);
        release.connect_event(move |_, event| {
            if event.event_type() == gdk::EventType::ButtonRelease {
                if let Some(zoom) = zoom.upgrade() {
                    if zoom.is_active() {
                        zoom.hide();
                    }
                }
            }
            glib::Propagation::Proceed
        });
        ui.window.add_controller(release);
        let zoom = Rc::downgrade(self);
        ui.window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                if let Some(zoom) = zoom.upgrade() {
                    zoom.hide();
                }
            }
        });
    }
}
pub(crate) fn attach(ui: &Rc<Ui>, button: &gtk::Button, skip: Rc<Cell<bool>>) {
    button.add_css_class("picker-previewable");
    let press = gtk::GestureLongPress::new();
    press.set_button(1);
    press.set_delay_factor(0.7);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    let target = button.downgrade();
    press.connect_pressed(move |gesture, _, _| {
        if gesture
            .current_event_state()
            .contains(gdk::ModifierType::ALT_MASK)
        {
            return;
        }
        if let (Some(ui), Some(button)) = (weak.upgrade(), target.upgrade()) {
            skip.set(true);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            ui.picker_zoom.show(button.upcast_ref());
        }
    });
    button.add_controller(press);
    let weak = Rc::downgrade(ui);
    button.connect_unmap(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.picker_zoom.hide();
        }
    });
}
