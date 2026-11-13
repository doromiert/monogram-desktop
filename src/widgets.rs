use gtk::{gdk, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
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
mod message_bubble_limit_imp {
    use super::*;

    pub(super) fn message_text_descendant(widget: &gtk::Widget) -> Option<super::MessageText> {
        if let Ok(found) = widget.clone().downcast::<super::MessageText>() {
            return Some(found);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = message_text_descendant(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }

    #[derive(Default)]
    pub struct MessageBubbleLimit {
        pub child: RefCell<Option<gtk::Widget>>,
        pub own: Cell<bool>,
        pub last_available: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageBubbleLimit {
        const NAME: &'static str = "MonogramMessageBubbleLimit";
        type Type = super::MessageBubbleLimit;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MessageBubbleLimit {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MessageBubbleLimit {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return (0, 0, -1, -1);
            };
            if orientation == gtk::Orientation::Horizontal {
                let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
                return (minimum, natural.max(minimum), -1, -1);
            }
            let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let child_width = if for_size < 0 {
                natural.max(minimum).max(1)
            } else {
                natural
                    .min(for_size.saturating_mul(2) / 5)
                    .max(minimum)
                    .max(1)
            };
            child.measure(gtk::Orientation::Vertical, child_width)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return;
            };
            let text = message_text_descendant(&child);
            if self.last_available.replace(width) != width {
                if let Some(text) = text.as_ref() {
                    text.set_compact(false);
                }
            }
            let cap = width.max(0).saturating_mul(2) / 5;
            let (_, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let child_width = natural.min(cap).max(1);
            let x = if self.own.get() {
                width.saturating_sub(child_width)
            } else {
                0
            };
            let transform =
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, 0.));
            child.allocate(child_width, height, baseline, Some(transform));
            if let Some(text) = text.as_ref() {
                if let Some((body_width, rendered_width)) = text.rendered_body_width() {
                    let outer_inset = (child_width - child.width()).max(0);
                    let mut needed = rendered_width + (child_width - body_width).max(0);
                    let mut sibling = child.first_child();
                    while let Some(widget) = sibling {
                        sibling = widget.next_sibling();
                        if widget.is_visible() && widget != *text.upcast_ref::<gtk::Widget>() {
                            needed = needed.max(
                                widget.measure(gtk::Orientation::Horizontal, -1).1 + outer_inset,
                            );
                        }
                    }
                    let tight = needed.clamp(1, child_width);
                    if tight < child_width {
                        let x = if self.own.get() {
                            width.saturating_sub(tight)
                        } else {
                            0
                        };
                        let transform = gtk::gsk::Transform::new()
                            .translate(&gtk::graphene::Point::new(x as f32, 0.));
                        child.allocate(tight, height, baseline, Some(transform));
                    }
                }
            }
            if let Some(text) = text {
                let weak = text.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(text) = weak.upgrade() {
                        text.refresh_compact();
                    }
                });
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct MessageBubbleLimit(ObjectSubclass<message_bubble_limit_imp::MessageBubbleLimit>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MessageBubbleLimit {
    pub fn new(child: &impl IsA<gtk::Widget>, own: bool) -> Self {
        let widget: Self = glib::Object::new();
        widget.imp().own.set(own);
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(&widget);
        widget.imp().child.replace(Some(child));
        widget.set_hexpand(true);
        widget
    }
}
mod message_text_imp {
    use super::*;
    #[derive(Default)]
    pub struct MessageText {
        pub body: RefCell<Option<gtk::Label>>,
        pub status: RefCell<Option<gtk::Box>>,
        pub source: RefCell<String>,
        pub links: RefCell<Vec<crate::link_previews::ClickableUrl>>,
        pub rich: RefCell<Option<Rc<crate::formatting_ui::FormattedLabel>>>,
        pub rtl: Cell<bool>,
        pub compact: Cell<bool>,
        pub status_in_reactions: Cell<bool>,
        pub reaction_row: RefCell<glib::WeakRef<gtk::Box>>,
        pub reaction_status: RefCell<Option<gtk::Box>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for MessageText {
        const NAME: &'static str = "MonogramMessageText";
        type Type = super::MessageText;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for MessageText {}
    impl WidgetImpl for MessageText {}
    impl BoxImpl for MessageText {}
}
glib::wrapper! {pub struct MessageText(ObjectSubclass<message_text_imp::MessageText>) @extends gtk::Box,gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget,gtk::Orientable;}

fn link_markup(source: &str, suffix: &str, links: &[crate::link_previews::ClickableUrl]) -> String {
    let mut markup = String::new();
    let mut cursor = 0;
    for link in links {
        if link.range.start < cursor
            || link.range.end > source.len()
            || !source.is_char_boundary(link.range.start)
            || !source.is_char_boundary(link.range.end)
        {
            continue;
        }
        markup.push_str(&glib::markup_escape_text(&source[cursor..link.range.start]));
        markup.push_str("<a href=\"");
        markup.push_str(&glib::markup_escape_text(&link.uri));
        markup.push_str("\">");
        markup.push_str(&glib::markup_escape_text(&source[link.range.clone()]));
        markup.push_str("</a>");
        cursor = link.range.end;
    }
    markup.push_str(&glib::markup_escape_text(&source[cursor..]));
    markup.push_str(&glib::markup_escape_text(suffix));
    markup
}

fn activate_links(label: &gtk::Label) {
    label.connect_activate_link(|label, uri| {
        if uri.starts_with("monogram-spoiler:") || uri.starts_with('#') { return glib::Propagation::Proceed; }
        if !crate::markdown::safe_uri(uri) { return glib::Propagation::Stop; }
        let parent = label.root().and_downcast::<gtk::Window>();
        let weak_parent = parent.as_ref().map(gtk::Window::downgrade);
        gtk::UriLauncher::new(uri).launch(
            parent.as_ref(),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                if result.is_err() {
                    let parent = weak_parent.and_then(|parent| parent.upgrade());
                    gtk::AlertDialog::builder()
                        .message("Could Not Open Link")
                        .detail("No application is available to open this web address.")
                        .modal(true)
                        .build()
                        .show(parent.as_ref());
                }
            },
        );
        glib::Propagation::Stop
    });
}

pub fn linkify_message_label(
    label: &gtk::Label,
    source: &str,
    links: &[crate::link_previews::ClickableUrl],
) {
    if links.is_empty() {
        return;
    }
    label.set_markup(&link_markup(source, "", links));
    label.set_selectable(true);
    label.update_property(&[gtk::accessible::Property::Label(source)]);
    activate_links(label);
}

impl MessageText {
    fn rendered_body_width(&self) -> Option<(i32, i32)> {
        let body = self.imp().body.borrow();
        let body = body.as_ref()?;
        (body.width() > 0).then(|| (body.width(), body.layout().pixel_size().0 + 1))
    }
    pub fn new(body: &gtk::Label, status: &gtk::Box) -> Self {
        Self::with_links(body, status, Vec::new())
    }

    pub fn with_links(
        body: &gtk::Label,
        status: &gtk::Box,
        links: Vec<crate::link_previews::ClickableUrl>,
    ) -> Self {
        let widget: Self = glib::Object::new();
        let source = body.text().to_string();
        let layout = body.create_pango_layout(Some(&source));
        let rtl = layout.line_readonly(0).is_some_and(|line| {
            matches!(
                line.resolved_direction(),
                gtk::pango::Direction::Rtl | gtk::pango::Direction::WeakRtl
            )
        });
        widget.imp().rtl.set(rtl);
        body.set_yalign(0.);
        body.set_selectable(true);
        body.update_property(&[gtk::accessible::Property::Label(&source)]);
        let original = source.clone();
        body.connect_copy_clipboard(move |label| {
            if label.has_css_class("formatted-message-text") { return; }
            if let Some((start, end)) = label.selection_bounds() {
                let (start, end) = (
                    start.min(end).max(0) as usize,
                    start.max(end).max(0) as usize,
                );
                let selected: String = original
                    .chars()
                    .skip(start)
                    .take(end.saturating_sub(start))
                    .collect();
                label.clipboard().set_text(&selected);
                label.stop_signal_emission_by_name("copy-clipboard");
            }
        });
        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_child(Some(body));
        status.set_halign(gtk::Align::End);
        status.set_valign(gtk::Align::End);
        overlay.add_overlay(status);
        widget.append(&overlay);
        widget.imp().source.replace(source);
        widget.imp().links.replace(links);
        widget.imp().body.replace(Some(body.clone()));
        widget.imp().status.replace(Some(status.clone()));
        if !widget.imp().links.borrow().is_empty() {
            activate_links(body);
        }
        widget.render_body();
        widget.refresh_shape();
        widget
    }

    pub fn with_rich(body:&gtk::Label,status:&gtk::Box,rich:crate::markdown::RichText)->Self {
        body.set_text(&rich.text);
        let widget=Self::new(body,status);
        widget.set_rich(rich);
        widget
    }
    pub fn formatted_state(&self)->Option<Rc<crate::formatting_ui::FormattedLabel>> {self.imp().rich.borrow().clone()}
    pub fn set_rich(&self,rich:crate::markdown::RichText) {
        if let Some(body)=self.imp().body.borrow().as_ref(){
            self.imp().rich.replace(Some(crate::formatting_ui::FormattedLabel::new(body,rich)));
        }
        self.render_body();self.refresh_shape();
    }
    fn render_body(&self) {
        let source = self.imp().source.borrow();
        let suffix = if self.imp().status_in_reactions.get() {
            String::new()
        } else {
            format!(
                "{}\u{fffc}",
                if self.imp().compact.get() || self.imp().rtl.get() {
                    "\n"
                } else {
                    " "
                }
            )
        };
        if let Some(rich)=self.imp().rich.borrow().as_ref(){rich.with_suffix(&suffix);return;}
        if let Some(body) = self.imp().body.borrow().as_ref() {
            // Inline emoji shapes can arrive asynchronously; retain them when
            // compact/status reflow rebuilds the native anchor markup.
            let attributes = body.attributes();
            let links = self.imp().links.borrow();
            if links.is_empty() {
                body.set_label(&format!("{source}{suffix}"));
            } else {
                body.set_markup(&link_markup(&source, &suffix, &links));
            }
            body.set_attributes(attributes.as_ref());
        }
    }

    pub fn refresh_shape(&self) {
        let body = self.imp().body.borrow();
        let status = self.imp().status.borrow();
        let (Some(body), Some(status)) = (body.as_ref(), status.as_ref()) else {
            return;
        };
        let (_, width, _, _) = status.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = status.measure(gtk::Orientation::Vertical, width);
        let logical = gtk::pango::Rectangle::new(
            0,
            (3 - height) * gtk::pango::SCALE,
            (width + 8) * gtk::pango::SCALE,
            height * gtk::pango::SCALE,
        );
        let ink = gtk::pango::Rectangle::new(0, 0, 0, 0);
        let mut shape = gtk::pango::AttrShape::new(&ink, &logical);
        shape.set_start_index(self.imp().source.borrow().len() as u32 + 1);
        shape.set_end_index(u32::MAX);
        // Preserve the native anchor attributes installed by set_markup.
        let attrs = if self.imp().rich.borrow().is_some(){body.attributes()}else{body.layout().attributes()}.and_then(|a|a.copy()).unwrap_or_default();
        attrs.change(shape);
        body.set_attributes(Some(&attrs));
    }

    pub(crate) fn set_compact(&self, compact: bool) {
        if self.imp().compact.replace(compact) != compact {
            self.render_body();
            self.refresh_shape();
        }
    }

    pub fn attach_reaction_row(bubble: &gtk::Box, row: &gtk::Box) -> bool {
        let Some(text) = message_bubble_limit_imp::message_text_descendant(bubble.upcast_ref())
        else {
            return false;
        };
        let slot = bx(false, 0);
        slot.add_css_class("reaction-status");
        slot.set_hexpand(true);
        slot.set_halign(gtk::Align::End);
        slot.set_valign(gtk::Align::End);
        slot.set_margin_start(8);
        slot.set_visible(false);
        row.append(&slot);
        text.imp().reaction_row.replace(row.downgrade());
        text.imp().reaction_status.replace(Some(slot));
        true
    }

    pub(crate) fn refresh_reaction_layout(bubble: &gtk::Box) {
        if let Some(text) = message_bubble_limit_imp::message_text_descendant(bubble.upcast_ref()) {
            text.refresh_compact();
        }
    }

    fn refresh_compact(&self) {
        let Some(body) = self.imp().body.borrow().as_ref().cloned() else {
            return;
        };
        let Some(status) = self.imp().status.borrow().as_ref().cloned() else {
            return;
        };
        if body.width() <= 0 {
            return;
        }
        let source = self.imp().source.borrow().clone();
        let Some((last, _)) = source.char_indices().last() else {
            return;
        };
        // Measure the inline timestamp even while its real widget lives in the reaction row.
        let probe = body.layout().copy();
        probe.set_text(&format!("{source} \u{fffc}"));
        probe.set_width(body.width() * gtk::pango::SCALE);
        let (_, width, _, _) = status.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = status.measure(gtk::Orientation::Vertical, width);
        let attrs = body
            .attributes()
            .and_then(|attrs| attrs.copy())
            .unwrap_or_default();
        let mut shape = gtk::pango::AttrShape::new(
            &gtk::pango::Rectangle::new(0, 0, 0, 0),
            &gtk::pango::Rectangle::new(
                0,
                (3 - height) * gtk::pango::SCALE,
                (width + 8) * gtk::pango::SCALE,
                height * gtk::pango::SCALE,
            ),
        );
        shape.set_start_index(source.len() as u32 + 1);
        shape.set_end_index(u32::MAX);
        attrs.change(shape);
        probe.set_attributes(Some(&attrs));
        let (source_line, _) = probe.index_to_line_x(last as i32, false);
        let (status_line, _) = probe.index_to_line_x(source.len() as i32 + 1, false);
        let wrapped = self.imp().rtl.get() || status_line > source_line;
        let row = self.imp().reaction_row.borrow().upgrade();
        let share = row.as_ref().is_some_and(|row| {
            let mut child=row.first_child();
            while let Some(widget)=child {
                child=widget.next_sibling();
                if !widget.has_css_class("reaction-status") && widget.is_visible(){return row.is_visible();}
            }
            false
        });
        if self.imp().status_in_reactions.replace(share) != share {
            if let (Some(slot), Some(overlay)) = (
                self.imp().reaction_status.borrow().as_ref(),
                body.parent().and_downcast::<gtk::Overlay>(),
            ) {
                if share {
                    overlay.remove_overlay(&status);
                    slot.append(&status);
                    slot.set_visible(true);
                } else {
                    slot.remove(&status);
                    slot.set_visible(false);
                    overlay.add_overlay(&status);
                }
            }
            // Restore natural inline width before deciding from the new allocation.
            self.imp().compact.set(false);
            self.render_body();
            self.refresh_shape();
            if !share { return; }
        }
        self.set_compact(wrapped && !share);
    }
}

const INLINE_MEDIA_MAX_WIDTH: i32 = 420;
const INLINE_MEDIA_MAX_HEIGHT: i32 = 360;
const STICKER_MAX_SIZE: i32 = 256;

mod image_imp {
    use super::*;
    fn fit_box(max_width: i32, max_height: i32, ratio: f64) -> (i32, i32) {
        let max_width = max_width.max(1);
        let max_height = max_height.max(1);
        let ratio = ratio.max(0.01);
        if ratio >= f64::from(max_width) / f64::from(max_height) {
            (
                max_width,
                (f64::from(max_width) / ratio).round().max(1.) as i32,
            )
        } else {
            (
                (f64::from(max_height) * ratio).round().max(1.) as i32,
                max_height,
            )
        }
    }

    #[derive(Default)]
    pub struct AspectImage {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub ratio: Cell<f64>,
        pub cover: Cell<bool>,
        pub max_width: Cell<i32>,
        pub max_height: Cell<i32>,
        pub borderless: Cell<bool>,
        pub circular: Cell<bool>,
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
                    gtk::Orientation::Horizontal => (0, self.max_width.get(), -1, -1),
                    _ => {
                        let edge = if size < 0 { self.max_width.get() } else { size };
                        (edge, edge, -1, -1)
                    }
                };
            }
            let (width, height) = fit_box(
                self.max_width.get(),
                self.max_height.get(),
                self.ratio.get(),
            );
            if o == gtk::Orientation::Horizontal {
                (0, width, -1, -1)
            } else {
                let height = if size >= 0 {
                    let allocated_width = size.min(width).max(0);
                    ((f64::from(allocated_width) / self.ratio.get().max(0.01)).round() as i32)
                        .min(height)
                } else {
                    height
                };
                (height, height, -1, -1)
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f64;
            let h = obj.height() as f64;
            if let Some(ref p) = *self.paintable.borrow() {
                let edge = w.min(h);
                let bounds = if self.circular.get() {
                    gtk::graphene::Rect::new(
                        ((w - edge) / 2.) as f32,
                        ((h - edge) / 2.) as f32,
                        edge as f32,
                        edge as f32,
                    )
                } else {
                    gtk::graphene::Rect::new(0., 0., w as f32, h as f32)
                };
                if self.circular.get() {
                    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(
                        bounds,
                        w.min(h) as f32 / 2.,
                    ));
                } else if !self.borderless.get() {
                    let mut radii = [if obj.has_css_class("reply-image-preview") { 4. } else { 10. }; 4];
                    let mut parent = obj.parent();
                    while let Some(widget) = parent {
                        if widget.has_css_class("media-bubble") && !obj.has_css_class("reply-image-preview") {
                            let own = widget.has_css_class("own");
                            radii[if own { 2 } else { 3 }] = 3.;
                            if widget.has_css_class("cluster-previous") {
                                radii[if own { 1 } else { 0 }] = 3.;
                            }
                            break;
                        }
                        parent = widget.parent();
                    }
                    let corner = |radius| gtk::graphene::Size::new(radius, radius);
                    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::new(
                        bounds,
                        corner(radii[0]),
                        corner(radii[1]),
                        corner(radii[2]),
                        corner(radii[3]),
                    ));
                }
                if self.circular.get() {
                    snapshot.save();
                    snapshot.translate(&gtk::graphene::Point::new(
                        ((w - edge) / 2.) as f32,
                        ((h - edge) / 2.) as f32,
                    ));
                    p.snapshot(snapshot, edge, edge);
                    snapshot.restore();
                } else if self.cover.get() {
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
                if self.circular.get() || !self.borderless.get() {
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
        image.imp().max_width.set(110);
        image
    }
    pub fn reply_thumbnail() -> Self {
        let image = Self::square();
        image.imp().max_width.set(36);
        image.set_size_request(36, 36);
        image.set_halign(gtk::Align::Start);
        image.set_valign(gtk::Align::Center);
        image.add_css_class("reply-image-preview");
        image
    }
    pub fn set_circular(&self, circular: bool) {
        self.imp().circular.set(circular);
        if circular {
            self.set_ratio(1.);
        }
        self.queue_draw();
    }
    pub fn is_circular(&self) -> bool {
        self.imp().circular.get()
    }
    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.imp().paintable.borrow().clone()
    }
    pub fn set_ratio(&self, ratio: f64) {
        if ratio.is_finite()
            && ratio > 0.
            && (self.imp().ratio.replace(ratio) - ratio).abs() > 0.001
        {
            self.queue_resize();
        }
    }
    pub fn set_paintable(&self, paintable: Option<gdk::Paintable>) {
        if !self.imp().cover.get()
            && let Some(image) = paintable.as_ref()
        {
            let ratio = image.intrinsic_aspect_ratio();
            if ratio > 0. && (self.imp().ratio.replace(ratio) - ratio).abs() > 0.001 {
                self.queue_resize();
            }
        }
        self.imp().paintable.replace(paintable);
        self.queue_draw();
    }
    pub fn thumbnail(width: i32, borderless: bool) -> Self {
        let obj: Self = glib::Object::new();
        let max_size = if borderless {
            STICKER_MAX_SIZE
        } else {
            INLINE_MEDIA_MAX_WIDTH
        };
        obj.imp().max_width.set(width.min(max_size).max(1));
        obj.imp().max_height.set(if borderless {
            STICKER_MAX_SIZE
        } else {
            INLINE_MEDIA_MAX_HEIGHT
        });
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

#[cfg(test)]
mod media_geometry_tests {
    use super::*;

    #[test]
    fn inline_media_fits_caps_and_narrow_widths() {
        gtk::init().expect("GTK initializes");
        for (width, borderless, ratio, expected, narrow) in [
            (320, false, 0.5, (180, 360), (120, 240)),
            (320, false, 2.0, (320, 160), (120, 60)),
            (512, true, 1.0, (256, 256), (120, 120)),
        ] {
            let image = AspectImage::thumbnail(width, borderless);
            image.set_ratio(ratio);
            assert_eq!(
                image.measure(gtk::Orientation::Horizontal, -1).1,
                expected.0
            );
            assert_eq!(
                image.measure(gtk::Orientation::Vertical, expected.0).1,
                expected.1
            );
            assert_eq!(
                image.measure(gtk::Orientation::Vertical, narrow.0).1,
                narrow.1
            );
            assert_eq!(
                image.measure(gtk::Orientation::Vertical, narrow.0).0,
                narrow.1
            );
        }
        let sticker = AspectImage::thumbnail(512, true);
        assert!(sticker.imp().borderless.get());
        let round = AspectImage::thumbnail(320, false);
        round.set_circular(true);
        let pixels =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 200, 200)
                .unwrap();
        pixels.fill(0xff0000ff);
        round.set_paintable(Some(gdk::Texture::for_pixbuf(&pixels).upcast()));
        round.allocate(200, 200, -1, None);
        let snapshot = gtk::Snapshot::new();
        round.imp().snapshot(&snapshot);
        let node = snapshot.to_node().unwrap();
        assert_eq!(node.node_type(), gtk::gsk::RenderNodeType::RoundedClipNode);
        let clip = node.downcast::<gtk::gsk::RoundedClipNode>().unwrap();
        assert_eq!(clip.clip().corner()[0].width(), 100.);
    }
}

pub fn reveal_media(controls: &gtk::Box, paintable: &impl IsA<gdk::Paintable>) {
    controls.remove_css_class("media-preview-error");
    controls.set_tooltip_text(None);
    if let Some(overlay) = controls.parent().and_downcast::<gtk::Overlay>()
        && let Some(placeholder) = overlay.child().and_downcast::<AspectImage>()
    {
        placeholder.set_ratio(paintable.intrinsic_aspect_ratio());
    }
    controls.set_visible(false);
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

mod square_cell_imp {
    use super::*;
    #[derive(Default)]
    pub struct SquareCell {
        pub child: RefCell<Option<gtk::Widget>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for SquareCell {
        const NAME: &'static str = "MonogramSquareCell";
        type Type = super::SquareCell;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for SquareCell {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for SquareCell {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().as_ref().cloned() else {
                return (0, 0, -1, -1);
            };
            let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            if orientation == gtk::Orientation::Horizontal {
                return (minimum, natural, -1, -1);
            }
            if for_size < 0 {
                return (minimum.max(1), natural.max(1), -1, -1);
            }
            (for_size.max(1), for_size.max(1), -1, -1)
        }
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                let side = width.min(height).max(1);
                let transform = gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(
                    ((width - side) / 2) as f32,
                    ((height - side) / 2) as f32,
                ));
                child.allocate(side, side, baseline, Some(transform));
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}
glib::wrapper! {
    pub struct SquareCell(ObjectSubclass<square_cell_imp::SquareCell>)
        @extends gtk::Widget, @implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;
}
impl SquareCell {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let cell: Self = glib::Object::new();
        let child = child.clone().upcast::<gtk::Widget>();
        child.set_parent(&cell);
        cell.imp().child.replace(Some(child));
        cell.set_hexpand(true);
        cell
    }
}
