use gtk::{glib, graphene, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
mod imp {
    use super::*;
    #[derive(Default)]
    pub struct FolderBar {
        pub dragged: RefCell<Option<gtk::Button>>,
        pub offset: Cell<f32>,
        pub dragging: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for FolderBar {
        const NAME: &'static str = "MonogramFolderBar";
        type Type = super::FolderBar;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for FolderBar {}
    impl WidgetImpl for FolderBar {
        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let dragged = self.dragged.borrow();
            let mut child = obj.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                if !self.dragging.get()
                    || dragged
                        .as_ref()
                        .is_none_or(|b| b.upcast_ref::<gtk::Widget>() != &c)
                {
                    obj.snapshot_child(&c, s)
                }
            }
            if self.dragging.get() {
                if let Some(ref b) = *dragged {
                    if let Some(rect) = b.compute_bounds(&*obj) {
                        s.save();
                        s.translate(&graphene::Point::new(self.offset.get() - rect.x(), 0.));
                        obj.snapshot_child(b, s);
                        s.restore()
                    }
                }
            }
        }
    }
    impl BoxImpl for FolderBar {}
}
glib::wrapper! {pub struct FolderBar(ObjectSubclass<imp::FolderBar>) @extends gtk::Box,gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget,gtk::Orientable;}
impl FolderBar {
    pub fn new() -> Self {
        let bar: Self = glib::Object::new();
        bar.set_spacing(4);
        bar.add_css_class("folder-bar");
        bar
    }
    pub fn bind(&self, button: &gtk::ToggleButton, reorder: impl Fn(&str, usize) + 'static) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let origin = Rc::new(Cell::new(0f32));
        let grabbed = Rc::new(Cell::new(0f32));
        let bar = self.downgrade();
        let b = button.downgrade();
        let o = origin.clone();
        let g = grabbed.clone();
        drag.connect_drag_begin(move |gesture, x, _| {
            if let (Some(bar), Some(b)) = (bar.upgrade(), b.upgrade()) {
                o.set(b.compute_bounds(&bar).map(|r| r.x()).unwrap_or(0.));
                g.set(x as f32);
                bar.imp().dragged.replace(Some(b.upcast()));
                bar.imp().dragging.set(false);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        let bar = self.downgrade();
        let b = button.downgrade();
        let o = origin.clone();
        drag.connect_drag_update(move |_, dx, _| {
            if let (Some(bar), Some(b)) = (bar.upgrade(), b.upgrade()) {
                if !bar.imp().dragging.get() && dx.abs() < 8. {
                    return;
                }
                bar.imp().dragging.set(true);
                let pointer = o.get() + grabbed.get() + dx as f32;
                let mut index = 0;
                let mut child = bar.first_child();
                while let Some(c) = child {
                    child = c.next_sibling();
                    if c != b.clone().upcast::<gtk::Widget>() {
                        if let Some(r) = c.compute_bounds(&bar) {
                            if pointer >= r.x() + r.width() / 2. {
                                index += 1
                            }
                        }
                    }
                }
                reorder(b.label().as_deref().unwrap_or(""), index);
                bar.imp()
                    .offset
                    .set((o.get() + dx as f32).clamp(0., (bar.width() - b.width()).max(0) as f32));
                bar.queue_draw();
            }
        });
        let bar = self.downgrade();
        let b = button.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let (Some(bar), Some(b)) = (bar.upgrade(), b.upgrade()) {
                if !bar.imp().dragging.get() {
                    b.set_active(true)
                }
                bar.imp().dragged.take();
                bar.imp().dragging.set(false);
                bar.queue_draw();
            }
        });
        let bar = self.downgrade();
        drag.connect_cancel(move |_, _| {
            if let Some(bar) = bar.upgrade() {
                bar.imp().dragged.take();
                bar.imp().dragging.set(false);
                bar.queue_draw();
            }
        });
        button.add_controller(drag);
    }
}
