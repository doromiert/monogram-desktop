//! Paint selection after a primary-button drag crosses message boundaries.
use super::Ui;
use adw::prelude::*;
use gtk::{gdk, glib};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

struct Brush {
    start: String,
    last_y: f64,
    pointer: Option<gtk::graphene::Point>,
    add: bool,
    active: bool,
    pressed_at: Instant,
    visited: HashSet<String>,
    timer: Option<glib::SourceId>,
}
impl Brush {
    fn stop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
    }
}
impl Drop for Brush {
    fn drop(&mut self) {
        self.stop();
    }
}
fn hide_drag_icon(drag: &gdk::Drag) {
    let hidden = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hidden.set_size_request(1, 1);
    hidden.set_opacity(0.0);
    gtk::DragIcon::for_drag(drag).set_child(Some(&hidden));
}
impl Ui {
    fn brush_row(&self, y: f64) -> Option<String> {
        let row = self.timeline.row_at_y(y as i32)?;
        row.has_css_class("message-row")
            .then(|| row.widget_name().to_string())
    }
    fn brush_control(&self, x: f64, y: f64) -> bool {
        let mut target = self.timeline.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = target {
            if widget.is::<gtk::Range>()
                || widget.is::<gtk::LinkButton>()
                || widget.is::<gtk::Entry>()
                || widget.is::<gtk::TextView>()
                || widget.has_css_class("round-video-progress")
                || widget.has_css_class("round-video-mute")
                || widget.has_css_class("reaction")
                || widget.has_css_class("media-download-action")
                || widget.has_css_class("poll-answer")
                || widget.has_css_class("link-preview")
                || widget.has_css_class("suggested-action")
            {
                return true;
            }
            if widget == self.timeline {
                break;
            }
            target = widget.parent();
        }
        false
    }
    fn paint_selection(&self, brush: &mut Brush, y: f64) {
        let range = brush.last_y.min(y)..=brush.last_y.max(y);
        let mut selected = self.selected.borrow_mut();
        let mut child = self.timeline.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            if !row.has_css_class("message-row") {
                continue;
            }
            let Some(bounds) = row.compute_bounds(&self.timeline) else {
                continue;
            };
            if f64::from(bounds.y()) <= *range.end()
                && f64::from(bounds.y() + bounds.height()) >= *range.start()
            {
                let id = row.widget_name().to_string();
                if brush.visited.insert(id.clone()) {
                    if brush.add {
                        selected.insert(id);
                    } else {
                        selected.remove(&id);
                    }
                }
            }
        }
        brush.last_y = y;
        drop(selected);
        self.update_selection();
    }
    fn brush_point(&self, event: &gdk::Event) -> Option<gtk::graphene::Point> {
        let native = self.timeline.native()?;
        if event.surface() != native.surface() {
            return None;
        }
        let (x, y) = event.position()?;
        let (dx, dy) = native.surface_transform();
        native.dynamic_cast::<gtk::Widget>().ok()?.compute_point(
            &self.timeline,
            &gtk::graphene::Point::new((x + dx) as f32, (y + dy) as f32),
        )
    }
    fn hide_brush_text_drag(&self, id: &str) -> Option<gdk::Drag> {
        let mut drag = None;
        let mut child = self.timeline.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            if row.widget_name() != id {
                continue;
            }
            let mut pending = vec![row];
            while let Some(widget) = pending.pop() {
                let controllers = widget.observe_controllers();
                for i in 0..controllers.n_items() {
                    if let Some(controller) = controllers.item(i) {
                        if let Ok(source) = controller.clone().downcast::<gtk::DragSource>() {
                            if let Some(active) = source.drag() {
                                hide_drag_icon(&active);
                                drag = Some(active);
                            }
                        }
                    }
                }
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    if label.is_selectable() {
                        label.select_region(0, 0);
                    }
                }
                let mut child = widget.first_child();
                while let Some(next) = child {
                    child = next.next_sibling();
                    pending.push(next);
                }
            }
            break;
        }
        drag
    }
    pub(super) fn install_brush_selection(self: &Rc<Self>) {
        let state = Rc::new(RefCell::new(None::<Brush>));
        let end_state = state.clone();
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        input.connect_event(move |_, event| {
            if std::env::var_os("MONOGRAM_SELECTION_TRACE").is_some() {
                eprintln!(
                    "Brush event {:?} surface-position={:?}",
                    event.event_type(),
                    event.position()
                );
            }
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let button = event
                .downcast_ref::<gdk::ButtonEvent>()
                .map(|event| event.button());
            let ending = event.event_type() == gdk::EventType::ButtonRelease
                && button == Some(gdk::BUTTON_PRIMARY)
                || matches!(
                    event.event_type(),
                    gdk::EventType::DropStart
                        | gdk::EventType::GrabBroken
                        | gdk::EventType::TouchCancel
                );
            if ending {
                let active = state.borrow_mut().take().is_some_and(|brush| brush.active);
                return if active && event.event_type() == gdk::EventType::DropStart {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                };
            }
            let Some(point) = ui.brush_point(event) else {
                return glib::Propagation::Proceed;
            };
            let (x, y) = (f64::from(point.x()), f64::from(point.y()));
            if std::env::var_os("MONOGRAM_SELECTION_TRACE").is_some() {
                eprintln!("Brush timeline={x},{y} row={:?}", ui.brush_row(y));
            }
            if event.event_type() == gdk::EventType::ButtonPress
                && button == Some(gdk::BUTTON_PRIMARY)
            {
                if ui.brush_control(x, y) {
                    return glib::Propagation::Proceed;
                }
                let Some(id) = ui.brush_row(y) else {
                    return glib::Propagation::Proceed;
                };
                let add = !ui.selected.borrow().contains(&id);
                state.replace(Some(Brush {
                    start: id,
                    last_y: y,
                    pointer: None,
                    add,
                    active: false,
                    pressed_at: Instant::now(),
                    visited: HashSet::new(),
                    timer: None,
                }));
                return glib::Propagation::Proceed;
            }
            let drag_motion = event.event_type() == gdk::EventType::DragMotion;
            if event.event_type() != gdk::EventType::MotionNotify && !drag_motion
                || !drag_motion
                    && !event
                        .modifier_state()
                        .contains(gdk::ModifierType::BUTTON1_MASK)
            {
                return glib::Propagation::Proceed;
            }
            let mut current = state.borrow_mut();
            let Some(current) = current.as_mut() else {
                return glib::Propagation::Proceed;
            };
            let crossed = ui.brush_row(y).is_some_and(|id| id != current.start);
            if !current.active && crossed {
                current.active = true;
                if std::env::var_os("MONOGRAM_SELECTION_TRACE").is_some() {
                    eprintln!(
                        "Brush activated in {}ms",
                        current.pressed_at.elapsed().as_millis()
                    );
                }
                let event_drag = event
                    .downcast_ref::<gdk::DNDEvent>()
                    .and_then(|event| event.drop())
                    .and_then(|drop| drop.drag());
                let source_drag = ui.hide_brush_text_drag(&current.start);
                if let Some(drag) = event_drag.as_ref() {
                    hide_drag_icon(drag);
                }
                if let Some(drag) = event_drag.or(source_drag) {
                    let hidden_drag = drag.clone();
                    let hidden_state = state.clone();
                    let hidden_start = current.start.clone();
                    glib::idle_add_local_once(move || {
                        if hidden_state
                            .borrow()
                            .as_ref()
                            .is_some_and(|brush| brush.active && brush.start == hidden_start)
                        {
                            hide_drag_icon(&hidden_drag);
                        }
                    });
                    let finished = state.clone();
                    drag.connect_dnd_finished(move |_| {
                        finished.borrow_mut().take();
                    });
                    let cancelled = state.clone();
                    drag.connect_cancel(move |_, _| {
                        cancelled.borrow_mut().take();
                    });
                }
            }
            if !current.active {
                return glib::Propagation::Proceed;
            }
            current.pointer = ui.timeline.compute_point(&ui.scroll, &point);
            ui.paint_selection(current, y);
            if current.timer.is_none() {
                let weak = Rc::downgrade(&ui);
                let state = state.clone();
                current.timer = Some(glib::timeout_add_local(
                    Duration::from_millis(16),
                    move || {
                        let Some(ui) = weak.upgrade() else {
                            if let Some(brush) = state.borrow_mut().as_mut() {
                                brush.timer.take();
                            }
                            return glib::ControlFlow::Break;
                        };
                        let mut state = state.borrow_mut();
                        let Some(brush) = state.as_mut() else {
                            return glib::ControlFlow::Break;
                        };
                        let Some(pointer) = brush.pointer else {
                            return glib::ControlFlow::Continue;
                        };
                        let height = ui.scroll.height() as f32;
                        let direction = if pointer.y() < 32. {
                            -1.
                        } else if pointer.y() > height - 32. {
                            1.
                        } else {
                            0.
                        };
                        if direction != 0. {
                            let a = ui.scroll.vadjustment();
                            let limit = (a.upper() - a.page_size()).max(0.);
                            a.set_value((a.value() + direction * 18.).clamp(0., limit));
                            if let Some(point) = ui.scroll.compute_point(&ui.timeline, &pointer) {
                                ui.paint_selection(brush, f64::from(point.y()));
                            }
                        }
                        glib::ControlFlow::Continue
                    },
                ));
            }
            glib::Propagation::Stop
        });
        let end = gtk::EventControllerLegacy::new();
        end.set_propagation_phase(gtk::PropagationPhase::Capture);
        end.connect_event(move |_, event| {
            let primary_release = event.event_type() == gdk::EventType::ButtonRelease
                && event
                    .downcast_ref::<gdk::ButtonEvent>()
                    .is_some_and(|event| event.button() == gdk::BUTTON_PRIMARY);
            if primary_release || event.event_type() == gdk::EventType::DropStart {
                let active = end_state
                    .borrow_mut()
                    .take()
                    .is_some_and(|brush| brush.active);
                if active && event.event_type() == gdk::EventType::DropStart {
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(end);
        self.timeline.add_controller(input);
    }
}
