use gtk::{glib, graphene, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

struct Tab {
    widget: gtk::Widget,
    shift: f32,
    target: f32,
}

struct Drag {
    button: gtk::ToggleButton,
    tabs: Vec<Tab>,
    from: usize,
    to: usize,
    start: f32,
    left: f32,
    active: bool,
}

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct FolderBar {
        pub(super) drag: RefCell<Option<Drag>>,
        pub(super) ticking: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for FolderBar {
        const NAME: &'static str = "MonogramFolderBar";
        type Type = super::FolderBar;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for FolderBar {}
    impl WidgetImpl for FolderBar {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let drag = self.drag.borrow();
            let Some(drag) = drag.as_ref().filter(|drag| drag.active) else {
                self.parent_snapshot(snapshot);
                return;
            };
            for (index, tab) in drag.tabs.iter().enumerate() {
                if index != drag.from && tab.widget.parent().as_ref() == Some(obj.upcast_ref()) {
                    obj.snapshot_tab(&tab.widget, snapshot, tab.shift);
                }
            }
            if let Some(bounds) = drag.button.compute_bounds(&*obj) {
                obj.snapshot_tab(&drag.button, snapshot, drag.left - bounds.x());
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
        let gesture = gtk::GestureDrag::new();
        gesture.set_button(1);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);

        let bar = self.downgrade();
        let button_ref = button.downgrade();
        gesture.connect_drag_begin(move |gesture, _, _| {
            if let (Some(bar), Some(button)) = (bar.upgrade(), button_ref.upgrade())
                && bar.begin_drag(button)
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });

        let bar = self.downgrade();
        gesture.connect_drag_update(move |_, dx, _| {
            if let Some(bar) = bar.upgrade() {
                bar.update_drag(dx as f32);
            }
        });

        let bar = self.downgrade();
        gesture.connect_drag_end(move |_, dx, _| {
            if let Some(bar) = bar.upgrade() {
                if bar
                    .imp()
                    .drag
                    .borrow()
                    .as_ref()
                    .is_some_and(|drag| drag.active)
                {
                    bar.update_drag(dx as f32);
                }
                let Some(drag) = bar.finish_drag() else {
                    return;
                };
                if !drag.active {
                    drag.button.set_active(true);
                } else if drag.from != drag.to {
                    reorder(drag.button.label().as_deref().unwrap_or(""), drag.to);
                }
            }
        });

        let bar = self.downgrade();
        gesture.connect_cancel(move |_, _| {
            if let Some(bar) = bar.upgrade() {
                bar.finish_drag();
            }
        });
        button.add_controller(gesture);
    }

    fn begin_drag(&self, button: gtk::ToggleButton) -> bool {
        let Some(bounds) = button.compute_bounds(self) else {
            return false;
        };
        if self.imp().drag.borrow().is_some() {
            return false;
        }
        let mut tabs = Vec::new();
        let mut child = self.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            tabs.push(Tab {
                widget,
                shift: 0.,
                target: 0.,
            });
        }
        let Some(from) = tabs
            .iter()
            .position(|tab| tab.widget == *button.upcast_ref::<gtk::Widget>())
        else {
            return false;
        };
        self.imp().drag.replace(Some(Drag {
            button,
            tabs,
            from,
            to: from,
            start: bounds.x(),
            left: bounds.x(),
            active: false,
        }));
        true
    }

    fn update_drag(&self, dx: f32) {
        let mut state = self.imp().drag.borrow_mut();
        let Some(drag) = state.as_mut() else { return };
        if !drag.active {
            if dx.abs() < 8. {
                return;
            }
            drag.active = true;
            drag.button.add_css_class("dragging-folder");
        }
        // The child stays at its original allocation, so gesture deltas stay in one frame of reference.
        drag.left = drag.start + dx;
        let Some(bounds) = drag.button.compute_bounds(self) else {
            return;
        };
        let center = drag.left + bounds.width() / 2.;
        let direction = if self.direction() == gtk::TextDirection::Rtl {
            -1.
        } else {
            1.
        };
        let target = drag
            .tabs
            .iter()
            .enumerate()
            .filter(|(index, tab)| {
                *index != drag.from
                    && tab
                        .widget
                        .compute_bounds(self)
                        .is_some_and(|r| direction * (center - (r.x() + r.width() / 2.)) >= 0.)
            })
            .count()
            .clamp(1, drag.tabs.len().saturating_sub(1).max(1));
        if target != drag.to {
            drag.to = target;
            let extent = direction * (bounds.width() + self.spacing() as f32);
            let animate = self.settings().property::<bool>("gtk-enable-animations");
            for (index, tab) in drag.tabs.iter_mut().enumerate() {
                tab.target = if drag.from < index && index <= target {
                    -extent
                } else if target <= index && index < drag.from {
                    extent
                } else {
                    0.
                };
                if !animate {
                    tab.shift = tab.target;
                }
            }
            if animate {
                self.animate_tabs();
            }
        }
        self.queue_draw();
    }

    fn animate_tabs(&self) {
        if self.imp().ticking.replace(true) {
            return;
        }
        let last = Cell::new(0);
        self.add_tick_callback(move |bar, clock| {
            let now = clock.frame_time();
            let delta = now - last.replace(now);
            let amount = (delta as f32 / 85_000.).clamp(0.16, 0.45);
            let mut moving = false;
            if let Some(drag) = bar.imp().drag.borrow_mut().as_mut() {
                for tab in &mut drag.tabs {
                    let distance = tab.target - tab.shift;
                    if distance.abs() > 0.5 {
                        tab.shift += distance * amount;
                        moving = true;
                    } else {
                        tab.shift = tab.target;
                    }
                }
                bar.queue_draw();
            }
            if moving {
                glib::ControlFlow::Continue
            } else {
                bar.imp().ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    fn finish_drag(&self) -> Option<Drag> {
        let drag = self.imp().drag.take()?;
        drag.button.remove_css_class("dragging-folder");
        self.queue_draw();
        Some(drag)
    }

    fn snapshot_tab(&self, tab: &impl IsA<gtk::Widget>, snapshot: &gtk::Snapshot, shift: f32) {
        snapshot.save();
        snapshot.translate(&graphene::Point::new(shift, 0.));
        self.snapshot_child(tab, snapshot);
        snapshot.restore();
    }
}
