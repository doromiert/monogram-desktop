//! Composer-to-bubble presentation, following mobile TextMessageEnterTransition (250ms).
use crate::ui::Ui;
use adw::prelude::*;
use gtk::{glib, graphene, gsk, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

const DURATION: f64 = 250_000.;
const MAX_FLIGHTS: usize = 4;

mod target_imp {
    use super::*;
    #[derive(Default)]
    pub struct Target {
        pub child: RefCell<Option<gtk::Widget>>,
        pub hidden: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Target {
        const NAME: &'static str = "MonogramSendTarget";
        type Type = super::Target;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Target {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for Target {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, o: gtk::Orientation, s: i32) -> (i32, i32, i32, i32) {
            self.child
                .borrow()
                .as_ref()
                .map_or((0, 0, -1, -1), |c| c.measure(o, s))
        }
        fn size_allocate(&self, w: i32, h: i32, b: i32) {
            if let Some(c) = self.child.borrow().as_ref() {
                c.allocate(w, h, b, None);
            }
        }
        fn snapshot(&self, s: &gtk::Snapshot) {
            if !self.hidden.get() {
                if let Some(c) = self.child.borrow().as_ref() {
                    self.obj().snapshot_child(c, s);
                }
            }
        }
    }
}
glib::wrapper! {pub struct Target(ObjectSubclass<target_imp::Target>) @extends gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
impl Target {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let target: Self = glib::Object::new();
        let child = child.upcast_ref::<gtk::Widget>();
        child.set_parent(&target);
        target.imp().child.replace(Some(child.clone()));
        target.imp().hidden.set(true);
        target
    }
    fn reveal(&self) {
        self.imp().hidden.set(false);
        self.queue_draw();
    }
    fn node(&self) -> Option<gsk::RenderNode> {
        let snapshot = gtk::Snapshot::new();
        self.snapshot_child(self.imp().child.borrow().as_ref()?, &snapshot);
        snapshot.to_node()
    }
}

struct Flight {
    id: String,
    scope: (String, String, u64),
    source: gsk::RenderNode,
    from: graphene::Rect,
    target: glib::WeakRef<Target>,
    to: Option<(gsk::RenderNode, graphene::Rect)>,
    queued: Instant,
    started: Option<i64>,
    progress: f64,
    settle: u8,
    viewport: (i32, i32),
}
impl Drop for Flight {
    fn drop(&mut self) {
        if let Some(target) = self.target.upgrade() {
            target.reveal();
        }
    }
}

mod layer_imp {
    use super::*;
    #[derive(Default)]
    pub struct Layer {
        pub(super) flights: RefCell<Vec<Flight>>,
        pub ticking: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Layer {
        const NAME: &'static str = "MonogramSendAnimation";
        type Type = super::SendAnimation;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Layer {
        fn dispose(&self) {
            self.flights.borrow_mut().clear();
        }
    }
    impl WidgetImpl for Layer {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            for flight in self.flights.borrow().iter() {
                draw(snapshot, flight);
            }
        }
    }
}
glib::wrapper! {pub struct SendAnimation(ObjectSubclass<layer_imp::Layer>) @extends gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
impl SendAnimation {
    pub fn new() -> Self {
        let layer: Self = glib::Object::new();
        layer.set_can_target(false);
        layer.set_focusable(false);
        layer.set_hexpand(true);
        layer.set_vexpand(true);
        layer.update_state(&[gtk::accessible::State::Hidden(true)]);
        layer
    }
    pub fn cancel(&self) {
        self.imp().flights.borrow_mut().clear();
        self.queue_draw();
    }
    pub fn wrap(&self, id: &str, bubble: &impl IsA<gtk::Widget>) -> gtk::Widget {
        let mut flights = self.imp().flights.borrow_mut();
        let Some(flight) = flights.iter_mut().find(|f| f.id == id) else {
            return bubble.clone().upcast();
        };
        if let Some(old) = flight.target.upgrade() {
            old.reveal();
        }
        let target = Target::new(bubble);
        flight.target = target.downgrade();
        flight.to = None;
        flight.settle = 2;
        target.upcast()
    }
    fn enqueue(&self, ui: &Rc<Ui>, id: &str) {
        if !animations_enabled(self) || !ui.composer.is_mapped() || !self.is_mapped() {
            return;
        }
        let Some((source, from)) = composer_snapshot(&ui.composer, self) else {
            return;
        };
        let state = ui.state.borrow();
        let scope = (
            state.account.clone(),
            state.current().jid.clone(),
            ui.generation.get(),
        );
        drop(state);
        let mut flights = self.imp().flights.borrow_mut();
        if flights.len() >= MAX_FLIGHTS {
            flights.remove(0);
        }
        flights.push(Flight {
            id: id.into(),
            scope,
            source,
            from,
            target: Default::default(),
            to: None,
            queued: Instant::now(),
            started: None,
            progress: 0.,
            settle: 2,
            viewport: (self.width(), self.height()),
        });
        drop(flights);
        ui.set_composer_send_hint(true);
        self.queue_draw();
        if self.imp().ticking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(ui);
        self.add_tick_callback(move |layer, clock| {
            let Some(ui) = weak.upgrade() else {
                layer.cancel();
                layer.imp().ticking.set(false);
                return glib::ControlFlow::Break;
            };
            layer.advance(&ui, clock.frame_time());
            if layer.imp().flights.borrow().is_empty() {
                layer.imp().ticking.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    fn advance(&self, ui: &Ui, now: i64) {
        let state = ui.state.borrow();
        let scope = (
            state.account.clone(),
            state.current().jid.clone(),
            ui.generation.get(),
        );
        drop(state);
        let enabled = animations_enabled(self)
            && ui.pages.visible_child_name().as_deref() == Some("chat")
            && ui.active_media.borrow().is_none()
            && ui.window.visible_dialog().is_none();
        self.imp().flights.borrow_mut().retain_mut(|f| {
            if !enabled || f.scope != scope || f.viewport != (self.width(), self.height()) {
                return false;
            }
            if f.started.is_none() && f.queued.elapsed() > Duration::from_millis(400) {
                return false;
            }
            let Some(target) = f.target.upgrade() else {
                return f.started.is_none();
            };
            if !target.is_mapped() || target.width() <= 0 || target.height() <= 0 {
                return f.started.is_none();
            }
            if f.settle > 0 {
                f.settle -= 1;
                return true;
            }
            let Some(bounds) = target.compute_bounds(self) else {
                return false;
            };
            let Some(viewport) = ui.scroll.compute_bounds(self) else {
                return false;
            };
            if bounds.y() + bounds.height() <= viewport.y()
                || bounds.y() >= viewport.y() + viewport.height()
            {
                return false;
            }
            if f.to.is_none() {
                let Some(node) = target.node() else {
                    return true;
                };
                f.to = Some((node, bounds));
            }
            if let Some((_, to)) = f.to.as_mut() {
                *to = bounds;
            }
            let start = *f.started.get_or_insert(now);
            f.progress = ((now - start) as f64 / DURATION).clamp(0., 1.);
            f.progress < 1.
        });
        ui.set_composer_send_hint(self.imp().flights.borrow().iter().any(|f| f.progress < 0.4));
        self.queue_draw();
    }
}
fn animations_enabled(widget: &impl IsA<gtk::Widget>) -> bool {
    widget.settings().is_gtk_enable_animations()
}

fn composer_snapshot(
    view: &gtk::TextView,
    layer: &SendAnimation,
) -> Option<(gsk::RenderNode, graphene::Rect)> {
    let buffer = view.buffer();
    if buffer.char_count() > 4096 {
        return None;
    }
    let mut line = buffer.start_iter();
    let mut region = None::<graphene::Rect>;
    for count in 0..=10 {
        if count == 10 {
            return None;
        }
        let mut end = line;
        view.forward_display_line_end(&mut end);
        let (a, b) = (view.iter_location(&line), view.iter_location(&end));
        let (x, y) =
            view.buffer_to_window_coords(gtk::TextWindowType::Widget, a.x().min(b.x()), a.y());
        let rect = graphene::Rect::new(
            x as f32,
            y as f32,
            (a.x() - b.x()).abs().max(1) as f32,
            a.height().max(b.height()) as f32,
        );
        region = Some(region.map_or(rect, |r| r.union(&rect)));
        if end.is_end() {
            break;
        }
        line = end;
        if !line.forward_char() {
            break;
        }
    }
    let view_rect = graphene::Rect::new(0., 0., view.width() as f32, view.height() as f32);
    let crop = region?.intersection(&view_rect)?;
    if crop.width() <= 0. || crop.height() <= 0. {
        return None;
    }
    let snapshot = gtk::Snapshot::new();
    snapshot.translate(&graphene::Point::new(-crop.x(), -crop.y()));
    snapshot.push_clip(&crop);
    gtk::WidgetPaintable::new(Some(view)).snapshot(
        &snapshot,
        view.width() as f64,
        view.height() as f64,
    );
    snapshot.pop();
    let origin = view.compute_point(layer, &graphene::Point::new(crop.x(), crop.y()))?;
    Some((
        snapshot.to_node()?,
        graphene::Rect::new(origin.x(), origin.y(), crop.width(), crop.height()),
    ))
}
fn draw(snapshot: &gtk::Snapshot, f: &Flight) {
    let Some((node, to)) = f.to.as_ref() else {
        snapshot.save();
        snapshot.translate(&graphene::Point::new(f.from.x(), f.from.y()));
        snapshot.append_node(&f.source);
        snapshot.restore();
        return;
    };
    let p = f.progress;
    let y = adw::Easing::EaseOutCubic.ease(p) as f32;
    let x = adw::Easing::EaseOutQuint.ease(p) as f32;
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let rect = graphene::Rect::new(
        mix(f.from.x() - 12., to.x(), x),
        mix(f.from.y() - 8., to.y(), y),
        mix(f.from.width() + 24., to.width(), x),
        mix(f.from.height() + 16., to.height(), y),
    );
    snapshot.save();
    snapshot.translate(&graphene::Point::new(rect.x(), rect.y()));
    snapshot.push_clip(&graphene::Rect::new(0., 0., rect.width(), rect.height()));
    let alpha = (p / 0.4).clamp(0., 1.);
    if alpha > 0. {
        snapshot.push_opacity(alpha);
        snapshot.save();
        snapshot.scale(
            rect.width() / to.width().max(1.),
            rect.height() / to.height().max(1.),
        );
        snapshot.append_node(node);
        snapshot.restore();
        snapshot.pop();
    }
    if alpha < 1. {
        snapshot.push_opacity(1. - alpha);
        snapshot.save();
        snapshot.translate(&graphene::Point::new(12., 8.));
        snapshot.append_node(&f.source);
        snapshot.restore();
        snapshot.pop();
    }
    snapshot.pop();
    snapshot.restore();
}
impl Ui {
    pub(crate) fn animate_sent_text(self: &Rc<Self>, id: &str) {
        self.send_animation.enqueue(self, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Message, State},
        widgets::asset,
    };
    #[test]
    #[ignore = "requires private X11, data roots and Secret Service"]
    fn native_send_flight_context_reduced_motion_and_rapid_sends() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramSendTransitionTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_path(asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut state = State::live("", vec![]);
        let second = state.chat_for("second@fixture.invalid").id.clone();
        let first = state.chat_for("first@fixture.invalid").id.clone();
        state.active = first;
        for i in 0..20 {
            state.current_mut().messages.push(Message {
                id: format!("history-{i}"),
                text: format!("A previous message in this private animation fixture, number {i}."),
                kind: "text".into(),
                author: "Peer".into(),
                time: "12:30".into(),
                ..Default::default()
            });
        }
        ui.state.replace(state);
        ui.load_current();
        ui.window.set_default_size(1000, 800);
        ui.window.present();
        let samples = Rc::new(RefCell::new(Vec::<(i64, f64)>::new()));
        let textures = Rc::new(RefCell::new(Vec::new()));
        let target = Rc::new(RefCell::new(None::<glib::WeakRef<Target>>));
        glib::MainContext::default().block_on(async{
            glib::timeout_future(Duration::from_millis(450)).await;
            let text="This text travels from the composer into its message bubble.";ui.composer.buffer().set_text(text);ui.composer.grab_focus();glib::timeout_future(Duration::from_millis(60)).await;
            let start=Instant::now();let id=ui.queue_formatted_text(text,None).expect("accepted local send");assert!(ui.composer_text().is_empty());assert_eq!(ui.send_animation.imp().flights.borrow().len(),1,"no composer snapshot");
            let sample=samples.clone();let captured=textures.clone();let observed=target.clone();let layer=ui.send_animation.downgrade();let window=ui.window.downgrade();
            let capture_clock=ui.window.frame_clock().unwrap();let callback=capture_clock.connect_after_paint(move|clock|{if let Some(layer)=layer.upgrade(){let flights=layer.imp().flights.borrow();if let Some(f)=flights.iter().find(|f|f.id==id){if let Some((_,to))=&f.to{assert!(f.from.y()>to.y(),"message did not rise from composer");assert!(f.target.upgrade().unwrap().imp().hidden.get());observed.replace(Some(f.target.clone()));sample.borrow_mut().push((clock.frame_time(),f.progress));if std::env::var_os("MONOGRAM_SEND_CAPTURE").is_some() && captured.borrow().len()<7 && f.progress>=captured.borrow().len() as f64/7.{if let Some(window)=window.upgrade(){let snapshot=gtk::Snapshot::new();gtk::WidgetPaintable::new(Some(&window)).snapshot(&snapshot,window.width() as f64,window.height() as f64);if let Some(node)=snapshot.to_node(){captured.borrow_mut().push(window.renderer().unwrap().render_texture(&node,None));}}}}}}});
            glib::timeout_future(Duration::from_millis(390)).await;capture_clock.disconnect(callback);assert!(ui.send_animation.imp().flights.borrow().is_empty(),"flight did not finish");assert!(start.elapsed()<Duration::from_millis(450));
            let frames=samples.borrow();assert!(frames.len()>=5,"too few animation frames: {}",frames.len());let max_gap=frames.windows(2).map(|w|(w[1].0-w[0].0)/1000).max().unwrap();assert!(max_gap<100,"send animation stalled {max_gap}ms");assert!(frames.windows(2).all(|w|w[1].1>=w[0].1));drop(frames);
            assert!(!target.borrow().as_ref().and_then(|t|t.upgrade()).unwrap().imp().hidden.get(),"finished target stayed hidden");
            if let Some(prefix)=std::env::var_os("MONOGRAM_SEND_CAPTURE"){assert!(textures.borrow().len()>=4,"missing recorded animation frames");for(i,image)in textures.borrow().iter().enumerate(){image.save_to_png(format!("{}-{i}.png",prefix.to_string_lossy())).unwrap();}}
            for i in 0..5{let text=format!("Rapid message {i}");ui.composer.buffer().set_text(&text);glib::timeout_future(Duration::from_millis(20)).await;ui.queue_text(&text);assert!(ui.send_animation.imp().flights.borrow().len()<=MAX_FLIGHTS);}
            let draining=Instant::now();while !ui.send_animation.imp().flights.borrow().is_empty() && draining.elapsed()<Duration::from_millis(700){glib::timeout_future(Duration::from_millis(20)).await;}assert!(ui.send_animation.imp().flights.borrow().is_empty(),"rapid-send flights did not finish");
            ui.composer.settings().set_gtk_enable_animations(false);ui.composer.buffer().set_text("Reduced motion send");glib::timeout_future(Duration::from_millis(20)).await;ui.queue_text("Reduced motion send");assert!(ui.send_animation.imp().flights.borrow().is_empty());ui.composer.settings().set_gtk_enable_animations(true);
            ui.composer.buffer().set_text("Switch conversations during flight");glib::timeout_future(Duration::from_millis(20)).await;ui.queue_text("Switch conversations during flight");assert!(!ui.send_animation.imp().flights.borrow().is_empty());ui.select_chat(&second);assert!(ui.send_animation.imp().flights.borrow().is_empty());
            println!("PASS composer-to-bubble flight: {} frames, maximum frame gap {max_gap}ms; durable send, rapid sends, reduced motion and context cleanup",samples.borrow().len());
        });
        ui.window.close();
    }
}
