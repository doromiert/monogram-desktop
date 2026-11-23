use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};
pub(crate) const TIMELINE_MAX_WIDTH:i32=1080;
pub(crate) const MESSAGE_MAX_WIDTH:i32=TIMELINE_MAX_WIDTH*2/5;

pub fn asset(name: &str) -> PathBuf {
    std::env::var_os("MONOGRAM_ASSET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"))
        .join(name)
}
pub fn install_icons() {
    gtk::Window::set_default_icon_name("com.negzero.MonogramDesktop");
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(asset("icons"));
    }
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
    let a = adw::Avatar::new(size, Some(chat.display_name()), true);
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

    fn preview_body(widget:&gtk::Widget)->Option<gtk::Label> {
        if let Some(label)=widget.downcast_ref::<gtk::Label>().filter(|label|label.has_css_class("message-text")){return Some(label.clone());}
        let mut child=widget.first_child();while let Some(widget)=child {child=widget.next_sibling();if let Some(label)=preview_body(&widget){return Some(label);}}None
    }

    fn media_bubble_width(widget: &gtk::Widget) -> Option<i32> {
        if widget.has_css_class("message-bubble") {
            if !widget.has_css_class("media-bubble") || widget.has_css_class("message-editing") { return None; }
            fn tile(widget: &gtk::Widget) -> Option<gtk::Widget> {
                if widget.has_css_class("media-download") { return Some(widget.clone()); }
                let mut child=widget.first_child();
                while let Some(widget)=child { child=widget.next_sibling(); if let Some(found)=tile(&widget) { return Some(found); } }
                None
            }
            let media=tile(widget)?;
            // GTK 4 has no other way to read a widget's CSS padding and border (the deprecation
            // says "use CSS"), and this is measured before the widget has any size to compare.
            #[allow(deprecated)]
            let (padding,border)={let style=widget.style_context();(style.padding(),style.border())};
            return Some(media.measure(gtk::Orientation::Horizontal,-1).1
                + i32::from(padding.left()+padding.right()+border.left()+border.right()));
        }
        let mut child=widget.first_child();
        while let Some(widget)=child { child=widget.next_sibling(); if let Some(width)=media_bubble_width(&widget) { return Some(width); } }
        None
    }
    fn preferred_width(child: &gtk::Widget) -> (i32,i32,i32,i32) {
        let (minimum,natural,base,natural_base)=child.measure(gtk::Orientation::Horizontal,-1);
        // A quote/caption wraps to its media, rather than leaving unused bubble space beside it.
        match media_bubble_width(child) {
            Some(cap) => (minimum.min(cap),natural.min(cap),base,natural_base),
            None => (minimum,natural,base,natural_base),
        }
    }

    #[derive(Default)]
    pub struct MessageBubbleLimit {
        pub child: RefCell<Option<gtk::Widget>>,
        pub own: Cell<bool>,
        pub last_available: Cell<i32>,
        pub measured_width: Cell<i32>,
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
                let (minimum, natural, _, _) = preferred_width(&child);
                // Report only a modest minimum. The timeline scrolls with a horizontal policy of
                // Never, which makes GTK propagate the widest child's minimum to the whole chat
                // pane: one unbreakable link would otherwise make the window ask for more width
                // than the breakpoint bin has (libadwaita then warns on every layout pass).
                // size_allocate still gives the child its real minimum whenever the row has room.
                return (minimum.min(BUBBLE_SOFT_MIN_WIDTH), natural.max(minimum), -1, -1);
            }
            let (minimum, natural, _, _) = preferred_width(&child);
            let child_width = if for_size < 0 {
                natural.max(minimum).max(1)
            } else {
                let cap = for_size.min(MESSAGE_MAX_WIDTH).max(1);
                if child.has_css_class("message-editing") {
                    cap.max(1)
                } else {
                    // Never narrower than the child's own minimum (for example an unbreakable link),
                    // but never wider than the row itself.
                    natural.min(cap).max(minimum).min(for_size).max(1)
                }
            };
            // Content fitting can choose a narrower width than the initial natural request.
            let child_width=if for_size==self.last_available.get()&&self.measured_width.get()>0 {self.measured_width.get()}else{child_width};
            let (minimum,natural,base,natural_base)=child.measure(gtk::Orientation::Vertical, child_width);
            // With no width, an aspect-ratio child has no fixed minimum height.
            // GTK otherwise carries its wide natural height into narrow rows.
            (if for_size<0 {0}else{minimum},natural.max(minimum),base,natural_base)
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
            let cap = width.min(MESSAGE_MAX_WIDTH).max(1);
            let (minimum, natural, _, _) = preferred_width(&child);
            // The cap must not squeeze a child below its minimum width: an unbreakable link
            // would otherwise overflow its bubble. The row width is the only hard limit. A bubble
            // being edited keeps the plain cap.
            let child_width = if child.has_css_class("message-editing") {
                cap.max(1)
            } else {
                natural.min(cap).max(minimum).min(width).max(1)
            };
            let x = if self.own.get() {
                width.saturating_sub(child_width)
            } else {
                0
            };
            let transform =
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, 0.));
            child.allocate(child_width, height, baseline, Some(transform));
            let mut fitted_width=child_width;
            if let Some(text) = text.as_ref() {
                if let Some((body_width, rendered_width)) = text.rendered_body_width() {
                    let outer_inset = (child_width - child.width()).max(0);
                    let mut needed = rendered_width + (child_width - body_width).max(0);
                    let mut sibling = (child != *text.upcast_ref::<gtk::Widget>()).then(|| child.first_child()).flatten();
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
                        fitted_width=tight;
                    }
                }
            }
            if text.is_none() {
                if let Some(body)=preview_body(&child) {
                    if let Some(bubble)=body.parent().and_downcast::<gtk::Box>().filter(|b|b.has_css_class("message-bubble")) {
                        let mut needed=body.layout().pixel_size().0+1;
                        let mut sibling=bubble.first_child();
                        while let Some(widget)=sibling {
                            sibling=widget.next_sibling();
                            if widget.is_visible() && widget!=body {
                                // Wrapped preview labels request more natural width than
                                // the card actually uses. Preserve its allocated outer edge.
                                needed=needed.max(if widget.has_css_class("link-preview"){widget.width()}else{widget.measure(gtk::Orientation::Horizontal,-1).1});
                            }
                        }
                        let tight=(needed+(child_width-bubble.width()).max(0)).clamp(1,child_width);
                        if tight<child_width {
                            let x=if self.own.get(){width.saturating_sub(tight)}else{0};
                            child.allocate(tight,height,baseline,Some(gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32,0.))));
                            fitted_width=tight;
                        }
                    }
                }
            }
            if self.measured_width.replace(fitted_width)!=fitted_width {
                // GTK ignores resize requests inside size_allocate itself.
                let weak=self.obj().downgrade();glib::idle_add_local_once(move||{if let Some(widget)=weak.upgrade(){widget.queue_resize();}});
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

/// Widest minimum a message bubble reports to its parents (see `MessageBubbleLimit::measure`).
const BUBBLE_SOFT_MIN_WIDTH: i32 = 320;

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

pub(crate) fn text_attributes(label:&gtk::Label)->Option<gtk::pango::AttrList>{
    let attrs=label.attributes().and_then(|attrs|attrs.copy());
    if let Some(attrs)=attrs.as_ref(){let _=attrs.filter(|attr|matches!(attr.type_(),gtk::pango::AttrType::Word|gtk::pango::AttrType::AllowBreaks));}
    attrs
}

pub(crate) fn watch_url_width(label:&gtk::Label,update:impl Fn(&gtk::Label)+'static) {
    let previous=Cell::new((-1,0));
    label.add_tick_callback(move |label,_|{
        let state=(label.width(),label.pango_context().serial());
        if state.0>0&&previous.replace(state)!=state {update(label);}
        glib::ControlFlow::Continue
    });
}

pub(crate) fn url_text_markup(value:&str,label:Option<&gtk::Label>)->String {
    let split=label.is_some_and(|label|label.width()>0&&label.create_pango_layout(Some(value)).pixel_size().0>label.width());
    if !split{return format!("<span segment=\"word\">{}</span>",glib::markup_escape_text(value));}
    let mut markup=String::new();let mut start=0;
    let run=|text:&str|format!("<span segment=\"word\">{}</span>",glib::markup_escape_text(text));
    for (at,ch) in value.char_indices().filter(|(_,ch)|matches!(ch,'/'|'&'|'?')) {
        if start<at {markup.push_str(&run(&value[start..at]));}
        markup.push_str(&glib::markup_escape_text(&ch.to_string()));start=at+ch.len_utf8();
    }
    if start<value.len(){markup.push_str(&run(&value[start..]));}
    markup
}

fn link_markup(source: &str, suffix: &str, links: &[crate::link_previews::ClickableUrl],label:&gtk::Label) -> String {
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
        markup.push_str(&url_text_markup(&source[link.range.clone()],Some(label)));
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
    label.set_markup(&link_markup(source, "", links,label));
    label.set_selectable(true);
    label.update_property(&[gtk::accessible::Property::Label(source)]);
    activate_links(label);
    let source=source.to_owned();let links=links.to_vec();
    watch_url_width(label,move|label|label.set_markup(&link_markup(&source,"",&links,label)));
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
        // Media captions use one character to let the media choose their width.
        if body.max_width_chars()>1 {body.set_max_width_chars(-1);}
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
            let weak=widget.downgrade();watch_url_width(body,move |_|{if let Some(widget)=weak.upgrade(){widget.render_body();}});
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
            let attributes = text_attributes(body);
            let links = self.imp().links.borrow();
            if links.is_empty() {
                body.set_label(&format!("{source}{suffix}"));
            } else {
                body.set_markup(&link_markup(&source, &suffix, &links,body));
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
        let attrs = text_attributes(body).unwrap_or_default();
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
                        (if size<0 {0}else{edge}, edge, -1, -1)
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
                (if size<0 {0}else{height}, height, -1, -1)
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
    #[ignore="requires private native display"]
    fn message_bubbles_use_clamp_max_fraction_and_urls_wrap_at_separators() {
        adw::init().unwrap();
        let css=gtk::CssProvider::new();css.load_from_path(asset("style.css"));
        gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let rows=bx(true,8);
        let body=wrap(&"A longer message can use the full readable conversation column. ".repeat(12),"message-text");
        let status=bx(false,4);status.append(&label("21:10 ✓","caption"));
        let long=MessageText::new(&body,&status);
        let long_limit=MessageBubbleLimit::new(&long,false);rows.append(&long_limit);
        let short=MessageText::new(&wrap("meow","message-text"),&bx(false,0));
        rows.append(&MessageBubbleLimit::new(&short,true));
        let editing=bx(true,0);editing.add_css_class("message-editing");editing.append(&gtk::TextView::new());
        rows.append(&MessageBubbleLimit::new(&editing,true));
        let link=wrap("https://github.com/doromiert/monogram-desktop/pull/2?expanded-message-width=full-conversation-clamp&download=original&more=query","message-text");
        let link_source=link.text().to_string();
        let mut rich=crate::markdown::RichText::plain(&link.text());rich.links.push(crate::markdown::Link{start:0,end:link.text().len(),uri:link.text().into()});
        let link_text=MessageText::with_rich(&link,&bx(false,0),rich);
        rows.append(&MessageBubbleLimit::new(&link_text,false));
        let raw=wrap(&link_source,"message-text");let raw_text=MessageText::with_links(&raw,&bx(false,0),crate::link_previews::clickable_urls(&raw.text(),None));rows.append(&MessageBubbleLimit::new(&raw_text,false));
        let clamp=adw::Clamp::builder().child(&rows).maximum_size(1080).tightening_threshold(840).build();
        let window=gtk::Window::builder().default_width(1000).default_height(700).child(&clamp).build();window.present();
        let context=glib::MainContext::default();
        let settle=||{let until=std::time::Instant::now()+std::time::Duration::from_millis(160);while std::time::Instant::now()<until{context.iteration(false);std::thread::sleep(std::time::Duration::from_millis(1));}};
        settle();
        assert!(long.width()>MESSAGE_MAX_WIDTH*9/10&&long.width()<=MESSAGE_MAX_WIDTH);
        assert_eq!(editing.width(),rows.width().min(MESSAGE_MAX_WIDTH));
        assert!(short.width()<160,"short bubbles still fit content");
        let bounds=short.compute_bounds(&rows).unwrap();assert!((bounds.x()+bounds.width()-rows.width() as f32).abs()<2.,"outgoing bubble keeps right alignment");
        assert!(link.width()<=MESSAGE_MAX_WIDTH);assert!(link.layout().line_count()>1);
        assert!(link.layout().pixel_size().0<=link.width(),"URL fits the expanded bubble");
        for body in [&link,&raw] {assert!(body.width()<=body.layout().pixel_size().0+2,"wrapped URLs fit their painted edge: allocated {}, painted {}",body.width(),body.layout().pixel_size().0);}
        for body in [&link,&raw] {let text=body.text();for line in body.layout().lines_readonly().iter().skip(1) {let at=line.start_index() as usize;assert!(text[..at].ends_with(['/', '&', '?'])||text[at..].starts_with(['/', '&', '?']),"URL break at {at} must use / & or ?");}}
        assert!(raw.text().starts_with("https://github.com/doromiert/monogram-desktop/pull/2?expanded-message-width=full-conversation-clamp&download=original&more=query"));
        window.set_default_size(280,700);settle();assert!(rows.width()<=280,"URL wrapping must not raise the window minimum");
        assert!(long.width()<=rows.width());assert_eq!(editing.width(),rows.width().min(MESSAGE_MAX_WIDTH));
        assert!(body.layout().pixel_size().0<=body.width());
        assert_eq!(long_limit.height(),long_limit.measure(gtk::Orientation::Vertical,long_limit.width()).1,"height uses the final fitted width");
        assert!(link.layout().pixel_size().0<=link.width(),"oversized URL wraps within a narrow column");
        let prefix="A short introduction before this link: ";let source=format!("{prefix}https://github.com/doromiert/monogram-desktop/pull/2");
        let preferred=wrap(&source,"message-text");let links=crate::link_previews::clickable_urls(&source,None);linkify_message_label(&preferred,&source,&links);
        let layout=preferred.layout();let width=preferred.create_pango_layout(Some(&source[prefix.len()..])).pixel_size().0+10;layout.set_width(width*gtk::pango::SCALE);
        assert_eq!(layout.line_readonly(1).unwrap().start_index(),prefix.len() as i32,"a URL that fits stays whole on its next line");
        window.close();
    }

    #[gtk::test]
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
        round.set_paintable(Some(texture_from_pixbuf(&pixels).upcast()));
        round.allocate(200, 200, -1, None);
        let snapshot = gtk::Snapshot::new();
        round.imp().snapshot(&snapshot);
        let node = snapshot.to_node().unwrap();
        assert_eq!(node.node_type(), gtk::gsk::RenderNodeType::RoundedClipNode);
        let clip = node.downcast::<gtk::gsk::RoundedClipNode>().unwrap();
        assert_eq!(clip.clip().corner()[0].width(), 100.);
    }
    #[test]
    fn pixbuf_texture_keeps_size_alpha_and_pixels() {
        use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
        // Width 5 without alpha gives a padded rowstride, which the copy must honour.
        let opaque = Pixbuf::new(Colorspace::Rgb, false, 8, 5, 3).unwrap();
        opaque.fill(0x00ff00ff);
        let texture = texture_from_pixbuf(&opaque);
        assert_eq!((texture.width(), texture.height()), (5, 3));
        let mut bytes = vec![0u8; 5 * 3 * 4];
        texture.download(&mut bytes, 5 * 4);
        assert!(bytes.chunks(4).all(|p| p == [0, 255, 0, 255]), "{bytes:?}");
        let clear = Pixbuf::new(Colorspace::Rgb, true, 8, 2, 2).unwrap();
        clear.fill(0xff000080);
        let texture = texture_from_pixbuf(&clear);
        let mut bytes = vec![0u8; 2 * 2 * 4];
        texture.download(&mut bytes, 2 * 4);
        // Premultiplied BGRA: straight red at alpha 0x80 becomes 0x80 red.
        assert!(bytes.chunks(4).all(|p| p == [0, 0, 128, 128]), "{bytes:?}");
    }
}

/// Copies a pixbuf's 8-bit pixels into a texture (`gdk::Texture::for_pixbuf` is deprecated).
pub(crate) fn texture_from_pixbuf(pixbuf: &gtk::gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    )
    .upcast()
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

/// Shared determinate/indeterminate ring for media upload and download controls.
pub fn transfer_progress_ring(
    fraction: std::rc::Rc<Cell<Option<f64>>>,
    displayed_fraction: std::rc::Rc<Cell<f64>>,
) -> gtk::DrawingArea {
    let disk = gtk::DrawingArea::builder()
        .content_width(58)
        .content_height(58)
        .build();
    disk.set_halign(gtk::Align::Center);
    disk.set_valign(gtk::Align::Center);
    let draw_fraction = fraction;
    let draw_displayed_fraction = displayed_fraction;
    let started = std::time::Instant::now();
    disk.set_draw_func(move |_, ctx, width, height| {
        let x = f64::from(width) / 2.;
        let y = f64::from(height) / 2.;
        let radius = x.min(y) - 3.;
        ctx.set_line_width(3.);
        ctx.set_source_rgba(1., 1., 1., 0.22);
        ctx.arc(x, y, radius, 0., std::f64::consts::TAU);
        let _ = ctx.stroke();
        let start = -std::f64::consts::FRAC_PI_2;
        let (angle, amount) = draw_fraction.get().map_or_else(
            || (start + started.elapsed().as_secs_f64() * 3., 0.23),
            |_| (start, draw_displayed_fraction.get().clamp(0., 1.)),
        );
        ctx.set_source_rgba(1., 1., 1., 0.95);
        ctx.set_line_cap(gtk::cairo::LineCap::Round);
        ctx.arc(x, y, radius, angle, angle + amount * std::f64::consts::TAU);
        let _ = ctx.stroke();
    });
    disk
}

/// Standard model-backed menu, matching Nautilus's PopoverMenu/GMenu pattern.
pub(crate) fn context_menu(items:&[(String,String)], activate:impl Fn(&str)+'static)->gtk::PopoverMenu {
    let model=gio::Menu::new();
    for (title,key) in items {
        let item=gio::MenuItem::new(Some(title),None);
        item.set_action_and_target_value(Some("context.activate"),Some(&key.to_variant()));
        model.append_item(&item);
    }
    let popup=gtk::PopoverMenu::from_model(Some(&model));
    popup.set_has_arrow(false);popup.set_halign(gtk::Align::Start);popup.set_width_request(200);
    let group=gio::SimpleActionGroup::new();
    let action=gio::SimpleAction::new("activate",Some(glib::VariantTy::STRING));
    let weak=popup.downgrade();
    let activate=Rc::new(activate);
    action.connect_activate(move|_,value| {
        let Some(key)=value.and_then(|value|value.str()).map(str::to_owned) else{return};
        let popup=weak.upgrade();
        if let Some(popup)=&popup {popup.popdown();}
        // The callback may rebuild the very list this popover is parented to (mute, pin, archive).
        // Run it after the popover has let go of its parent, never while it still points at a row
        // that is about to be destroyed: the idle below is queued behind the one `connect_closed`
        // schedules, and unparents first in case that one has not run yet.
        let activate=activate.clone();
        glib::idle_add_local_once(move||{
            if let Some(popup)=&popup {if popup.parent().is_some(){popup.unparent();}}
            activate(&key);
        });
    });
    group.add_action(&action);popup.insert_action_group("context",Some(&group));
    popup.connect_closed(|popup| {
        // Native menus close before action dispatch; keep the action group alive until then.
        let popup=popup.clone();glib::idle_add_local_once(move||{if popup.parent().is_some(){popup.unparent();}});
    });
    popup
}

pub(crate) fn style_context_button(button:&gtk::Button) {
    button.add_css_class("flat");button.add_css_class("context-menu-row");
    if let Some(label)=button.child().and_downcast::<gtk::Label>() {label.set_xalign(0.);}
}

#[cfg(test)]
mod context_menu_tests {
    use super::*;
    #[gtk::test]
    #[ignore="requires a private GTK display and D-Bus"]
    fn an_action_that_destroys_the_popover_parent_does_not_crash() {
        adw::init().unwrap();
        let list=gtk::ListBox::new();let row=gtk::ListBoxRow::new();row.set_child(Some(&gtk::Label::new(Some("Chat"))));list.append(&row);
        let window=gtk::Window::builder().default_width(300).default_height(300).child(&list).build();window.present();
        let pump=||{let until=std::time::Instant::now()+std::time::Duration::from_millis(300);while std::time::Instant::now()<until{glib::MainContext::default().iteration(false);std::thread::sleep(std::time::Duration::from_millis(2));}};pump();
        let ran=Rc::new(Cell::new(false));let seen=ran.clone();let target=list.clone();
        // Like "chat-mute": the action rebuilds the list, destroying the row the popover hangs from.
        let popup=context_menu(&[("Mute".into(),"mute".into())],move|_|{while let Some(child)=target.first_child(){target.remove(&child);}seen.set(true);});
        popup.set_parent(&row);popup.popup();pump();
        popup.activate_action("context.activate",Some(&"mute".to_variant())).unwrap();
        drop(row);pump();
        assert!(ran.get(),"the action must still run");
        window.close();
    }

    #[test]
    #[ignore="requires private GTK display, xdotool and D-Bus"]
    fn native_context_menu_is_compact_start_aligned_and_dispatches_before_cleanup() {
        adw::init().unwrap();adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css=gtk::CssProvider::new();css.load_from_path(asset("style.css"));gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let anchor=gtk::Button::with_label("Context menu");anchor.set_halign(gtk::Align::Center);anchor.set_valign(gtk::Align::Center);
        let window=gtk::Window::builder().title("Monogram Context Fixture").default_width(500).default_height(400).child(&anchor).build();window.present();
        let pump=||{let until=std::time::Instant::now()+std::time::Duration::from_millis(200);while std::time::Instant::now()<until{glib::MainContext::default().iteration(false);std::thread::sleep(std::time::Duration::from_millis(2));}};pump();
        let ids=std::process::Command::new("xdotool").args(["search","--name","^Monogram Context Fixture$"]).output().unwrap();let id=String::from_utf8(ids.stdout).unwrap().lines().last().unwrap().to_owned();assert!(std::process::Command::new("xdotool").args(["windowfocus","--sync",&id]).status().unwrap().success());pump();
        let chosen=Rc::new(RefCell::new(String::new()));let result=chosen.clone();
        let popup=context_menu(&[("Open".into(),"open".into()),("Pin".into(),"pin".into()),("Mark read".into(),"read".into()),("Archive".into(),"archive".into()),("Mute".into(),"mute".into())],move|key|{result.replace(key.into());});
        popup.set_parent(&anchor);popup.popup();pump();
        fn labels(widget:&gtk::Widget, out:&mut Vec<gtk::Label>) {
            if let Some(label)=widget.downcast_ref::<gtk::Label>() {out.push(label.clone());}
            let mut child=widget.first_child();while let Some(widget)=child {child=widget.next_sibling();labels(&widget,out);}
        }
        let mut all=Vec::new();labels(popup.upcast_ref(),&mut all);let mut pin=None;
        for title in ["Open","Pin","Mark read","Archive","Mute"] {
            let label=all.iter().find(|label|label.text()==title).unwrap();
            let mut row=label.clone().upcast::<gtk::Widget>();while row.css_name()!="modelbutton" {row=row.parent().unwrap();}
            assert!(row.height()<=34,"{title} row is{}px tall",row.height());
            assert!(row.width()>=130,"{title} menu row too narrow:{}px",row.width());
            let bounds=label.compute_bounds(&row).unwrap();
            assert!(bounds.x()<30.,"{title} label must start at the menu gutter: {:?}",bounds);
            if title=="Pin" {pin=Some(row.clone());}
            eprintln!("Native {title}:{}x{}px, label start aligned",row.width(),row.height());
        }
        if let Some(path)=std::env::var_os("MONOGRAM_CONTEXT_CAPTURE") {assert!(std::process::Command::new("import").arg("-window").arg("root").arg(path).status().unwrap().success());}
        assert!(pin.unwrap().grab_focus());assert!(std::process::Command::new("xdotool").args(["key","--clearmodifiers","Return"]).status().unwrap().success());pump();
        assert_eq!(chosen.borrow().as_str(),"pin","native menu action must survive menu close");assert!(popup.parent().is_none(),"closed popover must be cleaned up");
        let button=gtk::Button::with_label("Add to Favorites");style_context_button(&button);assert_eq!(button.child().and_downcast::<gtk::Label>().unwrap().xalign(),0.);
        window.close();
    }
}
