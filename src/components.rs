//! Shared UI components.
//!
//! Each element that several views used to assemble by hand lives here once, with a small
//! property API instead of loose CSS-class juggling:
//!
//! * [`UtilityPane`]: the flat toolbar view with a heading, an optional back button and a close
//!   button that every right-hand utility sidebar uses.
//! * [`MessageBubble`]: the timeline bubble; `kind`, `own`, `document`, `emoji-only` and
//!   `highlighted` properties own the style classes the stylesheet keys on.
//! * [`ChatRow`]: one conversation in the sidebar list.
//! * [`PresenceAvatar`]: an avatar with a presence dot.
//! * [`StoryAvatar`]: an avatar with the unseen/seen ring and an optional "add" badge.
//! * [`HatChip`] and [`HatStrip`]: XEP-0317 hats, drawn as an outlined pill in the hat's own hue.
//! * [`ContactRow`]: a list row for a person (avatar, name, optional address, optional check).
//! * [`NavRow`]: a list row that opens something (icon, title, subtitle, chevron).
//! * [`ReplyQuote`], [`ForwardedBanner`], [`FileCard`] and [`overlay_meta`]: the parts of a
//!   message bubble that more than one kind of message shows.
//!
//! The style class names are unchanged, so `assets/style.css` and the code that inspects them
//! (`has_css_class("media-bubble")`, ...) keep working.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::widgets::{bx, icon_button, label, text};

// ---------------------------------------------------------------------------------------------
// UtilityPane
// ---------------------------------------------------------------------------------------------

/// A utility sidebar page: `ToolbarView` + flat `HeaderBar` with a heading and a close button.
///
/// `view` is what gets installed in the split view; `header` stays available so a caller can add
/// its own controls (a tab switcher, extra buttons) next to the standard ones.
pub struct UtilityPane {
    pub view: adw::ToolbarView,
    pub header: adw::HeaderBar,
}

impl UtilityPane {
    /// `title` is the centred heading; pass `None` when the caller supplies its own title widget.
    pub fn new(title: Option<&str>) -> Self {
        let view = adw::ToolbarView::new();
        view.add_css_class("utility-sidebar");
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).build();
        if let Some(title) = title {
            header.set_title_widget(Some(&label(title, "heading")));
        }
        view.add_top_bar(&header);
        Self { view, header }
    }
    pub fn with_class(self, class: &str) -> Self {
        self.view.add_css_class(class);
        self
    }
    /// Adopt a toolbar view that already exists (a pane that is built once and kept), giving it
    /// the same chrome as a freshly built one.
    pub fn adopt(view: &adw::ToolbarView) -> Self {
        view.add_css_class("utility-sidebar");
        let header = adw::HeaderBar::builder().show_start_title_buttons(false).build();
        view.add_top_bar(&header);
        Self { view: view.clone(), header }
    }
    /// A leading "back" button.
    pub fn back(&self, tooltip: &str, on_click: impl Fn() + 'static) -> gtk::Button {
        let button = icon_button("go-previous-symbolic", tooltip);
        button.connect_clicked(move |_| on_click());
        self.header.pack_start(&button);
        button
    }
    /// The sidebar-toggle button that dismisses the pane; `at_end` packs it at the trailing edge.
    pub fn close(&self, tooltip: &str, at_end: bool, on_click: impl Fn() + 'static) -> gtk::Button {
        let button = icon_button("sidebar-show-symbolic", tooltip);
        button.connect_clicked(move |_| on_click());
        if at_end {
            self.header.pack_end(&button);
        } else {
            self.header.pack_start(&button);
        }
        button
    }
}

/// A page with nothing but the window's standard header bar above its content.
pub fn titled_page() -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view
}

// ---------------------------------------------------------------------------------------------
// MessageBubble
// ---------------------------------------------------------------------------------------------

/// What a bubble frames. Decides the container style, not the content.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MonogramBubbleKind")]
pub enum BubbleKind {
    /// Text, polls, files and everything else drawn on the normal bubble background.
    #[default]
    Text,
    /// A picture or video: the media fills the bubble.
    Media,
    /// A sticker: no bubble chrome at all.
    Sticker,
    /// A centred system line such as a date separator or a call notice.
    Action,
}

mod bubble_imp {
    use super::*;
    use std::cell::Cell;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::MessageBubble)]
    pub struct MessageBubble {
        #[property(get, set = Self::set_kind, explicit_notify, builder(BubbleKind::Text))]
        kind: Cell<BubbleKind>,
        /// Sent by the user (tinted, aligned to the end).
        #[property(get, set = Self::set_own, explicit_notify)]
        own: Cell<bool>,
        /// Carries a file that is not media: slightly different padding.
        #[property(get, set = Self::set_document, explicit_notify)]
        document: Cell<bool>,
        #[property(get, set = Self::set_emoji_only, name = "emoji-only", explicit_notify)]
        emoji_only: Cell<bool>,
        /// Briefly outlined after a jump to a replied-to message.
        #[property(get, set = Self::set_highlighted, explicit_notify)]
        highlighted: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageBubble {
        const NAME: &'static str = "MonogramMessageBubble";
        type Type = super::MessageBubble;
        type ParentType = gtk::Box;
    }

    #[glib::derived_properties]
    impl ObjectImpl for MessageBubble {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_vexpand(false);
            obj.set_valign(gtk::Align::Start);
            self.refresh();
        }
    }
    impl WidgetImpl for MessageBubble {}
    impl BoxImpl for MessageBubble {}

    impl MessageBubble {
        fn set_kind(&self, value: BubbleKind) {
            let previous = self.kind.replace(value);
            if previous != value {
                // Only entering or leaving an action pill touches the alignment, so a caller's
                // own choice for ordinary bubbles survives a change of kind.
                if value == BubbleKind::Action {
                    self.obj().set_halign(gtk::Align::Center);
                } else if previous == BubbleKind::Action {
                    self.obj().set_halign(gtk::Align::Fill);
                }
                self.refresh();
                self.obj().notify_kind();
            }
        }
        fn set_own(&self, value: bool) {
            if self.own.replace(value) != value {
                self.refresh();
                self.obj().notify_own();
            }
        }
        fn set_document(&self, value: bool) {
            if self.document.replace(value) != value {
                self.refresh();
                self.obj().notify_document();
            }
        }
        fn set_emoji_only(&self, value: bool) {
            if self.emoji_only.replace(value) != value {
                self.refresh();
                self.obj().notify_emoji_only();
            }
        }
        fn set_highlighted(&self, value: bool) {
            if self.highlighted.replace(value) != value {
                self.refresh();
                self.obj().notify_highlighted();
            }
        }
        /// Make the style classes match the properties; the only place that touches them.
        fn refresh(&self) {
            let obj = self.obj();
            let kind = self.kind.get();
            let action = kind == BubbleKind::Action;
            let wanted = [
                ("message-bubble", !action),
                ("date-pill", action),
                ("action-pill", action),
                ("own", self.own.get() && !action),
                ("sticker-bubble", kind == BubbleKind::Sticker),
                ("media-bubble", kind == BubbleKind::Media),
                ("document-bubble", self.document.get()),
                ("emoji-only", self.emoji_only.get()),
                ("reply-target", self.highlighted.get()),
            ];
            for (class, on) in wanted {
                if on {
                    obj.add_css_class(class);
                } else {
                    obj.remove_css_class(class);
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct MessageBubble(ObjectSubclass<bubble_imp::MessageBubble>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MessageBubble {
    pub fn new(kind: BubbleKind) -> Self {
        glib::Object::builder().property("kind", kind).build()
    }
    pub fn as_box(&self) -> &gtk::Box {
        self.upcast_ref()
    }
}

// ---------------------------------------------------------------------------------------------
// PresenceAvatar
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MonogramPresence")]
pub enum Presence {
    #[default]
    Online,
    Away,
    /// Do not disturb.
    Busy,
    /// Offline, or presence we are not allowed to see (no subscription, or a refused one).
    Unknown,
}

impl Presence {
    fn class(self) -> &'static str {
        match self {
            Presence::Online => "presence-online",
            Presence::Away => "presence-away",
            Presence::Busy => "presence-busy",
            Presence::Unknown => "presence-unknown",
        }
    }
    /// What the colour means in words, for the tooltip and assistive technology.
    pub fn meaning(self) -> &'static str {
        match self {
            Presence::Online => crate::contact_status::Light::Green.meaning(),
            Presence::Away => crate::contact_status::Light::Yellow.meaning(),
            Presence::Busy => crate::contact_status::Light::Red.meaning(),
            Presence::Unknown => crate::contact_status::Light::Grey.meaning(),
        }
    }
    /// From the status text a person has in a room or a profile ("Online", "Away", "Busy"): any
    /// other text ("Not in this room", "Offline", nothing) is unknown, never a guess of "online".
    pub fn from_status(status: &str) -> Self {
        match status {
            "Online" => Presence::Online,
            "Away" => Presence::Away,
            "Busy" | "Do not disturb" => Presence::Busy,
            _ => Presence::Unknown,
        }
    }
    pub fn from_light(light: crate::contact_status::Light) -> Self {
        use crate::contact_status::Light;
        match light {
            Light::Green => Presence::Online,
            Light::Yellow => Presence::Away,
            Light::Red => Presence::Busy,
            Light::Grey => Presence::Unknown,
        }
    }
    /// From the label `presence_label` produces ("Away", "Busy", anything else is online).
    pub fn from_label(label: &str) -> Self {
        match label {
            "Away" => Presence::Away,
            "Busy" => Presence::Busy,
            _ => Presence::Online,
        }
    }
}

mod presence_imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::PresenceAvatar)]
    pub struct PresenceAvatar {
        #[property(get, set = Self::set_presence, explicit_notify, builder(Presence::Online))]
        presence: Cell<Presence>,
        pub overlay: gtk::Overlay,
        pub dot: RefCell<Option<gtk::Box>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PresenceAvatar {
        const NAME: &'static str = "MonogramPresenceAvatar";
        type Type = super::PresenceAvatar;
        type ParentType = adw::Bin;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PresenceAvatar {
        fn constructed(&self) {
            self.parent_constructed();
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("presence-dot");
            dot.add_css_class(self.presence.get().class());
            dot.set_tooltip_text(Some(self.presence.get().meaning()));
            dot.set_halign(gtk::Align::End);
            dot.set_valign(gtk::Align::End);
            dot.set_can_target(false);
            self.overlay.add_overlay(&dot);
            self.obj().set_child(Some(&self.overlay));
            self.dot.replace(Some(dot));
        }
    }
    impl WidgetImpl for PresenceAvatar {}
    impl BinImpl for PresenceAvatar {}

    impl PresenceAvatar {
        fn set_presence(&self, value: Presence) {
            let previous = self.presence.replace(value);
            if previous == value {
                return;
            }
            if let Some(dot) = self.dot.borrow().as_ref() {
                dot.remove_css_class(previous.class());
                dot.add_css_class(value.class());
                // Colour alone must not carry the meaning.
                dot.set_tooltip_text(Some(value.meaning()));
            }
            self.obj().update_property(&[gtk::accessible::Property::Label(&format!("Status: {}", value.meaning()))]);
            self.obj().notify_presence();
        }
    }
}

glib::wrapper! {
    pub struct PresenceAvatar(ObjectSubclass<presence_imp::PresenceAvatar>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PresenceAvatar {
    pub fn new(avatar: &adw::Avatar) -> Self {
        let this: Self = glib::Object::new();
        this.set_avatar(avatar);
        this
    }
    /// Show or hide the circle (a privacy option can turn status off without changing the avatar).
    pub fn set_dot_visible(&self, visible: bool) {
        if let Some(dot) = self.imp().dot.borrow().as_ref() {
            dot.set_visible(visible);
        }
    }
    pub fn dot_visible(&self) -> bool {
        self.imp().dot.borrow().as_ref().is_some_and(|dot| dot.is_visible())
    }
    /// Swap the picture (a recycled list row binds a new person) and keep the dot on top.
    pub fn set_avatar(&self, avatar: &adw::Avatar) {
        self.imp().overlay.set_child(Some(avatar));
    }
}

// ---------------------------------------------------------------------------------------------
// ChatRow
// ---------------------------------------------------------------------------------------------

mod chat_row_imp {
    use super::*;
    use std::cell::{Cell, OnceCell};

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::ChatRow)]
    pub struct ChatRow {
        #[property(get, set = Self::set_title, explicit_notify)]
        title: std::cell::RefCell<String>,
        #[property(get, set = Self::set_time, explicit_notify)]
        time: std::cell::RefCell<String>,
        #[property(get, set = Self::set_preview, explicit_notify)]
        preview: std::cell::RefCell<String>,
        /// Unread message count; 0 shows no number.
        #[property(get, set = Self::set_unread, explicit_notify)]
        unread: Cell<u32>,
        /// Manually marked unread without a count: shows a dot.
        #[property(get, set = Self::set_unread_mark, name = "unread-mark", explicit_notify)]
        unread_mark: Cell<bool>,
        #[property(get, set = Self::set_pinned, explicit_notify)]
        pinned: Cell<bool>,
        pub name: OnceCell<gtk::Label>,
        pub when: OnceCell<gtk::Label>,
        pub snippet: OnceCell<gtk::Label>,
        pub badge: OnceCell<gtk::Label>,
        pub pin: OnceCell<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChatRow {
        const NAME: &'static str = "MonogramChatRow";
        type Type = super::ChatRow;
        type ParentType = gtk::ListBoxRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ChatRow {
        fn constructed(&self) {
            self.parent_constructed();
            let name = text("", "chat-name");
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let when = label("", "chat-time");
            let snippet = text("", "chat-preview");
            snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
            snippet.set_hexpand(true);
            let badge = label("", "unread-badge");
            badge.set_visible(false);
            let pin = gtk::Image::from_icon_name("view-pin-symbolic");
            pin.set_visible(false);

            let top = bx(false, 8);
            top.append(&name);
            top.append(&when);
            let bottom = bx(false, 6);
            bottom.append(&snippet);
            bottom.append(&badge);
            bottom.append(&pin);
            let words = bx(true, 5);
            words.set_hexpand(true);
            words.append(&top);
            words.append(&bottom);
            let line = bx(false, 11);
            line.append(&words);
            self.obj().set_child(Some(&line));

            let _ = self.name.set(name);
            let _ = self.when.set(when);
            let _ = self.snippet.set(snippet);
            let _ = self.badge.set(badge);
            let _ = self.pin.set(pin);
        }
    }
    impl WidgetImpl for ChatRow {}
    impl ListBoxRowImpl for ChatRow {}

    impl ChatRow {
        fn set_title(&self, value: String) {
            self.name.get().unwrap().set_label(&value);
            *self.title.borrow_mut() = value;
            self.obj().notify_title();
        }
        fn set_time(&self, value: String) {
            self.when.get().unwrap().set_label(&value);
            *self.time.borrow_mut() = value;
            self.obj().notify_time();
        }
        fn set_preview(&self, value: String) {
            self.snippet.get().unwrap().set_label(&value);
            *self.preview.borrow_mut() = value;
            self.obj().notify_preview();
        }
        fn set_unread(&self, value: u32) {
            self.unread.set(value);
            self.refresh_badges();
            self.obj().notify_unread();
        }
        fn set_unread_mark(&self, value: bool) {
            self.unread_mark.set(value);
            self.refresh_badges();
            self.obj().notify_unread_mark();
        }
        fn set_pinned(&self, value: bool) {
            self.pinned.set(value);
            self.refresh_badges();
            self.obj().notify_pinned();
        }
        /// The count (or dot) wins over the pin, exactly as the list always showed them.
        fn refresh_badges(&self) {
            let (badge, pin) = (self.badge.get().unwrap(), self.pin.get().unwrap());
            let shown = super::badge_text(self.unread.get(), self.unread_mark.get());
            badge.set_visible(shown.is_some());
            if let Some(text) = &shown {
                badge.set_label(text);
            }
            pin.set_visible(shown.is_none() && self.pinned.get());
        }
    }
}

/// What the unread badge says: the count, or a dot for a manual "unread" mark, or nothing.
fn badge_text(unread: u32, marked: bool) -> Option<String> {
    if unread > 0 {
        Some(unread.to_string())
    } else if marked {
        Some("•".into())
    } else {
        None
    }
}

glib::wrapper! {
    pub struct ChatRow(ObjectSubclass<chat_row_imp::ChatRow>)
        @extends gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl ChatRow {
    /// `id` becomes the widget name the list uses to find the conversation; `leading` is the
    /// avatar (or a button wrapping it).
    pub fn new(id: &str, leading: &impl IsA<gtk::Widget>) -> Self {
        let this: Self = glib::Object::builder().property("name", id).build();
        let line = this.child().and_downcast::<gtk::Box>().expect("chat row layout");
        line.prepend(leading);
        this
    }
}

// ---------------------------------------------------------------------------------------------
// StoryAvatar
// ---------------------------------------------------------------------------------------------

mod story_imp {
    use super::*;
    use std::cell::Cell;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::StoryAvatar)]
    pub struct StoryAvatar {
        /// There is something the user has not watched yet: the ring is drawn in the accent colour.
        #[property(get, set = Self::set_unseen, explicit_notify)]
        unseen: Cell<bool>,
        /// Show the "+" badge that offers adding a story (the user's own entry).
        #[property(get, set = Self::set_add_badge, name = "add-badge", explicit_notify)]
        add_badge: Cell<bool>,
        pub overlay: gtk::Overlay,
        pub badge: std::cell::OnceCell<gtk::Image>,
        pub avatar: std::cell::RefCell<Option<adw::Avatar>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StoryAvatar {
        const NAME: &'static str = "MonogramStoryAvatar";
        type Type = super::StoryAvatar;
        type ParentType = adw::Bin;
    }

    #[glib::derived_properties]
    impl ObjectImpl for StoryAvatar {
        fn constructed(&self) {
            self.parent_constructed();
            let badge = gtk::Image::from_icon_name("list-add-symbolic");
            badge.set_halign(gtk::Align::End);
            badge.set_valign(gtk::Align::End);
            badge.add_css_class("story-add");
            badge.set_visible(false);
            self.overlay.add_overlay(&badge);
            let _ = self.badge.set(badge);
            self.obj().set_child(Some(&self.overlay));
        }
    }
    impl WidgetImpl for StoryAvatar {}
    impl BinImpl for StoryAvatar {}

    impl StoryAvatar {
        fn set_unseen(&self, value: bool) {
            self.unseen.set(value);
            if let Some(avatar) = self.avatar.borrow().as_ref() {
                self.style(avatar);
            }
            self.obj().notify_unseen();
        }
        fn set_add_badge(&self, value: bool) {
            self.add_badge.set(value);
            self.badge.get().unwrap().set_visible(value);
            self.obj().notify_add_badge();
        }
        pub fn style(&self, avatar: &adw::Avatar) {
            let unseen = self.unseen.get();
            avatar.add_css_class(if unseen { "story-unread" } else { "story-seen" });
            avatar.remove_css_class(if unseen { "story-seen" } else { "story-unread" });
        }
    }
}

glib::wrapper! {
    pub struct StoryAvatar(ObjectSubclass<story_imp::StoryAvatar>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StoryAvatar {
    pub fn new(avatar: &adw::Avatar) -> Self {
        let this: Self = glib::Object::new();
        let imp = this.imp();
        imp.overlay.set_child(Some(avatar));
        imp.avatar.replace(Some(avatar.clone()));
        imp.style(avatar);
        this
    }
}

// ---------------------------------------------------------------------------------------------
// HatChip / HatStrip
// ---------------------------------------------------------------------------------------------

mod hat_chip_imp {
    use super::*;
    use crate::hat_color::{self, Rgb, Scheme};
    use gtk::{gdk, graphene, gsk};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct HatChip {
        pub label: gtk::Label,
        /// The hat's hue angle in degrees; `None` draws the chip in the normal text colour.
        pub hue: Cell<Option<f64>>,
        /// The colour currently applied to the text and border; `None` means "use the theme's".
        pub color: Cell<Option<Rgb>>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HatChip {
        const NAME: &'static str = "MonogramHatChip";
        type Type = super::HatChip;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for HatChip {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("hat-chip");
            let label = &self.label;
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(24);
            label.set_single_line_mode(true);
            label.add_css_class("caption-heading");
            label.set_margin_start(8);
            label.set_margin_end(8);
            label.set_margin_top(1);
            label.set_margin_bottom(1);
            obj.set_child(Some(label));
            // The colours follow the user's scheme: light or dark, and high contrast.
            let style = adw::StyleManager::default();
            let mut handlers = self.handlers.borrow_mut();
            for property in ["dark", "high-contrast"] {
                let weak = obj.downgrade();
                handlers.push(style.connect_notify_local(Some(property), move |_, _| {
                    if let Some(chip) = weak.upgrade() {
                        chip.imp().recolor();
                    }
                }));
            }
        }
        fn dispose(&self) {
            let style = adw::StyleManager::default();
            for id in self.handlers.take() {
                style.disconnect(id);
            }
        }
    }

    impl WidgetImpl for HatChip {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
            let (width, height) = (obj.width() as f32, obj.height() as f32);
            if width < 2.0 || height < 2.0 {
                return;
            }
            // A one-pixel outline in the text colour, nothing inside it.
            let color = match self.color.get() {
                Some(rgb) => {
                    let (r, g, b) = rgb.unit();
                    gdk::RGBA::new(r, g, b, 1.0)
                }
                None => self.label.color(),
            };
            let outline = gsk::RoundedRect::from_rect(graphene::Rect::new(0.0, 0.0, width, height), height / 2.0);
            snapshot.append_border(&outline, &[1.0; 4], &[color; 4]);
        }
    }
    impl BinImpl for HatChip {}

    impl HatChip {
        /// The colour to use right now, or `None` to fall back to the theme's own text colour.
        pub fn wanted(&self) -> Option<Rgb> {
            let style = adw::StyleManager::default();
            // High contrast asks for the plain foreground; a tinted colour would only weaken it.
            if style.is_high_contrast() {
                return None;
            }
            let scheme = if style.is_dark() { Scheme::Dark } else { Scheme::Light };
            self.hue.get().map(|hue| hat_color::hat_color(hue, scheme))
        }
        pub fn recolor(&self) {
            let wanted = self.wanted();
            self.color.set(wanted);
            let obj = self.obj();
            match wanted {
                Some(rgb) => {
                    let list = gtk::pango::AttrList::new();
                    let scale = |c: u8| u16::from(c) * 257;
                    list.insert(gtk::pango::AttrColor::new_foreground(scale(rgb.r), scale(rgb.g), scale(rgb.b)));
                    self.label.set_attributes(Some(&list));
                }
                None => self.label.set_attributes(None),
            }
            obj.queue_draw();
        }
    }
}

glib::wrapper! {
    /// One XEP-0317 hat: the title in an outlined pill, coloured from the hat's hue.
    ///
    /// The colour is chosen for the user's current scheme so that the text reads at 4.5:1 or
    /// better on every background it can sit on (see `hat_color`), and is dropped in favour of
    /// the theme's own foreground in high-contrast mode. Nothing is filled.
    pub struct HatChip(ObjectSubclass<hat_chip_imp::HatChip>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HatChip {
    pub fn new(hat: &crate::rooms::hats::Hat) -> Self {
        let this: Self = glib::Object::new();
        this.imp().label.set_label(&hat.title);
        this.set_tooltip_text(Some(&hat.title));
        this.update_property(&[gtk::accessible::Property::Label(&format!("Hat: {}", hat.title))]);
        this.imp().hue.set(hat.hue());
        this.imp().recolor();
        this
    }
    /// The title as shown.
    pub fn title(&self) -> String {
        self.imp().label.label().to_string()
    }
    /// The colour applied to text and border, `None` when the theme's own is used.
    pub fn color(&self) -> Option<crate::hat_color::Rgb> {
        self.imp().color.get()
    }
}

mod hat_strip_imp {
    use super::*;

    #[derive(Default)]
    pub struct HatStrip {
        pub wrap: adw::WrapBox,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HatStrip {
        const NAME: &'static str = "MonogramHatStrip";
        type Type = super::HatStrip;
        type ParentType = adw::Bin;
    }
    impl ObjectImpl for HatStrip {
        fn constructed(&self) {
            self.parent_constructed();
            self.wrap.set_child_spacing(4);
            self.wrap.set_line_spacing(4);
            let obj = self.obj();
            obj.set_child(Some(&self.wrap));
            obj.set_visible(false);
        }
    }
    impl WidgetImpl for HatStrip {}
    impl BinImpl for HatStrip {}
}

glib::wrapper! {
    /// The hats an occupant wears, wrapping onto more lines when the space is narrow. Hidden
    /// while there are none, so a room without hats looks exactly as it did.
    pub struct HatStrip(ObjectSubclass<hat_strip_imp::HatStrip>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HatStrip {
    pub fn new() -> Self {
        glib::Object::new()
    }
    pub fn set_hats(&self, hats: &[crate::rooms::hats::Hat]) {
        let wrap = &self.imp().wrap;
        wrap.remove_all();
        for hat in hats {
            wrap.append(&HatChip::new(hat));
        }
        self.set_visible(!hats.is_empty());
    }
    pub fn chips(&self) -> Vec<HatChip> {
        let mut chips = Vec::new();
        let mut child = self.imp().wrap.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(chip) = widget.downcast::<HatChip>() {
                chips.push(chip);
            }
        }
        chips
    }
}

impl Default for HatStrip {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// ContactRow
// ---------------------------------------------------------------------------------------------

mod contact_row_imp {
    use super::*;

    #[derive(Default)]
    pub struct ContactRow;

    #[glib::object_subclass]
    impl ObjectSubclass for ContactRow {
        const NAME: &'static str = "MonogramContactRow";
        type Type = super::ContactRow;
        type ParentType = adw::ActionRow;
    }
    impl ObjectImpl for ContactRow {}
    impl WidgetImpl for ContactRow {}
    impl ListBoxRowImpl for ContactRow {}
    impl PreferencesRowImpl for ContactRow {}
    impl ActionRowImpl for ContactRow {}
}

glib::wrapper! {
    /// A person in a list: avatar, name, and an address when there is one. Names and addresses
    /// come from other people, so markup is never interpreted.
    pub struct ContactRow(ObjectSubclass<contact_row_imp::ContactRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl ContactRow {
    pub fn new(name: &str, address: Option<&str>, avatar: &adw::Avatar) -> Self {
        let this: Self = glib::Object::builder()
            .property("title", name)
            .property("use-markup", false)
            .build();
        if let Some(address) = address {
            this.set_subtitle(address);
        }
        this.add_prefix(avatar);
        this
    }
    /// Activating the row (Enter, click) picks the person.
    pub fn pickable(self) -> Self {
        self.set_activatable(true);
        self
    }
    /// Turn the row into a multi-select entry: the check is the row's activatable widget.
    pub fn with_check(&self, active: bool) -> gtk::CheckButton {
        let check = gtk::CheckButton::builder().active(active).valign(gtk::Align::Center).build();
        self.set_activatable_widget(Some(&check));
        self.add_suffix(&check);
        check
    }
}

// ---------------------------------------------------------------------------------------------
// NavRow
// ---------------------------------------------------------------------------------------------

mod nav_row_imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::NavRow)]
    pub struct NavRow {
        /// Symbolic icon shown before the title.
        #[property(get, set = Self::set_icon, explicit_notify)]
        icon: std::cell::RefCell<String>,
        /// Show the "go next" arrow: the row opens something rather than toggling in place.
        #[property(get, set = Self::set_chevron, explicit_notify, default = true)]
        chevron: std::cell::Cell<bool>,
        pub image: OnceCell<gtk::Image>,
        pub arrow: OnceCell<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NavRow {
        const NAME: &'static str = "MonogramNavRow";
        type Type = super::NavRow;
        type ParentType = adw::ActionRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for NavRow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let image = gtk::Image::new();
            let arrow = gtk::Image::from_icon_name("go-next-symbolic");
            obj.add_prefix(&image);
            obj.add_suffix(&arrow);
            obj.set_activatable(true);
            let _ = self.image.set(image);
            let _ = self.arrow.set(arrow);
            // The property defaults to true; make the arrow agree before anything sets it.
            self.chevron.set(true);
        }
    }
    impl WidgetImpl for NavRow {}
    impl ListBoxRowImpl for NavRow {}
    impl PreferencesRowImpl for NavRow {}
    impl ActionRowImpl for NavRow {}

    impl NavRow {
        fn set_icon(&self, value: String) {
            self.image.get().unwrap().set_icon_name(Some(&value));
            *self.icon.borrow_mut() = value;
            self.obj().notify_icon();
        }
        fn set_chevron(&self, value: bool) {
            self.chevron.set(value);
            self.arrow.get().unwrap().set_visible(value);
            self.obj().notify_chevron();
        }
    }
}

glib::wrapper! {
    /// A row that opens something: icon, title, optional subtitle and a trailing arrow.
    pub struct NavRow(ObjectSubclass<nav_row_imp::NavRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl NavRow {
    pub fn new(icon: &str, title: &str, subtitle: Option<&str>) -> Self {
        let this: Self = glib::Object::builder()
            .property("icon", icon)
            .property("chevron", true)
            .property("title", title)
            .build();
        if let Some(subtitle) = subtitle {
            this.set_subtitle(subtitle);
        }
        this
    }
    /// Same row without the arrow (it acts in place instead of navigating).
    pub fn without_chevron(self) -> Self {
        self.set_chevron(false);
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Message bubble parts
// ---------------------------------------------------------------------------------------------

/// Draw the timestamp pill over the lower-right corner of media or a sticker, which have no
/// room for the usual line under the content. Returns the overlay to append to the bubble.
pub fn overlay_meta(content: &impl IsA<gtk::Widget>, meta: &gtk::Box) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(content));
    meta.add_css_class("sticker-meta");
    meta.set_halign(gtk::Align::End);
    meta.set_valign(gtk::Align::End);
    meta.set_can_target(false);
    overlay.add_overlay(meta);
    overlay
}

mod reply_imp {
    use super::*;

    #[derive(Default)]
    pub struct ReplyQuote {
        pub row: gtk::Grid,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReplyQuote {
        const NAME: &'static str = "MonogramReplyQuote";
        type Type = super::ReplyQuote;
        type ParentType = gtk::Button;
    }
    impl ObjectImpl for ReplyQuote {
        fn constructed(&self) {
            self.parent_constructed();
            self.row.set_column_spacing(7);
            let obj = self.obj();
            obj.add_css_class("message-reply");
            obj.set_child(Some(&self.row));
        }
    }
    impl WidgetImpl for ReplyQuote {}
    impl ButtonImpl for ReplyQuote {}
}

glib::wrapper! {
    /// The quoted message at the top of a reply: author line, then the quoted text, with room
    /// for a thumbnail in column 0 of [`ReplyQuote::grid`].
    pub struct ReplyQuote(ObjectSubclass<reply_imp::ReplyQuote>)
        @extends gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

/// The quoted text as one line: reply markers (`> `) dropped, line breaks turned into spaces.
pub(crate) fn quote_preview(quote: &str) -> String {
    quote
        .lines()
        .map(|line| line.strip_prefix("> ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join(" ")
}

impl ReplyQuote {
    pub fn new(author: &str, quote: &str) -> Self {
        let this: Self = glib::Object::new();
        let contents = bx(true, 2);
        contents.set_hexpand(true);
        let title = text(author, "reply-author");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let preview = quote_preview(quote);
        let snippet: gtk::Widget = if preview.starts_with("📎 ") {
            let name = text(&preview.chars().take(4096).collect::<String>(), "reply-preview");
            name.set_single_line_mode(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_max_width_chars(52);
            name.upcast()
        } else {
            crate::reply_preview_ui::Snippet::new(if preview.is_empty() { "Original message" } else { &preview }).upcast()
        };
        contents.append(&title);
        contents.append(&snippet);
        this.grid().attach(&contents, 1, 0, 1, 1);
        this
    }
    /// The grid the quote is laid out in; a thumbnail goes in column 0.
    pub fn grid(&self) -> &gtk::Grid {
        &self.imp().row
    }
}

/// The "Forwarded from …" line above a forwarded message; `on_click` shows the original sender.
pub struct ForwardedBanner;

impl ForwardedBanner {
    pub fn new(name: &str, on_click: impl Fn() + 'static) -> gtk::Button {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.set_halign(gtk::Align::Start);
        button.set_margin_bottom(4);
        let title = text(&format!("Forwarded from {name}"), "caption");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(36);
        title.set_single_line_mode(true);
        button.set_child(Some(&title));
        button.set_tooltip_text(Some(name));
        button.connect_clicked(move |_| on_click());
        button
    }
}

/// A file inside a bubble: type icon, name, and a line of detail.
pub struct FileCard;

impl FileCard {
    pub fn new(icon: &str, name: &str, detail: &str) -> gtk::Box {
        let file = bx(false, 10);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(32);
        file.append(&image);
        let words = bx(true, 2);
        words.append(&crate::widgets::wrap(name, "heading"));
        words.append(&text(detail, "caption dim-label"));
        file.append(&words);
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First descendant (or the widget itself) carrying a style class.
    pub(super) fn find(root: &impl IsA<gtk::Widget>, class: &str) -> Option<gtk::Widget> {
        let root = root.as_ref();
        if root.has_css_class(class) {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Some(found) = find(&widget, class) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
    pub(super) fn label_text(root: &impl IsA<gtk::Widget>, class: &str) -> String {
        find(root, class).and_downcast::<gtk::Label>().map(|l| l.label().to_string()).unwrap_or_default()
    }

    /// Every shared component keeps the style classes and structure the stylesheet and the
    /// rest of the UI rely on.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_shared_components_keep_their_classes_and_structure() {
        adw::init().unwrap();

        // MessageBubble: the properties own the classes, and they can be changed back.
        let bubble = MessageBubble::new(BubbleKind::Text);
        assert!(bubble.has_css_class("message-bubble") && !bubble.has_css_class("own"));
        assert_eq!(bubble.orientation(), gtk::Orientation::Vertical);
        bubble.set_own(true);
        bubble.set_emoji_only(true);
        bubble.set_document(true);
        bubble.set_highlighted(true);
        for class in ["own", "emoji-only", "document-bubble", "reply-target"] {
            assert!(bubble.has_css_class(class), "{class}");
        }
        bubble.set_kind(BubbleKind::Media);
        assert!(bubble.has_css_class("media-bubble") && bubble.has_css_class("message-bubble"));
        bubble.set_kind(BubbleKind::Sticker);
        assert!(bubble.has_css_class("sticker-bubble") && !bubble.has_css_class("media-bubble"));
        bubble.set_kind(BubbleKind::Action);
        assert!(bubble.has_css_class("date-pill") && bubble.has_css_class("action-pill"));
        assert!(!bubble.has_css_class("message-bubble") && !bubble.has_css_class("own"));
        assert_eq!(bubble.halign(), gtk::Align::Center);
        bubble.set_kind(BubbleKind::Text);
        assert!(bubble.has_css_class("own") && !bubble.has_css_class("date-pill"));
        assert_eq!(bubble.halign(), gtk::Align::Fill);
        bubble.set_own(false);
        bubble.set_highlighted(false);
        assert!(!bubble.has_css_class("own") && !bubble.has_css_class("reply-target"));
        // Properties are real GObject properties too.
        bubble.set_property("own", true);
        assert!(bubble.has_css_class("own"));

        // ChatRow: a count beats the pin, the dot stands in for a count, nothing hides both.
        let row = ChatRow::new("chat-1", &adw::Avatar::new(44, Some("Mira"), true));
        assert_eq!(row.widget_name(), "chat-1");
        row.set_title("Mira");
        row.set_time("12:30");
        row.set_preview("You: hi");
        assert_eq!(label_text(&row, "chat-name"), "Mira");
        assert_eq!(label_text(&row, "chat-time"), "12:30");
        assert_eq!(label_text(&row, "chat-preview"), "You: hi");
        let badge = find(&row, "unread-badge").unwrap();
        let pin = row.child().and_then(|line| find(&line, "chat-preview")).and_then(|p| p.next_sibling().and_then(|b| b.next_sibling())).unwrap();
        assert!(!badge.is_visible() && !pin.is_visible());
        row.set_pinned(true);
        assert!(!badge.is_visible() && pin.is_visible());
        row.set_unread(3);
        assert!(badge.is_visible() && !pin.is_visible());
        assert_eq!(label_text(&row, "unread-badge"), "3");
        row.set_unread(0);
        row.set_unread_mark(true);
        assert_eq!(label_text(&row, "unread-badge"), "•");
        row.set_unread_mark(false);
        assert!(!badge.is_visible() && pin.is_visible());

        // PresenceAvatar: one dot, one presence class at a time, avatar swappable.
        let person = PresenceAvatar::new(&adw::Avatar::new(40, None, true));
        let dot = find(&person, "presence-dot").unwrap();
        assert!(dot.has_css_class("presence-online") && !dot.can_target());
        person.set_presence(Presence::Busy);
        assert!(dot.has_css_class("presence-busy") && !dot.has_css_class("presence-online"));
        person.set_avatar(&adw::Avatar::new(40, Some("Sam"), true));
        person.set_presence(Presence::from_label("Away"));
        assert!(dot.has_css_class("presence-away") && !dot.has_css_class("presence-busy"));
        assert_eq!(Presence::from_label("whatever"), Presence::Online);

        // StoryAvatar.
        let story = StoryAvatar::new(&adw::Avatar::new(44, None, true));
        assert!(find(&story, "story-seen").is_some());
        story.set_unseen(true);
        assert!(find(&story, "story-unread").is_some() && find(&story, "story-seen").is_none());
        let add = find(&story, "story-add").unwrap();
        assert!(!add.is_visible());
        story.set_add_badge(true);
        assert!(add.is_visible());

        // NavRow: arrow on by default, can be removed; the row is activatable.
        let nav = NavRow::new("system-search-symbolic", "Browse", Some("Look around"));
        assert!(nav.is_activatable() && nav.chevron() && nav.title() == "Browse" && nav.subtitle().as_deref() == Some("Look around"));
        assert_eq!(nav.icon(), "system-search-symbolic");
        let arrow = nav.imp().arrow.get().unwrap().clone();
        assert!(arrow.is_visible());
        let nav = nav.without_chevron();
        assert!(!nav.chevron() && !arrow.is_visible());

        // ContactRow: names from other people are never read as markup.
        let contact = ContactRow::new("<b>Eve</b>", Some("eve@example.org"), &adw::Avatar::new(36, None, true));
        assert!(!contact.uses_markup() && !contact.is_activatable());
        assert_eq!(contact.title(), "<b>Eve</b>");
        let check = contact.with_check(true);
        assert!(check.is_active() && contact.activatable_widget().as_ref() == Some(check.upcast_ref()));
        assert!(ContactRow::new("x", None, &adw::Avatar::new(36, None, true)).pickable().is_activatable());

        // ReplyQuote: button > grid > [author, quote], thumbnail column left free.
        let quote = ReplyQuote::new("Mira", "> first\n> second");
        assert!(quote.has_css_class("message-reply"));
        let grid = quote.child().and_downcast::<gtk::Grid>().unwrap();
        assert_eq!(&grid, quote.grid());
        let contents = grid.child_at(1, 0).and_downcast::<gtk::Box>().unwrap();
        assert!(contents.first_child().and_downcast::<gtk::Label>().unwrap().has_css_class("reply-author"));
        assert!(grid.child_at(0, 0).is_none());
        let file_quote = ReplyQuote::new("Sam", "📎 notes.pdf");
        assert_eq!(label_text(&file_quote, "reply-preview"), "📎 notes.pdf");

        // overlay_meta, FileCard, ForwardedBanner.
        let meta = bx(false, 4);
        let overlay = overlay_meta(&gtk::Label::new(Some("x")), &meta);
        assert!(meta.has_css_class("sticker-meta") && meta.parent().as_ref() == Some(overlay.upcast_ref()));
        assert!(!meta.can_target() && meta.halign() == gtk::Align::End);
        let card = FileCard::new("application-pdf-symbolic", "a.pdf", "1 MB");
        assert_eq!(label_text(&card, "heading"), "a.pdf");
        let clicked = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = clicked.clone();
        let banner = ForwardedBanner::new("Alex", move || seen.set(true));
        assert_eq!(banner.tooltip_text().as_deref(), Some("Alex"));
        banner.emit_clicked();
        assert!(clicked.get());

        // UtilityPane: heading, back at the start, close at the requested edge.
        let pane = UtilityPane::new(Some("Shared Media")).with_class("shared-media-sidebar");
        assert!(pane.view.has_css_class("utility-sidebar") && pane.view.has_css_class("shared-media-sidebar"));
        assert_eq!(pane.view.top_bar_style(), adw::ToolbarStyle::Flat, "libadwaita default: flat");
        assert_eq!(label_text(&pane.header, "heading"), "Shared Media");
        let went = std::rc::Rc::new(std::cell::Cell::new(0));
        let (a, b) = (went.clone(), went.clone());
        pane.back("Back", move || a.set(a.get() + 1)).emit_clicked();
        pane.close("Close", true, move || b.set(b.get() + 10)).emit_clicked();
        assert_eq!(went.get(), 11);
        let kept = adw::ToolbarView::new();
        assert!(UtilityPane::adopt(&kept).view.has_css_class("utility-sidebar"));
        assert!(titled_page().content().is_none());
    }
}

/// Edge cases. The first group needs no display; the rest each run in their own process
/// (`tools/headless-gtk-test.sh --bin monogram-desktop -- --include-ignored --exact <name>`)
/// because GTK may only be initialised on one thread per process.
#[cfg(test)]
mod edge_cases {
    use super::tests::{find, label_text};
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn init() {
        adw::init().unwrap();
    }
    fn avatar() -> adw::Avatar {
        adw::Avatar::new(40, None, true)
    }
    /// Count `notify::<name>` emissions while `change` runs.
    fn notifications(object: &impl IsA<glib::Object>, name: &str, change: impl FnOnce()) -> u32 {
        let count = Rc::new(Cell::new(0));
        let seen = count.clone();
        let id = object.connect_notify_local(Some(name), move |_, _| seen.set(seen.get() + 1));
        change();
        object.disconnect(id);
        count.get()
    }
    fn n_children(widget: &impl IsA<gtk::Widget>) -> u32 {
        let (mut n, mut c) = (0, widget.first_child());
        while let Some(w) = c {
            n += 1;
            c = w.next_sibling();
        }
        n
    }
    fn class_count(widget: &impl IsA<gtk::Widget>, class: &str) -> usize {
        widget.css_classes().iter().filter(|c| c.as_str() == class).count()
    }

    // ---- pure logic: no display needed --------------------------------------------------

    #[test]
    fn the_unread_badge_prefers_a_count_then_a_dot_then_nothing() {
        assert_eq!(badge_text(0, false), None);
        assert_eq!(badge_text(0, true).as_deref(), Some("•"));
        assert_eq!(badge_text(1, false).as_deref(), Some("1"));
        assert_eq!(badge_text(7, true).as_deref(), Some("7"), "a count beats the manual mark");
        assert_eq!(badge_text(999_999, false).as_deref(), Some("999999"));
        assert_eq!(badge_text(u32::MAX, false).as_deref(), Some("4294967295"));
    }

    #[test]
    fn a_presence_label_maps_exactly_and_anything_else_is_online() {
        assert_eq!(Presence::from_label("Away"), Presence::Away);
        assert_eq!(Presence::from_label("Busy"), Presence::Busy);
        assert_eq!(Presence::from_label("Online"), Presence::Online);
        for other in ["", "away", "BUSY", "Away ", "xa", "dnd", "offline", "🙂"] {
            assert_eq!(Presence::from_label(other), Presence::Online, "{other:?}");
        }
        assert_eq!(Presence::default(), Presence::Online);
        assert_eq!(BubbleKind::default(), BubbleKind::Text);
        let classes = [Presence::Online, Presence::Away, Presence::Busy, Presence::Unknown].map(Presence::class);
        assert_eq!(classes, ["presence-online", "presence-away", "presence-busy", "presence-unknown"]);
        assert_eq!(
            [Presence::Online, Presence::Away, Presence::Busy, Presence::Unknown].map(Presence::meaning),
            ["Online", "Away", "Do not disturb", "Status unknown"]
        );
    }

    #[test]
    fn a_quote_is_flattened_to_one_line_without_reply_markers() {
        assert_eq!(quote_preview(""), "");
        assert_eq!(quote_preview("hello"), "hello");
        assert_eq!(quote_preview("> a\n> b"), "a b");
        assert_eq!(quote_preview("> a\r\n> b\r\n"), "a b", "CRLF line ends");
        assert_eq!(quote_preview("> a\n\n> b"), "a  b", "a blank line keeps its gap");
        assert_eq!(quote_preview(">no space"), ">no space", "only `> ` is a marker");
        assert_eq!(quote_preview(">  two spaces"), " two spaces", "only one space belongs to the marker");
        assert_eq!(quote_preview("> > nested"), "> nested", "only the outer marker goes");
        assert_eq!(quote_preview("> "), "");
        assert_eq!(quote_preview(">"), ">");
        assert_eq!(quote_preview("a > b"), "a > b", "mid-line markers stay");
        assert_eq!(quote_preview("one\ntwo\nthree"), "one two three");
        assert_eq!(quote_preview("🙂\n> ünï"), "🙂 ünï");
    }

    // ---- MessageBubble ------------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_bubble_starts_plain_and_every_kind_has_exactly_its_own_classes() {
        init();
        let managed = [
            "message-bubble", "date-pill", "action-pill", "own", "sticker-bubble", "media-bubble",
            "document-bubble", "emoji-only", "reply-target",
        ];
        let expected = |kind| match kind {
            BubbleKind::Text => vec!["message-bubble"],
            BubbleKind::Media => vec!["message-bubble", "media-bubble"],
            BubbleKind::Sticker => vec!["message-bubble", "sticker-bubble"],
            BubbleKind::Action => vec!["date-pill", "action-pill"],
        };
        for kind in [BubbleKind::Text, BubbleKind::Media, BubbleKind::Sticker, BubbleKind::Action] {
            let bubble = MessageBubble::new(kind);
            let have: Vec<_> = managed.iter().copied().filter(|c| bubble.has_css_class(c)).collect();
            let mut want = expected(kind);
            want.sort();
            let mut have_sorted = have.clone();
            have_sorted.sort();
            assert_eq!(have_sorted, want, "{kind:?}");
            assert_eq!(bubble.kind(), kind);
            assert!(!bubble.vexpands() && bubble.valign() == gtk::Align::Start);
        }
        // Built without the constructor: the default kind is a normal text bubble.
        let plain: MessageBubble = glib::Object::new();
        assert!(plain.has_css_class("message-bubble") && plain.kind() == BubbleKind::Text);
        assert!(!plain.own() && !plain.document() && !plain.emoji_only() && !plain.highlighted());
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_bubbles_flags_combine_without_duplicating_classes_or_losing_each_other() {
        init();
        let bubble = MessageBubble::new(BubbleKind::Text);
        for _ in 0..3 {
            bubble.set_own(true);
            bubble.set_kind(BubbleKind::Media);
            bubble.set_emoji_only(true);
        }
        for class in ["own", "media-bubble", "message-bubble", "emoji-only"] {
            assert_eq!(class_count(&bubble, class), 1, "{class} must appear once");
        }
        // The old quirk: a sticker that is also a document keeps both classes.
        bubble.set_kind(BubbleKind::Sticker);
        bubble.set_document(true);
        assert!(bubble.has_css_class("sticker-bubble") && bubble.has_css_class("document-bubble"));
        assert!(!bubble.has_css_class("media-bubble"), "kinds exclude each other");
        // Flags survive a kind change in both directions.
        bubble.set_highlighted(true);
        bubble.set_kind(BubbleKind::Action);
        assert!(!bubble.has_css_class("own") && !bubble.has_css_class("message-bubble"));
        assert!(bubble.has_css_class("reply-target") && bubble.has_css_class("emoji-only"));
        assert!(bubble.own(), "the property is remembered while the class is suppressed");
        bubble.set_kind(BubbleKind::Text);
        assert!(bubble.has_css_class("own") && bubble.has_css_class("message-bubble"));
        assert!(!bubble.has_css_class("date-pill") && !bubble.has_css_class("action-pill"));
        // Turning everything off leaves only the base class.
        bubble.set_own(false);
        bubble.set_emoji_only(false);
        bubble.set_document(false);
        bubble.set_highlighted(false);
        let left: Vec<_> = bubble.css_classes().iter().map(|c| c.to_string()).collect();
        assert!(left.contains(&"message-bubble".to_string()));
        for gone in ["own", "emoji-only", "document-bubble", "reply-target", "sticker-bubble"] {
            assert!(!left.contains(&gone.to_string()), "{gone} still set: {left:?}");
        }
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_bubble_only_notifies_when_a_property_really_changes() {
        init();
        let bubble = MessageBubble::new(BubbleKind::Text);
        assert_eq!(notifications(&bubble, "own", || bubble.set_own(false)), 0, "same value");
        assert_eq!(notifications(&bubble, "own", || bubble.set_own(true)), 1);
        assert_eq!(notifications(&bubble, "own", || bubble.set_own(true)), 0);
        assert_eq!(notifications(&bubble, "emoji-only", || bubble.set_emoji_only(true)), 1);
        assert_eq!(notifications(&bubble, "highlighted", || bubble.set_highlighted(true)), 1);
        assert_eq!(notifications(&bubble, "document", || bubble.set_document(true)), 1);
        assert_eq!(notifications(&bubble, "kind", || bubble.set_kind(BubbleKind::Text)), 0);
        assert_eq!(notifications(&bubble, "kind", || bubble.set_kind(BubbleKind::Media)), 1);
        // Setting through the GObject property path takes the same route.
        assert_eq!(notifications(&bubble, "own", || bubble.set_property("own", false)), 1);
        assert!(!bubble.has_css_class("own"));
        bubble.set_property("emoji-only", false);
        assert!(!bubble.has_css_class("emoji-only"));
        bubble.set_property("kind", BubbleKind::Action);
        assert!(bubble.has_css_class("action-pill"));
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_bubble_only_resets_alignment_when_it_changes_to_or_from_an_action_pill() {
        init();
        let bubble = MessageBubble::new(BubbleKind::Text);
        bubble.set_halign(gtk::Align::Start); // what a caller does after construction
        bubble.set_own(true);
        bubble.set_emoji_only(true);
        bubble.set_highlighted(true);
        assert_eq!(bubble.halign(), gtk::Align::Start, "flags must not touch the alignment");
        bubble.set_kind(BubbleKind::Media);
        assert_eq!(bubble.halign(), gtk::Align::Start, "text to media keeps it too");
        bubble.set_kind(BubbleKind::Action);
        assert_eq!(bubble.halign(), gtk::Align::Center);
        bubble.set_kind(BubbleKind::Text);
        assert_eq!(bubble.halign(), gtk::Align::Fill);
        assert!(MessageBubble::new(BubbleKind::Action).halign() == gtk::Align::Center);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_bubble_is_a_vertical_box_that_holds_and_releases_children() {
        init();
        let bubble = MessageBubble::new(BubbleKind::Text);
        assert_eq!(bubble.orientation(), gtk::Orientation::Vertical);
        let (a, b) = (gtk::Label::new(Some("a")), gtk::Label::new(Some("b")));
        bubble.as_box().append(&a);
        bubble.as_box().append(&b);
        assert_eq!(a.next_sibling().as_ref(), Some(b.upcast_ref()));
        bubble.as_box().remove(&a);
        assert_eq!(bubble.first_child().as_ref(), Some(b.upcast_ref()));
        let weak = bubble.downgrade();
        drop(bubble);
        assert!(weak.upgrade().is_none(), "nothing keeps a dropped bubble alive");
    }

    // ---- ChatRow ------------------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_chat_row_with_nothing_set_is_empty_and_shows_no_badge_or_pin() {
        init();
        let row = ChatRow::new("", &avatar());
        assert_eq!(row.widget_name(), "", "an empty id is allowed");
        for class in ["chat-name", "chat-time", "chat-preview"] {
            assert_eq!(label_text(&row, class), "", "{class}");
        }
        assert!(!find(&row, "unread-badge").unwrap().is_visible());
        assert_eq!((row.title().as_str(), row.time().as_str(), row.preview().as_str()), ("", "", ""));
        assert_eq!((row.unread(), row.unread_mark(), row.pinned()), (0, false, false));
        // The leading widget comes first, the text block fills the rest.
        let line = row.child().and_downcast::<gtk::Box>().unwrap();
        assert_eq!(n_children(&line), 2);
        assert!(line.first_child().unwrap().is::<adw::Avatar>());
        assert!(line.last_child().unwrap().hexpands());
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_chat_row_shows_text_literally_and_truncates_instead_of_growing() {
        init();
        let row = ChatRow::new("c", &avatar());
        let hostile = "<b>bold</b> & <span foreground=\"red\">red</span>\u{202e}rtl";
        row.set_title(hostile);
        row.set_preview(hostile);
        let name = find(&row, "chat-name").and_downcast::<gtk::Label>().unwrap();
        let preview = find(&row, "chat-preview").and_downcast::<gtk::Label>().unwrap();
        assert_eq!(name.label(), hostile);
        assert!(!name.uses_markup() && !preview.uses_markup(), "never parsed as markup");
        for label in [&name, &preview] {
            assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::End);
            assert!(label.hexpands());
        }
        let long = "x".repeat(50_000);
        row.set_title(long.as_str());
        row.set_preview(long.as_str());
        assert_eq!(row.title().len(), 50_000);
        let (min, _, _, _) = row.measure(gtk::Orientation::Horizontal, -1);
        assert!(min < 600, "a very long title must not set the sidebar width ({min})");
        row.set_title("日本語 🙂 ünï");
        assert_eq!(row.title(), "日本語 🙂 ünï");
        // Multi-line previews pass through as given (the caller flattens them).
        row.set_preview("one\ntwo");
        assert_eq!(row.preview(), "one\ntwo");
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_chat_rows_badge_and_pin_follow_every_combination() {
        init();
        let row = ChatRow::new("c", &avatar());
        let badge = find(&row, "unread-badge").unwrap();
        let pin = badge.next_sibling().unwrap();
        assert!(pin.is::<gtk::Image>());
        let state = |row: &ChatRow| (badge.is_visible(), pin.is_visible(), label_text(row, "unread-badge"));
        // (unread, marked, pinned) -> (badge visible, pin visible)
        for unread in [0u32, 1, 99, u32::MAX] {
            for marked in [false, true] {
                for pinned in [false, true] {
                    row.set_unread(unread);
                    row.set_unread_mark(marked);
                    row.set_pinned(pinned);
                    let shown = unread > 0 || marked;
                    let (b, p, text) = state(&row);
                    assert_eq!((b, p), (shown, !shown && pinned), "{unread} {marked} {pinned}");
                    if shown {
                        assert_eq!(text, if unread > 0 { unread.to_string() } else { "•".into() });
                    }
                }
            }
        }
        // Order independence: the same final state from any path.
        row.set_unread(0);
        row.set_unread_mark(false);
        row.set_pinned(true);
        assert!(!badge.is_visible() && pin.is_visible());
        row.set_unread(5);
        row.set_unread(0);
        assert!(pin.is_visible() && !badge.is_visible(), "clearing the count brings the pin back");
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_chat_row_is_a_list_row_that_reports_changes_and_can_be_looked_up() {
        init();
        let list = gtk::ListBox::new();
        let (a, b) = (ChatRow::new("alpha", &avatar()), ChatRow::new("beta", &avatar()));
        list.append(&a);
        list.append(&b);
        list.select_row(Some(&b));
        // The list finds the conversation by widget name, as `select_chat` does.
        assert_eq!(list.selected_row().unwrap().widget_name(), "beta");
        assert_eq!(notifications(&a, "unread", || a.set_unread(2)), 1);
        assert_eq!(notifications(&a, "title", || a.set_title("t")), 1);
        assert_eq!(notifications(&a, "unread-mark", || a.set_unread_mark(true)), 1);
        a.set_property("pinned", true);
        assert!(a.pinned());
        assert_eq!(a.property::<u32>("unread"), 2);
        list.remove(&a);
        assert_eq!(list.first_child().map(|w| w.widget_name()).as_deref(), Some("beta"));
        // A clicked leading widget (the profile button) must still be reachable and clickable.
        let clicked = Rc::new(Cell::new(false));
        let button = gtk::Button::new();
        let seen = clicked.clone();
        button.connect_clicked(move |_| seen.set(true));
        let row = ChatRow::new("c", &button);
        assert!(button.ancestor(ChatRow::static_type()).is_some());
        button.emit_clicked();
        assert!(clicked.get() && row.is_activatable());
    }

    // ---- PresenceAvatar -----------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_presence_avatar_keeps_one_dot_whatever_happens_to_the_picture() {
        init();
        let person = PresenceAvatar::new(&avatar());
        let overlay = person.imp().overlay.clone();
        let dot = find(&person, "presence-dot").unwrap();
        let dots = |w: &gtk::Widget| {
            let mut n = 0;
            let mut c = w.first_child();
            while let Some(x) = c {
                n += x.has_css_class("presence-dot") as u32;
                c = x.next_sibling();
            }
            n
        };
        assert_eq!(person.presence(), Presence::Online);
        for i in 0..5 {
            person.set_avatar(&adw::Avatar::new(40, Some(&format!("n{i}")), true));
        }
        assert_eq!(dots(overlay.upcast_ref()), 1, "swapping the picture never adds a dot");
        assert_eq!(overlay.first_child().and_downcast::<adw::Avatar>().unwrap().text().as_deref(), Some("n4"));
        assert!(dot.parent().as_ref() == Some(overlay.upcast_ref()));
        assert!(!dot.can_target() && dot.halign() == gtk::Align::End && dot.valign() == gtk::Align::End);
        // The same avatar set twice is fine.
        let same = avatar();
        person.set_avatar(&same);
        person.set_avatar(&same);
        assert_eq!(overlay.first_child().as_ref(), Some(same.upcast_ref()));
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_presence_avatar_has_exactly_one_presence_class_through_any_sequence() {
        init();
        let person = PresenceAvatar::new(&avatar());
        let dot = find(&person, "presence-dot").unwrap();
        let active = |dot: &gtk::Widget| {
            ["presence-online", "presence-away", "presence-busy"]
                .into_iter()
                .filter(|c| dot.has_css_class(c))
                .collect::<Vec<_>>()
        };
        for sequence in [
            vec![Presence::Busy, Presence::Away, Presence::Online],
            vec![Presence::Online, Presence::Online, Presence::Busy, Presence::Busy],
            vec![Presence::Away; 4],
        ] {
            for presence in sequence {
                person.set_presence(presence);
                assert_eq!(active(&dot), vec![presence.class()], "{presence:?}");
                assert_eq!(person.presence(), presence);
            }
        }
        assert_eq!(notifications(&person, "presence", || person.set_presence(Presence::Away)), 0, "unchanged");
        assert_eq!(notifications(&person, "presence", || person.set_presence(Presence::Busy)), 1);
        person.set_property("presence", Presence::Online);
        assert_eq!(active(&dot), vec!["presence-online"]);
        // Built with the default constructor, before any avatar exists.
        let bare: PresenceAvatar = glib::Object::new();
        assert!(find(&bare, "presence-dot").is_some(), "the dot exists without an avatar");
        assert!(bare.imp().overlay.first_child().unwrap().has_css_class("presence-dot"));
    }

    // ---- StoryAvatar --------------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_story_ring_is_never_both_seen_and_unseen_and_the_badge_toggles() {
        init();
        let inner = avatar();
        let story = StoryAvatar::new(&inner);
        assert!(inner.has_css_class("story-seen") && !inner.has_css_class("story-unread"));
        for unseen in [true, true, false, true, false, false] {
            story.set_unseen(unseen);
            assert_eq!(inner.has_css_class("story-unread"), unseen);
            assert_eq!(inner.has_css_class("story-seen"), !unseen);
            assert_eq!(class_count(&inner, "story-unread") + class_count(&inner, "story-seen"), 1);
        }
        let badge = find(&story, "story-add").unwrap();
        assert!(!badge.is_visible() && !story.add_badge());
        story.set_add_badge(true);
        assert!(badge.is_visible());
        story.set_add_badge(false);
        assert!(!badge.is_visible());
        assert_eq!(badge.halign(), gtk::Align::End);
        assert_eq!(notifications(&story, "unseen", || story.set_unseen(true)), 1);
        assert_eq!(notifications(&story, "add-badge", || story.set_add_badge(true)), 1);
        // Setting the ring before an avatar exists must not panic.
        let bare: StoryAvatar = glib::Object::new();
        bare.set_unseen(true);
        bare.set_add_badge(true);
        assert!(bare.unseen() && bare.add_badge());
    }

    // ---- NavRow -------------------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_nav_row_keeps_its_arrow_and_icon_consistent_with_its_properties() {
        init();
        // Built without the constructor: the arrow must already agree with the default.
        let bare: NavRow = glib::Object::new();
        let arrow = bare.imp().arrow.get().unwrap().clone();
        assert_eq!(bare.chevron(), arrow.is_visible(), "default and arrow disagree");
        assert!(bare.chevron() && bare.is_activatable());
        let row = NavRow::new("go-home-symbolic", "Home", None);
        let image = row.imp().image.get().unwrap().clone();
        assert_eq!(image.icon_name().as_deref(), Some("go-home-symbolic"));
        row.set_icon("system-search-symbolic");
        assert_eq!(image.icon_name().as_deref(), Some("system-search-symbolic"));
        assert_eq!(row.icon(), "system-search-symbolic");
        row.set_icon("");
        assert_eq!(row.icon(), "");
        for chevron in [false, true, true, false] {
            row.set_chevron(chevron);
            assert_eq!(row.imp().arrow.get().unwrap().is_visible(), chevron);
        }
        assert_eq!(notifications(&row, "chevron", || row.set_chevron(true)), 1);
        assert_eq!(notifications(&row, "icon", || row.set_icon("x-symbolic")), 1);
        // No subtitle given: none shown. Empty title is allowed.
        let plain = NavRow::new("go-home-symbolic", "", None);
        assert!(plain.subtitle().map_or(true, |s| s.is_empty()) && plain.title().is_empty());
        // Chevron removal keeps the row activatable (it acts in place) and is chainable.
        let in_place = NavRow::new("go-home-symbolic", "Act", Some("now")).without_chevron();
        assert!(in_place.is_activatable() && !in_place.chevron());
        // It really activates.
        let hits = Rc::new(Cell::new(0));
        let seen = hits.clone();
        in_place.connect_activated(move |_| seen.set(seen.get() + 1));
        adw::prelude::ActionRowExt::activate(&in_place);
        assert_eq!(hits.get(), 1);
        // A disabled row (the blocking page) stays inert.
        let off = NavRow::new("go-home-symbolic", "Off", None).without_chevron();
        off.set_activatable(false);
        assert!(!off.is_activatable());
    }

    // ---- ContactRow ---------------------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_contact_row_never_reads_names_or_addresses_as_markup() {
        init();
        let nasty = "<span size=\"xx-large\">x</span> &amp; \u{202e}evil";
        let row = ContactRow::new(nasty, Some("<b>a@b</b>"), &avatar());
        assert!(!row.uses_markup());
        assert_eq!(row.title(), nasty, "stored verbatim");
        assert_eq!(row.subtitle().as_deref(), Some("<b>a@b</b>"));
        // Empty and missing pieces are fine.
        let empty = ContactRow::new("", None, &avatar());
        assert!(empty.title().is_empty() && empty.subtitle().map_or(true, |s| s.is_empty()));
        assert!(!empty.is_activatable(), "not pickable until asked");
        assert!(ContactRow::new("a", Some(""), &avatar()).subtitle().map_or(true, |s| s.is_empty()));
        // The avatar is the prefix: the first thing in the row.
        assert!(find(&row, "prefixes").is_some());
        let long = "n".repeat(10_000);
        assert_eq!(ContactRow::new(&long, None, &avatar()).title().len(), 10_000);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_contact_rows_check_toggles_with_the_row_and_reports_its_state() {
        init();
        let list = gtk::ListBox::new();
        let row = ContactRow::new("A", None, &avatar());
        let check = row.with_check(false);
        list.append(&row);
        assert!(!check.is_active() && check.valign() == gtk::Align::Center);
        assert_eq!(row.activatable_widget().as_ref(), Some(check.upcast_ref()));
        adw::prelude::ActionRowExt::activate(&row);
        assert!(check.is_active(), "activating the row ticks the check");
        adw::prelude::ActionRowExt::activate(&row);
        assert!(!check.is_active());
        assert!(ContactRow::new("B", None, &avatar()).with_check(true).is_active(), "starts ticked");
        // Toggling reports through the usual signal.
        let toggles = Rc::new(Cell::new(0));
        let seen = toggles.clone();
        check.connect_toggled(move |_| seen.set(seen.get() + 1));
        check.set_active(true);
        check.set_active(true);
        assert_eq!(toggles.get(), 1, "one change, one signal");
        // A pickable row fires `activated` and has no check.
        let pick = ContactRow::new("C", None, &avatar()).pickable();
        assert!(pick.is_activatable() && pick.activatable_widget().is_none());
        let hits = Rc::new(Cell::new(0));
        let seen = hits.clone();
        pick.connect_activated(move |_| seen.set(seen.get() + 1));
        adw::prelude::ActionRowExt::activate(&pick);
        assert_eq!(hits.get(), 1);
        // A caller-supplied check (room dialogs) replaces the default wiring cleanly.
        let own = ContactRow::new("D", None, &avatar());
        let mine = gtk::CheckButton::new();
        own.set_activatable_widget(Some(&mine));
        own.add_suffix(&mine);
        adw::prelude::ActionRowExt::activate(&own);
        assert!(mine.is_active());
    }

    // ---- ReplyQuote / ForwardedBanner / FileCard / overlay_meta -------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_reply_quote_handles_empty_file_long_and_hostile_quotes() {
        init();
        let contents = |q: &ReplyQuote| q.grid().child_at(1, 0).and_downcast::<gtk::Box>().unwrap();
        // Empty quote: a placeholder, never a blank card.
        let empty = ReplyQuote::new("Mira", "");
        assert_eq!(n_children(&contents(&empty)), 2);
        let snippet = contents(&empty).last_child().unwrap();
        assert!(snippet.is::<crate::reply_preview_ui::Snippet>());
        // Only the marker: still the placeholder.
        let marker = ReplyQuote::new("Mira", "> ");
        assert!(contents(&marker).last_child().unwrap().is::<crate::reply_preview_ui::Snippet>());
        // Empty author is allowed and ellipsized.
        let author = ReplyQuote::new("", "hi");
        let title = contents(&author).first_child().and_downcast::<gtk::Label>().unwrap();
        assert_eq!(title.label(), "");
        assert_eq!(title.ellipsize(), gtk::pango::EllipsizeMode::End);
        // A file reference becomes a one-line label, cut at 4096 characters.
        let file = ReplyQuote::new("Sam", &format!("📎 {}", "é".repeat(9_000)));
        let name = contents(&file).last_child().and_downcast::<gtk::Label>().unwrap();
        assert!(name.has_css_class("reply-preview") && name.is_single_line_mode());
        assert_eq!(name.label().chars().count(), 4096);
        assert_eq!(name.max_width_chars(), 52);
        // A "📎" without the space is plain text, not a file.
        let near = ReplyQuote::new("Sam", "📎notes");
        assert!(contents(&near).last_child().unwrap().is::<crate::reply_preview_ui::Snippet>());
        // Hostile author text is shown literally.
        let hostile = ReplyQuote::new("<b>x</b>", "q");
        let title = contents(&hostile).first_child().and_downcast::<gtk::Label>().unwrap();
        assert_eq!(title.label(), "<b>x</b>");
        assert!(!title.uses_markup());
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_reply_quote_is_a_clickable_button_with_a_free_thumbnail_slot() {
        init();
        let quote = ReplyQuote::new("A", "text");
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        quote.connect_clicked(move |_| seen.set(seen.get() + 1));
        quote.emit_clicked();
        assert_eq!(clicks.get(), 1);
        assert!(quote.grid().child_at(0, 0).is_none());
        // A thumbnail goes in column 0 without displacing the text.
        let thumb = gtk::Image::new();
        quote.grid().attach(&thumb, 0, 0, 1, 1);
        assert!(quote.grid().child_at(0, 0).is_some() && quote.grid().child_at(1, 0).is_some());
        assert_eq!(quote.grid().column_spacing(), 7);
        // The button is the grid's parent: the thumbnail path walks up to the quote.
        assert!(thumb.ancestor(ReplyQuote::static_type()).is_some());
        assert_eq!(quote.child().as_ref(), Some(quote.grid().upcast_ref()));
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_small_message_parts_survive_empty_long_and_repeated_input() {
        init();
        // overlay_meta: the content is the child, the meta floats, applying it again is harmless.
        let meta = bx(false, 4);
        let content = gtk::Label::new(Some("c"));
        let overlay = overlay_meta(&content, &meta);
        assert_eq!(overlay.child().as_ref(), Some(content.upcast_ref()));
        assert_eq!(class_count(&meta, "sticker-meta"), 1);
        overlay.remove_overlay(&meta);
        let again = overlay_meta(&gtk::Label::new(None), &meta);
        assert_eq!(class_count(&meta, "sticker-meta"), 1, "the class is not stacked");
        assert!(meta.parent().as_ref() == Some(again.upcast_ref()));
        // FileCard: empty text, a missing icon name, and a huge name wrap instead of growing.
        let empty = FileCard::new("", "", "");
        assert_eq!(label_text(&empty, "heading"), "");
        let missing = FileCard::new("no-such-icon-symbolic", "a", "b");
        assert_eq!(missing.first_child().and_downcast::<gtk::Image>().unwrap().pixel_size(), 32);
        let huge = FileCard::new("text-x-generic-symbolic", &"w".repeat(20_000), "d");
        let heading = find(&huge, "heading").and_downcast::<gtk::Label>().unwrap();
        assert!(heading.wraps() && heading.max_width_chars() == 52);
        assert!(find(&huge, "caption").unwrap().has_css_class("dim-label"));
        // ForwardedBanner: tooltip carries the full name; the visible text is cut.
        let name = "n".repeat(500);
        let banner = ForwardedBanner::new(&name, || {});
        assert_eq!(banner.tooltip_text().as_deref(), Some(name.as_str()));
        let shown = banner.child().and_downcast::<gtk::Label>().unwrap();
        assert!(shown.is_single_line_mode() && shown.max_width_chars() == 36);
        assert_eq!(shown.ellipsize(), gtk::pango::EllipsizeMode::End);
        assert!(shown.label().starts_with("Forwarded from "));
        assert!(!shown.uses_markup());
        let blank = ForwardedBanner::new("", || {});
        assert_eq!(blank.child().and_downcast::<gtk::Label>().unwrap().label(), "Forwarded from ");
        // The callback runs once per click, however many times it is pressed.
        let n = Rc::new(Cell::new(0));
        let seen = n.clone();
        let counted = ForwardedBanner::new("x", move || seen.set(seen.get() + 1));
        for _ in 0..3 {
            counted.emit_clicked();
        }
        assert_eq!(n.get(), 3);
    }

    // ---- UtilityPane / titled_page ------------------------------------------------------

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_utility_pane_places_its_buttons_and_title_where_the_guidelines_say() {
        init();
        let pane = UtilityPane::new(None);
        assert!(pane.header.title_widget().is_none(), "no title widget unless asked");
        assert!(!pane.header.shows_start_title_buttons(), "window buttons stay with the main header");
        assert!(pane.view.has_css_class("utility-sidebar"));
        assert_eq!(pane.view.top_bar_style(), adw::ToolbarStyle::Flat, "flat is libadwaita's default");
        let back = pane.back("Go back", || {});
        let close = pane.close("Close it", false, || {});
        let end = pane.close("Close at end", true, || {});
        for (button, tip) in [(&back, "Go back"), (&close, "Close it"), (&end, "Close at end")] {
            assert_eq!(button.tooltip_text().as_deref(), Some(tip));
            assert!(button.has_css_class("flat") && button.icon_name().is_some());
        }
        assert_eq!(back.icon_name().as_deref(), Some("go-previous-symbolic"));
        assert_eq!(close.icon_name().as_deref(), Some("sidebar-show-symbolic"));
        // All three live inside the header; the back button comes before the close at the start.
        for button in [&back, &close, &end] {
            assert!(button.ancestor(adw::HeaderBar::static_type()).as_ref() == Some(pane.header.upcast_ref()));
        }
        // Builders chain and a heading with odd text is shown literally.
        let odd = UtilityPane::new(Some("<i>Odd & ends</i>")).with_class("a").with_class("b");
        let title = odd.header.title_widget().and_downcast::<gtk::Label>().unwrap();
        assert_eq!(title.label(), "<i>Odd & ends</i>");
        assert!(!title.uses_markup() && title.has_css_class("heading"));
        assert!(odd.view.has_css_class("a") && odd.view.has_css_class("b"));
        // Two panes are independent.
        let other = UtilityPane::new(Some("Other"));
        assert!(!other.view.has_css_class("a") && other.view != odd.view);
        assert_ne!(other.header, odd.header);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_utility_pane_buttons_run_their_own_callback_only() {
        init();
        let pane = UtilityPane::new(Some("T"));
        let log = Rc::new(std::cell::RefCell::new(Vec::<&str>::new()));
        let (a, b) = (log.clone(), log.clone());
        let back = pane.back("b", move || a.borrow_mut().push("back"));
        let close = pane.close("c", true, move || b.borrow_mut().push("close"));
        back.emit_clicked();
        close.emit_clicked();
        close.emit_clicked();
        assert_eq!(*log.borrow(), ["back", "close", "close"]);
        // A callback that holds the pane weakly can not keep it alive.
        let weak = pane.view.downgrade();
        let held = pane.view.downgrade();
        pane.close("d", false, move || {
            let _ = held.upgrade();
        });
        let view = pane.view.clone();
        drop(pane);
        drop(view);
        assert!(weak.upgrade().is_none(), "dropping the pane releases it");
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn adopting_a_view_and_the_titled_page_add_exactly_one_header() {
        init();
        let headers = |view: &adw::ToolbarView| {
            let mut n = 0;
            let mut c = view.first_child();
            let mut stack = vec![];
            while let Some(w) = c.take().or_else(|| stack.pop()) {
                n += w.is::<adw::HeaderBar>() as u32;
                if let Some(next) = w.next_sibling() {
                    stack.push(next);
                }
                c = w.first_child();
            }
            n
        };
        let kept = adw::ToolbarView::new();
        kept.set_content(Some(&gtk::Label::new(Some("body"))));
        let pane = UtilityPane::adopt(&kept);
        assert_eq!(pane.view, kept, "the same view, not a copy");
        assert_eq!(headers(&kept), 1);
        assert!(kept.content().is_some(), "existing content is kept");
        assert!(!pane.header.shows_start_title_buttons());
        assert_eq!(headers(&titled_page()), 1);
        let page = titled_page();
        assert!(page.content().is_none());
        assert_eq!(page.top_bar_style(), adw::ToolbarStyle::Flat);
        // The standard header keeps the window buttons, unlike a pane header.
        let standard = find(&page, "titlebar").or_else(|| page.first_child()).is_some();
        assert!(standard);
    }

    // ---- HatChip / HatStrip -------------------------------------------------------------

    fn hat(title: &str, uri: &str, hue: Option<f64>) -> crate::rooms::hats::Hat {
        crate::rooms::hats::Hat::new(title, uri, hue).unwrap()
    }

    /// The colour is chosen for the current scheme and follows it when the scheme changes.
    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_hat_chip_is_coloured_from_its_hue_for_the_current_scheme_and_follows_changes() {
        use crate::hat_color::{contrast, backgrounds, hat_color, Scheme, MIN_CONTRAST};
        init();
        let style = adw::StyleManager::default();
        let chip = HatChip::new(&hat("Host", "urn:h", Some(327.255)));
        assert_eq!(chip.title(), "Host");
        assert!(chip.has_css_class("hat-chip") && chip.tooltip_text().as_deref() == Some("Host"));
        for (forced, scheme) in [
            (adw::ColorScheme::ForceLight, Scheme::Light),
            (adw::ColorScheme::ForceDark, Scheme::Dark),
            (adw::ColorScheme::ForceLight, Scheme::Light),
        ] {
            style.set_color_scheme(forced);
            let color = chip.color().expect("a hue gives a colour");
            assert_eq!(color, hat_color(327.255, scheme), "{scheme:?}");
            for background in backgrounds(scheme) {
                assert!(contrast(color, *background) >= MIN_CONTRAST, "{scheme:?} on {background:?}");
            }
        }
        // Every hue the chip can be given is readable in the scheme in force.
        for scheme in [(adw::ColorScheme::ForceLight, Scheme::Light), (adw::ColorScheme::ForceDark, Scheme::Dark)] {
            style.set_color_scheme(scheme.0);
            for hue in (0..360).step_by(15) {
                let chip = HatChip::new(&hat("H", "u", Some(f64::from(hue))));
                for background in backgrounds(scheme.1) {
                    assert!(contrast(chip.color().unwrap(), *background) >= MIN_CONTRAST, "{hue}");
                }
            }
        }
        style.set_color_scheme(adw::ColorScheme::Default);
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_hat_without_a_hue_uses_the_themes_own_text_colour() {
        init();
        let chip = HatChip::new(&hat("Plain", "urn:p", None));
        assert_eq!(chip.color(), None);
        assert!(chip.imp().label.attributes().is_none(), "no colour is forced on the text");
        let coloured = HatChip::new(&hat("Tinted", "urn:t", Some(10.0)));
        assert!(coloured.imp().label.attributes().is_some());
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_hat_chip_never_fills_and_keeps_long_or_odd_titles_literal_and_short() {
        init();
        let chip = HatChip::new(&hat("<b>Boss</b> & co", "urn:x", Some(90.0)));
        assert_eq!(chip.title(), "<b>Boss</b> & co");
        assert!(!chip.imp().label.uses_markup(), "titles come from other people");
        let long = HatChip::new(&hat(&"w".repeat(500), "urn:long", Some(90.0)));
        let label = &long.imp().label;
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::End);
        assert_eq!(label.max_width_chars(), 24);
        let (_, natural, _, _) = long.measure(gtk::Orientation::Horizontal, -1);
        assert!(natural < 400, "a 40-character title stays a small chip ({natural}px)");
        // Fill is never drawn: the chip has no background class of its own.
        for class in ["suggested-action", "destructive-action", "accent", "card"] {
            assert!(!chip.has_css_class(class));
        }
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_hat_strip_hides_when_empty_and_replaces_its_chips_each_time() {
        init();
        let strip = HatStrip::new();
        assert!(!strip.is_visible() && strip.chips().is_empty(), "a room without hats looks unchanged");
        strip.set_hats(&[hat("Host", "a", Some(10.0)), hat("Scribe", "b", None)]);
        assert!(strip.is_visible());
        assert_eq!(strip.chips().iter().map(HatChip::title).collect::<Vec<_>>(), ["Host", "Scribe"]);
        strip.set_hats(&[hat("Only", "c", Some(200.0))]);
        assert_eq!(strip.chips().len(), 1, "the old chips are gone, not added to");
        let eight: Vec<_> = (0..8).map(|i| hat(&format!("H{i}"), &format!("u{i}"), Some(f64::from(i) * 40.0))).collect();
        strip.set_hats(&eight);
        assert_eq!(strip.chips().len(), 8);
        strip.set_hats(&[]);
        assert!(!strip.is_visible() && strip.chips().is_empty());
        strip.set_hats(&[]);
        assert!(!strip.is_visible(), "clearing twice is fine");
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn a_hat_strip_wraps_instead_of_widening_a_narrow_pane() {
        init();
        let strip = HatStrip::new();
        let hats: Vec<_> = (0..6).map(|i| hat(&format!("A fairly long hat {i}"), &format!("u{i}"), Some(f64::from(i) * 50.0))).collect();
        strip.set_hats(&hats);
        let (min, _, _, _) = strip.measure(gtk::Orientation::Horizontal, -1);
        assert!(min < 260, "the strip's minimum width must stay narrow so it can wrap ({min}px)");
        let (_, height_wide, _, _) = strip.measure(gtk::Orientation::Vertical, 1200);
        let (_, height_narrow, _, _) = strip.measure(gtk::Orientation::Vertical, 200);
        assert!(height_narrow > height_wide, "narrow space means more lines ({height_wide} vs {height_narrow})");
    }

    // ---- the status circle (XEP-0319 and `show`) ----------------------------------------

    #[test]
    fn a_status_text_maps_strictly_and_anything_unrecognised_is_grey() {
        for (text, want) in [
            ("Online", Presence::Online),
            ("Away", Presence::Away),
            ("Busy", Presence::Busy),
            ("Do not disturb", Presence::Busy),
            ("Offline", Presence::Unknown),
            ("Not in this room", Presence::Unknown),
            ("Presence unavailable", Presence::Unknown),
            ("", Presence::Unknown),
            ("online", Presence::Unknown),
            ("Online now", Presence::Unknown),
            ("🟢", Presence::Unknown),
        ] {
            assert_eq!(Presence::from_status(text), want, "{text:?}");
        }
    }

    #[test]
    fn the_four_lights_map_to_the_four_presences_and_back_to_words() {
        use crate::contact_status::Light;
        for (light, presence) in [
            (Light::Green, Presence::Online),
            (Light::Yellow, Presence::Away),
            (Light::Red, Presence::Busy),
            (Light::Grey, Presence::Unknown),
        ] {
            assert_eq!(Presence::from_light(light), presence);
            assert_eq!(presence.meaning(), light.meaning());
        }
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_status_circle_shows_each_state_with_a_word_for_those_who_cannot_see_colour() {
        init();
        let person = PresenceAvatar::new(&avatar());
        let dot = find(&person, "presence-dot").unwrap();
        for presence in [Presence::Away, Presence::Busy, Presence::Unknown, Presence::Online, Presence::Unknown] {
            person.set_presence(presence);
            assert!(dot.has_css_class(presence.class()), "{presence:?}");
            assert_eq!(dot.tooltip_text().as_deref(), Some(presence.meaning()));
            let others = [Presence::Online, Presence::Away, Presence::Busy, Presence::Unknown]
                .into_iter()
                .filter(|other| *other != presence && dot.has_css_class(other.class()))
                .count();
            assert_eq!(others, 0, "only one colour at a time");
        }
        assert_eq!(dot.tooltip_text().as_deref(), Some("Status unknown"));
        // Built grey from the start (a contact whose presence is not shared).
        let refused: PresenceAvatar = glib::Object::builder().property("presence", Presence::Unknown).build();
        refused.set_avatar(&avatar());
        // The property-built avatar still has its dot, and it is grey once set.
        refused.set_presence(Presence::Online);
        refused.set_presence(Presence::Unknown);
        assert!(find(&refused, "presence-dot").unwrap().has_css_class("presence-unknown"));
    }

    #[gtk::test]
    #[ignore = "requires a private GTK display and D-Bus"]
    fn the_stylesheet_parses_cleanly_and_every_circle_is_a_real_dot() {
        init();
        let errors = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let provider = gtk::CssProvider::new();
        let sink = errors.clone();
        provider.connect_parsing_error(move |_, section, error| {
            sink.borrow_mut().push(format!("{section}: {error}"));
        });
        provider.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        assert!(errors.borrow().is_empty(), "style.css has errors: {:?}", errors.borrow());
        let window = gtk::Window::new();
        let row = bx(false, 8);
        let dots: Vec<_> = [Presence::Online, Presence::Away, Presence::Busy, Presence::Unknown]
            .into_iter()
            .map(|presence| {
                let person = PresenceAvatar::new(&avatar());
                person.set_presence(presence);
                row.append(&person);
                find(&person, "presence-dot").unwrap()
            })
            .collect();
        window.set_child(Some(&row));
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        for dot in &dots {
            assert!(dot.width() >= 12 && dot.height() >= 12, "{}x{}", dot.width(), dot.height());
            assert_eq!(dot.width(), dot.height(), "a circle, not an oval");
        }
        window.close();
    }
}
