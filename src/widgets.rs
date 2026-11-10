use gtk::{gdk, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
};
pub fn asset(name: &str) -> PathBuf {
    std::env::var_os("MONOGRAM_ASSET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"))
        .join(name)
}
pub fn bx(vertical: bool, spacing: i32) -> gtk::Box {
    gtk::Box::new(
        if vertical {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        },
        spacing,
    )
}
pub fn margins(w: &impl IsA<gtk::Widget>, v: i32) {
    w.set_margin_top(v);
    w.set_margin_bottom(v);
    w.set_margin_start(v);
    w.set_margin_end(v)
}
pub fn label(text: &str, css: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    for c in css.split_whitespace() {
        l.add_css_class(c)
    }
    l
}
pub fn text(text: &str, css: &str) -> gtk::Label {
    let l = label(text, css);
    l.set_xalign(0.);
    l
}
pub fn wrap(text: &str, css: &str) -> gtk::Label {
    let l = self::text(text, css);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.set_max_width_chars(52);
    l
}
pub fn icon_button(icon: &str, title: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(title));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}
pub fn avatar(chat: &crate::model::Chat, size: i32) -> adw::Avatar {
    let a = adw::Avatar::new(size, Some(&chat.name), true);
    a.add_css_class(&format!("avatar-{}", chat.color));
    if chat.id == "saved" {
        a.set_show_initials(false);
        a.set_icon_name(Some("user-bookmarks-symbolic"))
    }
    a
}
pub fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build()
}
pub fn clear(w: &gtk::Box) {
    while let Some(c) = w.first_child() {
        w.remove(&c)
    }
}
pub fn clear_list(w: &gtk::ListBox) {
    while let Some(c) = w.first_child() {
        w.remove(&c)
    }
}
/// Native horizontal scrolling, including a vertical mouse wheel on strips.
pub fn horizontal_strip(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build();
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = scroll.downgrade();
    controller.connect_scroll(move |controller, dx, dy| {
        let Some(scroll) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let adjustment = scroll.hadjustment();
        let delta = if dx.abs() > dy.abs() { dx } else { dy };
        let step = if controller.unit() == gdk::ScrollUnit::Wheel {
            adjustment.page_size().powf(2. / 3.)
        } else {
            1.
        };
        let limit = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value((adjustment.value() + delta * step).clamp(adjustment.lower(), limit));
        glib::Propagation::Stop
    });
    scroll.add_controller(controller);
    scroll
}
mod image_imp {
    use super::*;
    #[derive(Default)]
    pub struct AspectImage {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub ratio: Cell<f64>,
        pub cover: Cell<bool>,
        pub max_width: Cell<i32>,
        pub borderless: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for AspectImage {
        const NAME: &'static str = "MonogramAspectImage";
        type Type = super::AspectImage;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for AspectImage {}
    impl WidgetImpl for AspectImage {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, o: gtk::Orientation, size: i32) -> (i32, i32, i32, i32) {
            if self.cover.get() {
                return match o {
                    gtk::Orientation::Horizontal => (0, 110, -1, -1),
                    _ => {
                        let edge = if size < 0 { 110 } else { size };
                        (edge, edge, -1, -1)
                    }
                };
            }
            let width = self.max_width.get().max(1);
            if o == gtk::Orientation::Horizontal {
                (0, width, -1, -1)
            } else {
                // ListBox measures a start-aligned row against the entire viewport.
                // Its photo is at most its natural width, even in a wide window.
                let h = ((if size >= 0 { size.min(width) } else { width }) as f64
                    / self.ratio.get().max(0.01))
                .round() as i32;
                (if size >= 0 { h } else { 0 }, h, -1, -1)
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f64;
            let h = obj.height() as f64;
            if let Some(ref p) = *self.paintable.borrow() {
                let bounds = gtk::graphene::Rect::new(0., 0., w as f32, h as f32);
                if !self.borderless.get() {
                    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(bounds, 10.));
                }
                if self.cover.get() {
                    let ratio = p.intrinsic_aspect_ratio();
                    let (width, height) = if ratio <= 0. {
                        (w, h)
                    } else {
                        (w.max(h * ratio), h.max(w / ratio))
                    };
                    snapshot.save();
                    snapshot.translate(&gtk::graphene::Point::new(
                        ((w - width) / 2.) as f32,
                        ((h - height) / 2.) as f32,
                    ));
                    p.snapshot(snapshot, width, height);
                    snapshot.restore();
                } else {
                    p.snapshot(snapshot, w, h);
                }
                if !self.borderless.get() {
                    snapshot.pop();
                }
            }
        }
    }
}
glib::wrapper! {pub struct AspectImage(ObjectSubclass<image_imp::AspectImage>) @extends gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
impl AspectImage {
    pub fn square() -> Self {
        let image: Self = glib::Object::new();
        image.imp().cover.set(true);
        image
    }
    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.imp().paintable.borrow().clone()
    }
    pub fn set_paintable(&self, paintable: Option<gdk::Paintable>) {
        if !self.imp().cover.get()
            && let Some(image) = paintable.as_ref()
        {
            let ratio = image.intrinsic_aspect_ratio().max(0.01);
            if (self.imp().ratio.replace(ratio) - ratio).abs() > 0.001 {
                self.queue_resize();
            }
        }
        self.imp().paintable.replace(paintable);
        self.queue_draw();
    }
    pub fn thumbnail(width: i32, borderless: bool) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().max_width.set(width);
        obj.imp().borderless.set(borderless);
        obj.imp().ratio.set(1.);
        obj
    }
    pub fn new(path: &Path) -> Self {
        let obj = Self::thumbnail(420, false);
        let source = gtk::Picture::for_filename(path);
        if let Some(p) = source.paintable() {
            let ratio = p.intrinsic_aspect_ratio();
            obj.imp().ratio.set(if ratio > 0. { ratio } else { 2. });
            obj.imp().paintable.replace(Some(p));
        } else {
            obj.imp().ratio.set(2.)
        }
        obj.set_vexpand(false);
        obj.set_valign(gtk::Align::Start);
        obj.set_halign(gtk::Align::Center);
        obj
    }
}

mod gif_imp {
    use super::*;
    #[derive(Default)]
    pub struct AnimatedImage {
        pub frame: RefCell<Option<gtk::gdk_pixbuf::PixbufAnimationIter>>,
        pub timer: RefCell<Option<glib::SourceId>>,
        pub ratio: Cell<f64>,
        pub max_width: Cell<i32>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for AnimatedImage {
        const NAME: &'static str = "MonogramAnimatedImage";
        type Type = super::AnimatedImage;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for AnimatedImage {
        fn dispose(&self) {
            if let Some(id) = self.timer.borrow_mut().take() {
                id.remove()
            }
        }
    }
    impl WidgetImpl for AnimatedImage {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, o: gtk::Orientation, size: i32) -> (i32, i32, i32, i32) {
            if o == gtk::Orientation::Horizontal {
                (0, 160, -1, -1)
            } else {
                let mut width = if size >= 0 { size } else { 160 };
                if self.max_width.get() > 0 {
                    width = width.min(self.max_width.get());
                }
                let h = (width as f64 * self.ratio.get()).round() as i32;
                (if size >= 0 { h } else { 0 }, h, -1, -1)
            }
        }
        fn map(&self) {
            self.parent_map();
            let obj = self.obj();
            let weak = obj.downgrade();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
                let Some(obj) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if let Some(ref iter) = *obj.imp().frame.borrow() {
                    iter.advance(std::time::SystemTime::now());
                }
                obj.queue_draw();
                glib::ControlFlow::Continue
            });
            self.timer.replace(Some(id));
        }
        fn unmap(&self) {
            if let Some(id) = self.timer.borrow_mut().take() {
                id.remove()
            }
            self.parent_unmap()
        }
        fn snapshot(&self, s: &gtk::Snapshot) {
            if let Some(ref iter) = *self.frame.borrow() {
                let pix = iter.pixbuf();
                let bytes = pix.read_pixel_bytes();
                let texture = gdk::MemoryTexture::new(
                    pix.width(),
                    pix.height(),
                    if pix.has_alpha() {
                        gdk::MemoryFormat::R8g8b8a8
                    } else {
                        gdk::MemoryFormat::R8g8b8
                    },
                    &bytes,
                    pix.rowstride() as usize,
                );
                let obj = self.obj();
                let bounds =
                    gtk::graphene::Rect::new(0., 0., obj.width() as f32, obj.height() as f32);
                s.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(bounds, 10.));
                s.append_texture(&texture, &bounds);
                s.pop();
            }
        }
    }
}
glib::wrapper! {pub struct AnimatedImage(ObjectSubclass<gif_imp::AnimatedImage>) @extends gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
impl AnimatedImage {
    pub fn constrain_width(&self, width: i32) {
        self.imp().max_width.set(width);
        self.set_halign(gtk::Align::Center);
        self.queue_resize();
    }
    pub fn new(index: usize) -> Self {
        let obj: Self = glib::Object::new();
        if let Ok(animation) = gtk::gdk_pixbuf::PixbufAnimation::from_file(asset(&format!(
            "preview-{}.gif",
            index % 6
        ))) {
            obj.imp()
                .ratio
                .set(animation.height() as f64 / animation.width() as f64);
            obj.imp().frame.replace(Some(animation.iter(None)));
        }
        obj
    }
}
