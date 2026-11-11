use crate::{
    folders::FolderBar,
    model::*,
    protocol::{BackendEvent, Command, Envelope, Session},
    storage::Store,
    widgets::*,
};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
pub struct Ui {
    pub avatars: RefCell<crate::avatar_ui::Avatars>,
    pub images: RefCell<crate::image_ui::Images>,
    pub uploads: RefCell<crate::upload_ui::Uploads>,
    pub downloads: RefCell<crate::downloads::Downloads>,
    pub window: adw::ApplicationWindow,
    pub toast: adw::ToastOverlay,
    pub overlay: gtk::Overlay,
    shell: gtk::Stack,
    welcome_status: gtk::Label,
    welcome_progress: gtk::Spinner,
    welcome_sign_in: gtk::Button,
    welcome_cancel: gtk::Button,
    welcome_register: gtk::LinkButton,
    welcome_account: RefCell<Option<Account>>,
    login_secret: RefCell<Option<zeroize::Zeroizing<String>>>,
    pub split: adw::OverlaySplitView,
    pub utility: adw::OverlaySplitView,
    pub pages: gtk::Stack,
    pub state: RefCell<State>,
    pub demo: RefCell<State>,
    pub store: Store,
    pub folder_bar: FolderBar,
    pub chat_stack: gtk::Stack,
    pub stories: gtk::Box,
    pub sidebar_titles: gtk::Stack,
    pub sidebar_scroll: gtk::ScrolledWindow,
    pub story_reveal: RefCell<Option<adw::TimedAnimation>>,
    pub title: gtk::Button,
    pub titles: gtk::Stack,
    pub section_title: gtk::Label,
    pub search: gtk::SearchEntry,
    pub search_toggle: gtk::ToggleButton,
    pub search_page: gtk::Box,
    pub timeline: gtk::ListBox,
    pub history: gtk::Spinner,
    pub history_latest: gtk::Button,
    pub history_window: crate::history_window::HistoryWindow,
    pub search_revision: Cell<u64>,
    pub scroll: gtk::ScrolledWindow,
    pub message_scroll: RefCell<Option<crate::messages::ScrollTarget>>,
    pub composer: gtk::TextView,
    pub(crate) composer_inline: Rc<crate::inline_emoji_ui::ComposerInlineEmoji>,
    composer_placeholder: gtk::Label,
    pub send_button: gtk::Button,
    pub input_scroll: gtk::ScrolledWindow,
    pub reply_bar: gtk::Revealer,
    pub reply_label: gtk::Label,
    pub replying: RefCell<Option<String>>,
    pub attachments: RefCell<Vec<gio::File>>,
    pub chips: gtk::Box,
    pub selection: gtk::ActionBar,
    pub selection_count: gtk::Label,
    pub selected: RefCell<HashSet<String>>,
    pub anchor: RefCell<Option<String>>,
    pub bubbles: RefCell<HashMap<String, gtk::Widget>>,
    pub(crate) rendered_rows: RefCell<crate::messages::RenderedRows>,
    chat_refresh_pending: Cell<bool>,
    pub media_stack: gtk::Stack,
    pub(crate) media_search: gtk::SearchEntry,
    pub media_pane: adw::ToolbarView,
    pub media_mode: RefCell<String>,
    pub media_toggles: RefCell<HashMap<String, adw::Toggle>>,
    pub request_button: gtk::Button,
    pub request_badge: gtk::Label,
    pub session: RefCell<Option<Session>>,
    pub generation: Cell<u64>,
    pub online: Cell<bool>,
    pub crypto: RefCell<crate::crypto::service::Status>,
    pub legacy_crypto: RefCell<crate::crypto::service::Status>,
    pub pq_crypto: RefCell<crate::crypto::service::Status>,
    pub pq_reviews: RefCell<HashMap<String, Vec<crate::crypto::pq::service::Review>>>,
    pub encryption_checks: RefCell<HashMap<String, String>>,
    pub encryption_revision: Cell<u64>,
    pub room_syncing: RefCell<Option<(u64, String, u64)>>,
    pub room_sync_retry: RefCell<HashMap<String, (Instant, u8)>>,
    pub request_secrets: RefCell<crate::request_logic::SecretJobs>,
    pub security_widgets: RefCell<Option<crate::dialogs::SecurityControls>>,
    pub link_preview_jobs: RefCell<crate::link_preview_logic::Jobs>,
    pub send_queue: RefCell<crate::send_queue::Queue>,
    pub read_ui: RefCell<crate::read_state::ReadState>,
    pub contact_access_widgets: RefCell<Option<crate::contact_access_ui::ContactAccessControls>>,
    pub review_request: RefCell<Option<(String, crate::encryption_policy::Mode)>>,
    pub status: RefCell<String>,
    pub event_tx: mpsc::Sender<Envelope>,
    pub event_rx: RefCell<mpsc::Receiver<Envelope>>,
    pub loading: Cell<bool>,
    pub draft_timer: RefCell<Option<glib::SourceId>>,
    pub big_emoji: Cell<bool>,
    pub typing: RefCell<HashMap<String, Instant>>,
    pub last_typing: Cell<Option<Instant>>,
    pub history_pages: RefCell<HashMap<String, (String, bool)>>,
    pub history_pending: RefCell<HashSet<String>>,
    pub cards: RefCell<Vec<Rc<crate::wallet::WalletCard>>>,
    pub dialogs: RefCell<Vec<adw::Dialog>>,
    pub smoke: bool,
    pub folder_serial: Cell<u32>,
    pub active_media: RefCell<Option<Rc<crate::media::MediaViewer>>>,
    pub active_story: RefCell<Option<Rc<crate::media::StoryViewer>>>,
    pub sidebar_card: RefCell<Option<Rc<crate::wallet::WalletCard>>>,
    pub(crate) pack_library: RefCell<Option<crate::packs::Library>>,
    pub(crate) pack_pages: RefCell<Vec<Rc<crate::media_library_ui::PackLibraryPage>>>,
    pub(crate) gif_library: RefCell<Option<crate::gifs::Library>>,
    pub(crate) gif_page: RefCell<Option<Rc<crate::media_library_ui::GifLibraryPage>>>,
}

impl Ui {
    pub fn new(app: &adw::Application, smoke: bool) -> Rc<Self> {
        Self::new_with_window(app, smoke, Self::new_window(app))
    }
    pub fn new_window(app: &adw::Application) -> adw::ApplicationWindow {
        adw::ApplicationWindow::builder()
            .application(app)
            .title("Monogram")
            .default_width(1380)
            .default_height(900)
            .width_request(430)
            .height_request(480)
            .build()
    }
    pub fn new_with_window(
        _app: &adw::Application,
        smoke: bool,
        window: adw::ApplicationWindow,
    ) -> Rc<Self> {
        let store = Store::open(smoke && !std::env::args().any(|s| s == "--crypto-smoke-test"))
            .expect("private local history database");
        let demo = State::demo();
        let owner = store.pref("active_account", "");
        let accounts = store.accounts();
        let has_owner = accounts.iter().any(|a| a.jid == owner);
        let live_smoke = std::env::args().any(|arg| arg == "--live-smoke-test");
        let welcome = (!smoke || live_smoke) && !has_owner && accounts.is_empty();
        let state = match (has_owner, welcome) {
            (true, _) => store.load_windowed(&owner),
            (_, true) => State::live("", vec![]),
            _ => demo.clone(),
        };
        let toast = adw::ToastOverlay::new();
        let utility = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .min_sidebar_width(360.)
            .max_sidebar_width(400.)
            .sidebar_width_fraction(0.3)
            .show_sidebar(false)
            .pin_sidebar(true)
            .build();
        utility.set_sidebar_width_unit(adw::LengthUnit::Px);
        let overlay = gtk::Overlay::builder().child(&utility).build();
        let shell = gtk::Stack::new();
        shell.set_hhomogeneous(false);
        shell.set_vhomogeneous(false);
        shell.add_named(&overlay, Some("main"));
        toast.set_child(Some(&shell));
        let welcome_view = adw::ToolbarView::new();
        welcome_view.add_top_bar(&adw::HeaderBar::new());
        let welcome_sign_in = gtk::Button::with_label("Sign In");
        welcome_sign_in.add_css_class("suggested-action");
        let welcome_cancel = gtk::Button::with_label("Cancel");
        welcome_cancel.set_visible(false);
        let welcome_register = gtk::LinkButton::with_label(
            "https://jabber.hot-chilli.net/forms/create/",
            "Create an XMPP Account on Hot-Chilli",
        );
        let welcome_status = label("", "dim-label");
        welcome_status.set_wrap(true);
        welcome_status.set_justify(gtk::Justification::Center);
        welcome_status.set_visible(false);
        let welcome_progress = gtk::Spinner::new();
        welcome_progress.set_visible(false);
        let controls = bx(true, 12);
        controls.set_halign(gtk::Align::Center);
        controls.append(&welcome_sign_in);
        controls.append(&welcome_progress);
        controls.append(&welcome_status);
        controls.append(&welcome_cancel);
        controls.append(&welcome_register);
        let landing = adw::StatusPage::builder()
            .icon_name("mail-unread-symbolic")
            .title("Welcome to Monogram")
            .description("Sign in with your XMPP account to start messaging.")
            .child(&controls)
            .build();
        welcome_view.set_content(Some(&scroll(&landing)));
        shell.add_named(&welcome_view, Some("welcome"));
        if welcome {
            shell.set_visible_child_name("welcome");
        }
        let split = adw::OverlaySplitView::builder()
            .min_sidebar_width(290.)
            .max_sidebar_width(360.)
            .sidebar_width_fraction(0.25)
            .build();
        split.set_sidebar_width_unit(adw::LengthUnit::Px);
        let bin = adw::BreakpointBin::builder()
            .child(&split)
            .width_request(375)
            .height_request(360)
            .build();
        utility.set_content(Some(&bin));
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").unwrap());
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        bp.add_setter(&split, "show-sidebar", Some(&false.to_value()));
        bin.add_breakpoint(bp);
        let bp =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1240sp").unwrap());
        bp.add_setter(&utility, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
        let (tx, rx) = mpsc::channel();
        let composer = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .hexpand(true)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(4)
            .right_margin(4)
            .build();
        let composer_inline = crate::inline_emoji_ui::ComposerInlineEmoji::new(&composer.buffer());
        let request_button = icon_button("contact-new-symbolic", "Requests and invitations");
        request_button.set_visible(false);
        let request_badge = label("", "request-badge");
        request_badge.set_visible(false);
        let ui = Rc::new(Self {
            avatars: RefCell::new(Default::default()),
            images: RefCell::new(Default::default()),
            uploads: RefCell::new(Default::default()),
            downloads: RefCell::new(Default::default()),
            window,
            toast,
            overlay,
            shell,
            welcome_status,
            welcome_progress,
            welcome_sign_in,
            welcome_cancel,
            welcome_register,
            welcome_account: RefCell::new(None),
            login_secret: RefCell::new(None),
            split,
            utility,
            pages: gtk::Stack::builder()
                .hhomogeneous(false)
                .vhomogeneous(false)
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(160)
                .build(),
            state: RefCell::new(state),
            demo: RefCell::new(demo),
            store,
            folder_bar: FolderBar::new(),
            chat_stack: gtk::Stack::builder()
                .transition_duration(220)
                .vhomogeneous(false)
                .hhomogeneous(true)
                .build(),
            stories: bx(false, 8),
            sidebar_titles: gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .build(),
            sidebar_scroll: gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .build(),
            story_reveal: RefCell::new(None),
            title: gtk::Button::new(),
            titles: gtk::Stack::new(),
            section_title: label("", "heading"),
            search: gtk::SearchEntry::builder()
                .placeholder_text("Search chats and messages")
                .hexpand(true)
                .build(),
            search_toggle: gtk::ToggleButton::builder()
                .icon_name("system-search-symbolic")
                .tooltip_text("Search chats and messages")
                .build(),
            search_page: bx(true, 18),
            timeline: gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .valign(gtk::Align::Start)
                .vexpand(false)
                .build(),
            history: gtk::Spinner::builder()
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Start)
                .margin_top(8)
                .can_target(false)
                .visible(false)
                .build(),
            history_latest: gtk::Button::builder()
                .icon_name("go-bottom-symbolic")
                .tooltip_text("Jump to latest message")
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .margin_bottom(12)
                .margin_end(16)
                .css_classes(["circular", "osd"])
                .visible(false)
                .build(),
            history_window: Default::default(),
            search_revision: Cell::new(0),
            message_scroll: RefCell::new(None),
            scroll: gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
            composer,
            composer_inline,
            composer_placeholder: gtk::Label::builder()
                .label("Message…")
                .xalign(0.)
                .halign(gtk::Align::Fill)
                .valign(gtk::Align::Center)
                .margin_start(4)
                .can_target(false)
                .css_classes(["dim-label"])
                .build(),
            send_button: icon_button("mail-send-symbolic", "Send"),
            input_scroll: gtk::ScrolledWindow::builder()
                .min_content_height(32)
                .max_content_height(108)
                .propagate_natural_height(false)
                .hexpand(true)
                .build(),
            reply_bar: gtk::Revealer::new(),
            reply_label: text("", ""),
            replying: RefCell::new(None),
            attachments: RefCell::new(vec![]),
            chips: bx(false, 6),
            selection: gtk::ActionBar::builder().revealed(false).build(),
            selection_count: label("", "heading"),
            selected: RefCell::new(HashSet::new()),
            anchor: RefCell::new(None),
            bubbles: RefCell::new(HashMap::new()),
            rendered_rows: RefCell::new(Default::default()),
            chat_refresh_pending: Cell::new(false),
            media_stack: gtk::Stack::builder()
                .hhomogeneous(false)
                .vhomogeneous(false)
                .vexpand(true)
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .build(),
            media_search: gtk::SearchEntry::builder()
                .placeholder_text("Search")
                .build(),
            media_pane: adw::ToolbarView::new(),
            media_mode: RefCell::new("media".into()),
            media_toggles: RefCell::new(HashMap::new()),
            request_button,
            request_badge,
            session: RefCell::new(None),
            generation: Cell::new(0),
            online: Cell::new(false),
            crypto: RefCell::new(Default::default()),
            legacy_crypto: RefCell::new(Default::default()),
            pq_crypto: RefCell::new(Default::default()),
            pq_reviews: RefCell::new(HashMap::new()),
            encryption_checks: RefCell::new(HashMap::new()),
            encryption_revision: Cell::new(0),
            room_syncing: RefCell::new(None),
            room_sync_retry: RefCell::new(HashMap::new()),
            request_secrets: RefCell::new(Default::default()),
            security_widgets: RefCell::new(None),
            contact_access_widgets: RefCell::new(None),
            link_preview_jobs: RefCell::new(Default::default()),
            send_queue: RefCell::new(Default::default()),
            read_ui: RefCell::new(Default::default()),
            review_request: RefCell::new(None),
            status: RefCell::new("Not connected".into()),
            event_tx: tx,
            event_rx: RefCell::new(rx),
            loading: Cell::new(false),
            draft_timer: RefCell::new(None),
            big_emoji: Cell::new(true),
            typing: RefCell::new(HashMap::new()),
            last_typing: Cell::new(None),
            history_pages: RefCell::new(HashMap::new()),
            history_pending: RefCell::new(HashSet::new()),
            cards: RefCell::new(vec![]),
            dialogs: RefCell::new(vec![]),
            smoke,
            folder_serial: Cell::new(0),
            active_media: RefCell::new(None),
            active_story: RefCell::new(None),
            sidebar_card: RefCell::new(None),
            pack_library: RefCell::new(None),
            pack_pages: RefCell::new(Vec::new()),
            gif_library: RefCell::new(None),
            gif_page: RefCell::new(None),
        });
        ui.make_sidebar();
        ui.weak_button(&ui.welcome_sign_in, |ui| {
            let account = ui.welcome_account.borrow().clone();
            ui.login(account);
        });
        ui.weak_button(&ui.welcome_cancel, |ui| {
            ui.session.borrow_mut().take();
            ui.login_secret.borrow_mut().take();
            ui.generation.set(ui.generation.get() + 1);
            ui.show_welcome_status("", false, false);
        });
        ui.make_content();
        ui.make_media();
        ui.actions();
        ui.rebuild_folders();
        ui.refresh_chats(None);
        ui.refresh_stories();
        ui.load_current();
        ui.update_mode();
        let weak = Rc::downgrade(&ui);
        ui.window.connect_is_active_notify(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.mark_visible_chat_read();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.pages.connect_visible_child_name_notify(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.mark_visible_chat_read();
            }
        });
        ui.big_emoji
            .set(ui.store.pref("big_emoji", "true") == "true");
        adw::StyleManager::default().set_color_scheme(
            match ui.store.pref("style", "system").as_str() {
                "dark" => adw::ColorScheme::ForceDark,
                "light" => adw::ColorScheme::ForceLight,
                _ => adw::ColorScheme::Default,
            },
        );
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_millis(220), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            ui.poll_avatars();
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_millis(30), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let events: Vec<_> = ui.event_rx.borrow().try_iter().collect();
            for (g, a, e) in events {
                ui.backend_event(g, &a, e)
            }
            ui.poll_uploads();
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(&ui);
        let typing_ui = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(ui) = typing_ui.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let before = ui.typing.borrow().len();
            ui.sync_room_bookmarks();
            ui.retry_request_cleanup();
            ui.typing
                .borrow_mut()
                .retain(|_, t| t.elapsed() < Duration::from_secs(10));
            if before != ui.typing.borrow().len()
                && ui.pages.visible_child_name().as_deref() == Some("chat")
            {
                ui.show_chat();
            }
            glib::ControlFlow::Continue
        });
        let keys = gtk::EventControllerKey::new();
        let weak_keys = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(ui) = weak_keys.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if ui.selected.borrow().is_empty() {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::Escape => ui.clear_selection(),
                gdk::Key::Delete => ui.selection_action("delete"),
                gdk::Key::a if modifiers.contains(gdk::ModifierType::CONTROL_MASK) => {
                    ui.selection_action("select-all")
                }
                gdk::Key::c if modifiers.contains(gdk::ModifierType::CONTROL_MASK) => {
                    ui.selection_action("copy")
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        ui.window.add_controller(keys);
        let typing_keys = gtk::EventControllerKey::new();
        typing_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_keys = Rc::downgrade(&ui);
        typing_keys.connect_key_pressed(move |controller, key, _, modifiers| {
            let Some(ui) = weak_keys.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let blocked_modifiers = gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK;
            let printable = key
                .to_unicode()
                .is_some_and(|character| !character.is_control());
            let focus_is_editable =
                gtk::prelude::GtkWindowExt::focus(&ui.window).is_some_and(|focus| {
                    let mut current = Some(focus);
                    while let Some(widget) = current {
                        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
                            return true;
                        }
                        current = widget.parent();
                    }
                    false
                });
            if !printable
                || modifiers.intersects(blocked_modifiers)
                || focus_is_editable
                || ui.pages.visible_child_name().as_deref() != Some("chat")
                || ui.active_media.borrow().is_some()
            {
                return glib::Propagation::Proceed;
            }
            ui.composer.grab_focus();
            if controller.forward(&ui.composer) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        ui.window.add_controller(typing_keys);
        ui.window.connect_close_request(move |_| {
            if let Some(u) = weak.upgrade() {
                u.save_draft();
                u.stop_uploads();
                u.images.borrow_mut().clear();
                u.avatars.borrow_mut().clear();
                let media = u.active_media.borrow().clone();
                if let Some(media) = media {
                    media.close_immediate();
                }
                u.downloads.borrow_mut().clear();
                u.session.borrow_mut().take();
                u.generation.set(u.generation.get() + 1);
            }
            glib::Propagation::Proceed
        });
        // Keep the already-painted startup page visible until the complete interface is ready.
        ui.window.set_content(Some(&ui.toast));
        ui
    }
    pub fn weak_button(self: &Rc<Self>, b: &gtk::Button, f: impl Fn(&Rc<Self>) + 'static) {
        let weak = Rc::downgrade(self);
        b.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                f(&ui)
            }
        });
    }
    pub fn notice(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }
    fn queue_chat_refresh(self: &Rc<Self>) {
        if self.chat_refresh_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(ui) = weak.upgrade() {
                ui.chat_refresh_pending.set(false);
                ui.refresh_chats(None);
            }
        });
    }
    pub fn persist(&self) -> bool {
        if let Err(_) = self.store.save(&self.state.borrow()) {
            self.notice("Local history could not be saved. Check free space and permissions.");
            false
        } else {
            true
        }
    }
    pub fn action(self: &Rc<Self>, name: &str, f: impl Fn(&Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.on_welcome()) {
                f(&ui)
            }
        });
        self.window.add_action(&action);
    }
    fn actions(self: &Rc<Self>) {
        self.action("search", |s| {
            s.search_toggle.set_active(!s.search_toggle.is_active())
        });
        self.action("media", |s| s.toggle_media());
        self.action("shared-media", |s| s.shared_media());
        self.action("new-chat", |s| s.new_chat());
        self.action("accounts", |s| s.accounts_dialog());
        self.action("preferences", |s| s.preferences());
        self.action("shortcuts", |s| s.shortcuts());
        self.action("security", |s| s.security());
        self.action("info", |s| s.show_info());
        self.action("requests", |s| s.requests());
        self.action("sync-history", |s| {
            let peer = s.state.borrow().current().jid.clone();
            if !s.history_pending.borrow().contains(&peer) {
                s.history_pages.borrow_mut().remove(&peer);
                s.request_server_history(&peer);
            }
        });
        self.action("contacts", |s| s.contacts());
        self.action("archives", |s| s.archives());
        self.action("saved", |s| s.select_chat("saved"));
        self.action("wallet", |s| s.wallet());
        self.action("stories", |s| s.show_stories());
        self.action("spaces", |s| {
            s.preview_form("Spaces and Channels", &["Service", "Channel name"])
        });
        self.action("profile", |s| {
            s.preview_form("Your Profile", &["Display name", "About", "Website"])
        });
        self.action("developer", |s| {
            s.preview_form("Developer Tools", &["Service address", "Command"])
        });
        self.action("call", |s| s.call_preview());
        self.action("attach-file", |s| s.choose_files(false));
        self.action("attach-media", |s| s.choose_files(true));
        self.action("poll", |s| s.new_poll());
        self.action("article", |s| s.article());
        self.action("location", |s| {
            s.send_content("Riverside Park", "location");
        });
        self.action("payment", |s| s.payment(true));
        self.action("archive", |s| s.organize("archive"));
        let mute = gio::SimpleAction::new_stateful(
            "notifications",
            Some(glib::VariantTy::STRING),
            &"all".to_variant(),
        );
        let weak = Rc::downgrade(self);
        mute.connect_activate(move |a, value| {
            if let (Some(ui), Some(v)) = (weak.upgrade(), value) {
                if let Some(mode) = v.str() {
                    ui.state.borrow_mut().current_mut().notifications = mode.into();
                    a.set_state(v);
                    ui.persist();
                }
            }
        });
        self.window.add_action(&mute);
        if let Some(app) = self.window.application() {
            for (name, keys) in [
                ("search", vec!["<Control>f"]),
                ("media", vec!["F9"]),
                ("new-chat", vec!["<Control>n"]),
                ("attach-file", vec!["<Control>o"]),
                ("preferences", vec!["<Control>comma"]),
                ("shortcuts", vec!["<Control>question"]),
            ] {
                app.set_accels_for_action(&format!("win.{name}"), &keys)
            }
        }
    }
    fn make_sidebar(self: &Rc<Self>) {
        let view = adw::ToolbarView::new();
        view.add_css_class("chat-sidebar");
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .title_widget(&self.sidebar_titles)
            .build();
        header.pack_start(&self.search_toggle);
        let menu = gio::Menu::new();
        for (title, action) in [
            ("New Conversation…", "new-chat"),
            ("Contacts", "contacts"),
            ("Saved Messages", "saved"),
            ("Archived Chats", "archives"),
            ("Wallet", "wallet"),
            ("Stories", "stories"),
            ("Spaces and Channels", "spaces"),
            ("Your Profile", "profile"),
        ] {
            menu.append(Some(title), Some(&format!("win.{action}")))
        }
        let section = gio::Menu::new();
        for (title, action) in [
            ("Accounts", "accounts"),
            ("Preferences", "preferences"),
            ("Keyboard Shortcuts", "shortcuts"),
            ("Developer Tools", "developer"),
        ] {
            section.append(Some(title), Some(&format!("win.{action}")))
        }
        menu.append_section(None, &section);
        let primary = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .build();
        header.pack_end(&primary);
        let icon = gtk::Overlay::builder()
            .child(&gtk::Image::from_icon_name("contact-new-symbolic"))
            .build();
        self.request_badge.set_halign(gtk::Align::End);
        self.request_badge.set_valign(gtk::Align::Start);
        icon.add_overlay(&self.request_badge);
        self.request_button.set_child(Some(&icon));
        self.request_badge
            .bind_property("visible", &self.request_button, "visible")
            .sync_create()
            .build();
        self.weak_button(&self.request_button, |s| s.requests());
        header.pack_end(&self.request_button);
        view.add_top_bar(&header);
        let content = bx(true, 6);
        let folders = horizontal_strip(&self.folder_bar);
        content.append(&folders);
        let body = bx(true, 0);
        margins(&self.stories, 12);
        self.stories.set_margin_top(8);
        self.stories.set_margin_bottom(8);
        body.append(&horizontal_strip(&self.stories));
        body.append(&self.chat_stack);
        self.sidebar_scroll.set_child(Some(&body));
        content.append(&self.sidebar_scroll);
        self.sidebar_titles
            .add_named(&label("Monogram", "heading"), Some("title"));
        let mini = gtk::Button::builder().tooltip_text("Show stories").build();
        mini.add_css_class("flat");
        let people = bx(false, 2);
        for name in ["Mira", "Sam", "Alex"] {
            people.append(&avatar(
                &Chat {
                    name: name.into(),
                    ..Default::default()
                },
                22,
            ));
        }
        mini.set_child(Some(&people));
        self.weak_button(&mini, |ui| ui.show_stories());
        self.sidebar_titles.add_named(&mini, Some("stories"));
        let weak = Rc::downgrade(self);
        self.sidebar_scroll
            .vadjustment()
            .connect_value_changed(move |a| {
                if let Some(ui) = weak.upgrade() {
                    ui.sidebar_titles.set_visible_child_name(
                        if ui.stories.is_visible() && a.value() >= 80. {
                            "stories"
                        } else {
                            "title"
                        },
                    );
                }
            });
        view.set_content(Some(&content));
        self.split.set_sidebar(Some(&view));
        let weak = Rc::downgrade(self);
        self.search_toggle.connect_toggled(move |b| {
            if let Some(ui) = weak.upgrade() {
                ui.search_mode(b.is_active())
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_results();
                ui.refresh_chats(None);
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_stop_search(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_toggle.set_active(false)
            }
        });
        self.chat_stack.connect_transition_running_notify(|stack| {
            if !stack.is_transition_running() {
                let current = stack.visible_child();
                let pages = stack.pages();
                let children: Vec<gtk::Widget> = (0..pages.n_items())
                    .filter_map(|i| {
                        pages
                            .item(i)
                            .and_downcast::<gtk::StackPage>()
                            .map(|p| p.child())
                    })
                    .collect();
                for c in children {
                    if Some(&c) != current.as_ref() {
                        stack.remove(&c)
                    }
                }
            }
        });
    }
    pub fn rebuild_folders(self: &Rc<Self>) {
        while let Some(c) = self.folder_bar.first_child() {
            self.folder_bar.remove(&c)
        }
        let state = self.state.borrow();
        let mut first: Option<gtk::ToggleButton> = None;
        for name in &state.folder_order {
            let b = gtk::ToggleButton::with_label(name);
            b.add_css_class("flat");
            if let Some(ref first) = first {
                b.set_group(Some(first))
            } else {
                first = Some(b.clone())
            }
            b.set_active(name == &state.folder);
            let weak = Rc::downgrade(self);
            let name = name.clone();
            let n = name.clone();
            b.connect_toggled(move |b| {
                if b.is_active() {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_folder(&n)
                    }
                }
            });
            let weak = Rc::downgrade(self);
            self.folder_bar.bind(&b, move |name, index| {
                if let Some(ui) = weak.upgrade() {
                    ui.state.borrow_mut().reorder(name, index);
                    ui.order_folders();
                    ui.persist();
                }
            });
            let mut actions = vec![
                ("Edit folder…".into(), format!("folder-edit:{name}")),
                ("Move Left".into(), format!("folder-left:{name}")),
                ("Move Right".into(), format!("folder-right:{name}")),
                ("New folder…".into(), "folder-new".into()),
            ];
            if state.folders.contains_key(&name) {
                actions.push(("Delete folder…".into(), format!("folder-delete:{name}")));
            }
            self.context_actions(&b, actions);
            self.folder_bar.append(&b);
        }
    }
    fn order_folders(&self) {
        let mut buttons = HashMap::new();
        let mut child = self.folder_bar.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if let Ok(b) = c.downcast::<gtk::ToggleButton>() {
                buttons.insert(b.label().unwrap_or_default().to_string(), b);
            }
        }
        let mut previous: Option<gtk::ToggleButton> = None;
        for name in &self.state.borrow().folder_order {
            if let Some(b) = buttons.get(name) {
                self.folder_bar.reorder_child_after(b, previous.as_ref());
                previous = Some(b.clone());
            }
        }
    }
    pub fn set_folder(self: &Rc<Self>, name: &str) {
        let (old, new) = {
            let mut s = self.state.borrow_mut();
            let old = s
                .folder_order
                .iter()
                .position(|n| n == &s.folder)
                .unwrap_or(0);
            let new = s.folder_order.iter().position(|n| n == name).unwrap_or(0);
            if s.folder == name {
                return;
            }
            s.folder = name.into();
            (old, new)
        };
        self.refresh_chats(Some(new >= old));
        self.persist();
    }
    pub fn refresh_chats(self: &Rc<Self>, direction: Option<bool>) {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("chat-list");
        list.set_valign(gtk::Align::Start);
        list.set_vexpand(false);
        let pending = self.request_count();
        if pending > 0 {
            self.request_badge.set_label(&pending.to_string());
        } else {
            self.request_badge.set_label("");
        }
        self.request_badge.set_visible(pending > 0);
        self.request_button.set_visible(pending > 0);
        let state = self.state.borrow();
        for chat in state.filtered(self.search.text().as_str()) {
            let latest = chat.latest_preview();
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&chat.id);
            let line = bx(false, 11);
            line.append(&self.chat_avatar(&chat, 44));
            let words = bx(true, 5);
            words.set_hexpand(true);
            let top = bx(false, 8);
            let name = text(&chat.name, "chat-name");
            name.set_hexpand(true);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            top.append(&name);
            top.append(&label(
                latest.as_ref().map(|m| m.time.as_str()).unwrap_or("Now"),
                "chat-time",
            ));
            words.append(&top);
            let bottom = bx(false, 6);
            let preview = if !chat.draft.is_empty() {
                format!("Draft: {}", chat.draft.replace('\n', " "))
            } else {
                latest
                    .as_ref()
                    .map(|m| {
                        format!(
                            "{}{}",
                            if m.own { "You: " } else { "" },
                            m.text.replace('\n', " ")
                        )
                    })
                    .unwrap_or_else(|| "No messages yet".into())
            };
            let l = text(&preview, "chat-preview");
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_hexpand(true);
            bottom.append(&l);
            if chat.unread > 0 || chat.unread_mark {
                bottom.append(&label(
                    &if chat.unread > 0 {
                        chat.unread.to_string()
                    } else {
                        "•".into()
                    },
                    "unread-badge",
                ))
            } else if chat.pinned {
                bottom.append(&gtk::Image::from_icon_name("view-pin-symbolic"))
            }
            words.append(&bottom);
            line.append(&words);
            row.set_child(Some(&line));
            let actions = vec![
                ("Open".into(), format!("chat-open:{}", chat.id)),
                (
                    (if chat.pinned { "Unpin" } else { "Pin" }).into(),
                    format!("chat-pin:{}", chat.id),
                ),
                (
                    (if chat.unread > 0 || chat.unread_mark {
                        "Mark read"
                    } else {
                        "Mark unread"
                    })
                    .into(),
                    format!("chat-unread:{}", chat.id),
                ),
                (
                    (if chat.archived {
                        "Unarchive"
                    } else {
                        "Archive"
                    })
                    .into(),
                    format!("chat-archive:{}", chat.id),
                ),
                (
                    (if chat.notifications == "all" {
                        "Mute"
                    } else {
                        "Unmute"
                    })
                    .into(),
                    format!("chat-mute:{}", chat.id),
                ),
            ];
            self.context_actions(&row, actions);
            list.append(&row);
            if chat.id == state.active {
                list.select_row(Some(&row))
            }
        }
        drop(state);
        let weak = Rc::downgrade(self);
        list.connect_row_activated(move |_, row| {
            if let Some(ui) = weak.upgrade() {
                ui.select_chat(row.widget_name().as_str())
            }
        });
        let serial = self.folder_serial.get() + 1;
        self.folder_serial.set(serial);
        self.chat_stack.set_transition_type(match direction {
            Some(true) => gtk::StackTransitionType::SlideLeft,
            Some(false) => gtk::StackTransitionType::SlideRight,
            None => gtk::StackTransitionType::None,
        });
        self.chat_stack
            .add_named(&list, Some(&format!("list-{serial}")));
        self.chat_stack.set_visible_child(&list);
        if !self.chat_stack.is_transition_running() {
            let pages = self.chat_stack.pages();
            for i in (0..pages.n_items()).rev() {
                if let Some(page) = pages.item(i).and_downcast::<gtk::StackPage>() {
                    if page.child() != list {
                        self.chat_stack.remove(&page.child());
                    }
                }
            }
        }
    }
    fn make_content(self: &Rc<Self>) {
        let view = adw::ToolbarView::new();
        view.set_top_bar_style(adw::ToolbarStyle::Flat);
        view.add_css_class("conversation-view");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .build();
        let sidebar = icon_button("sidebar-show-symbolic", "Show conversations");
        sidebar.set_visible(false);
        let split = self.split.downgrade();
        sidebar.connect_clicked(move |_| {
            if let Some(split) = split.upgrade() {
                split.set_show_sidebar(!split.shows_sidebar())
            }
        });
        header.pack_start(&sidebar);
        let weak = sidebar.downgrade();
        self.split.connect_collapsed_notify(move |s| {
            if let Some(b) = weak.upgrade() {
                b.set_visible(s.is_collapsed())
            }
        });
        self.title.add_css_class("flat");
        self.title.add_css_class("conversation-title");
        self.weak_button(&self.title, |s| s.show_info());
        header.pack_start(&self.title);
        self.titles.add_named(&self.section_title, Some("normal"));
        self.titles.add_named(&self.search, Some("search"));
        self.titles.set_hexpand(true);
        header.set_title_widget(Some(&self.titles));
        let menu = gio::Menu::new();
        for (t, a) in [
            ("Conversation Security", "security"),
            ("Conversation Info", "info"),
        ] {
            menu.append(Some(t), Some(&format!("win.{a}")))
        }
        let mute = gio::Menu::new();
        for (t, v) in [
            ("All Messages", "all"),
            ("Mentions Only", "mentions"),
            ("For 1 Hour", "1h"),
            ("For 8 Hours", "8h"),
            ("For 2 Days", "2d"),
            ("Until I Turn It On", "forever"),
        ] {
            let item = gio::MenuItem::new(Some(t), None);
            item.set_action_and_target_value(Some("win.notifications"), Some(&v.to_variant()));
            mute.append_item(&item)
        }
        menu.append_submenu(Some("Mute"), &mute);
        for (t, a) in [
            ("Shared Media", "shared-media"),
            ("Call", "call"),
            ("Archive Conversation", "archive"),
        ] {
            menu.append(Some(t), Some(&format!("win.{a}")))
        }
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .build(),
        );
        view.add_top_bar(&header);
        let chat = bx(true, 0);
        chat.add_css_class("conversation");
        self.timeline.add_css_class("messages");
        self.timeline.add_css_class("message-list");
        let timeline_clamp = adw::Clamp::builder()
            .child(&self.timeline)
            .maximum_size(1080)
            .tightening_threshold(840)
            .build();
        timeline_clamp.add_css_class("timeline-clamp");
        self.scroll.set_child(Some(&timeline_clamp));
        let history_view = gtk::Overlay::builder()
            .child(&self.scroll)
            .vexpand(true)
            .build();
        history_view.add_overlay(&self.history);
        history_view.add_overlay(&self.history_latest);
        self.weak_button(&self.history_latest, |ui| ui.load_timeline(None));
        let scroll_bounds = Rc::new(Cell::new(self.history_bounds()));
        let previous_bounds = scroll_bounds.clone();
        let weak = Rc::downgrade(self);
        self.scroll.vadjustment().connect_value_changed(move |adjustment| {
            if let Some(ui) = weak.upgrade() {
                let (previous_value, page, upper) = previous_bounds.get();
                if adjustment.value() < previous_value
                    && adjustment.page_size() == page
                    && adjustment.upper() == upper
                    && !ui.history_window.moving.get()
                    && matches!(ui.message_scroll.borrow().as_ref(), Some(crate::messages::ScrollTarget::Bottom))
                {
                    ui.message_scroll.borrow_mut().take();
                }
                previous_bounds.set((adjustment.value(), page, upper));
                ui.schedule_history_maintenance();
            }
        });
        let weak = Rc::downgrade(self);
        self.scroll.vadjustment().connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                let (value, page, upper) = scroll_bounds.replace(ui.history_bounds());
                // Media can grow after its row is inserted; preserve the previous bottom position.
                if upper - page - value < 24.
                    && ui.history_window.newest()
                    && ui.active_media.borrow().is_none()
                    && !matches!(ui.message_scroll.borrow().as_ref(), Some(crate::messages::ScrollTarget::Message(_)))
                {
                    ui.queue_message_scroll(crate::messages::ScrollTarget::Bottom);
                }
                ui.schedule_history_maintenance();
            }
        });
        chat.append(&history_view);
        let history_scroll =
            gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        history_scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        history_scroll.connect_scroll(move |_, _, dy| {
            if dy != 0. {
                if let Some(ui) = weak.upgrade() {
                    ui.history_edge(if dy < 0. {
                        crate::history_window::Side::Older
                    } else {
                        crate::history_window::Side::Newer
                    });
                }
            }
            glib::Propagation::Proceed
        });
        self.scroll.add_controller(history_scroll);
        self.selection.pack_start(&self.selection_count);
        let clear = icon_button("window-close-symbolic", "Clear selection");
        self.weak_button(&clear, |s| s.clear_selection());
        self.selection.pack_start(&clear);
        for (icon, title, action) in [
            ("edit-select-all-symbolic", "Select all", "select-all"),
            ("edit-copy-symbolic", "Copy selected", "copy"),
            ("mail-forward-symbolic", "Forward selected", "forward"),
            ("user-trash-symbolic", "Delete selected", "delete"),
        ] {
            let b = icon_button(icon, title);
            self.weak_button(&b, move |s| s.selection_action(action));
            self.selection.pack_end(&b)
        }
        chat.append(&self.selection);
        let shell = bx(true, 0);
        shell.add_css_class("composer-shell");
        let context = bx(false, 8);
        self.reply_label.set_hexpand(true);
        context.append(&self.reply_label);
        let cancel = icon_button("window-close-symbolic", "Cancel reply");
        self.weak_button(&cancel, |s| s.cancel_reply());
        context.append(&cancel);
        self.reply_bar.set_child(Some(&context));
        shell.append(&self.reply_bar);
        self.chips.set_visible(false);
        let attachments = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&self.chips)
            .build();
        attachments.add_css_class("attachment-preview-strip");
        self.chips
            .bind_property("visible", &attachments, "visible")
            .sync_create()
            .build();
        shell.append(&attachments);
        let line = bx(false, 8);
        let composer = bx(false, 2);
        composer.set_hexpand(true);
        composer.set_valign(gtk::Align::Center);
        composer.add_css_class("composer");
        let attach = gtk::MenuButton::builder()
            .icon_name("mail-attachment-symbolic")
            .valign(gtk::Align::Center)
            .build();
        attach.add_css_class("flat");
        let menu = gio::Menu::new();
        for (t, a) in [
            ("Photo or Video…", "attach-media"),
            ("File…", "attach-file"),
            ("Poll…", "poll"),
            ("Payment…", "payment"),
            ("Location…", "location"),
            ("Article…", "article"),
        ] {
            menu.append(Some(t), Some(&format!("win.{a}")))
        }
        attach.set_menu_model(Some(&menu));
        composer.append(&attach);
        self.input_scroll.set_child(Some(&self.composer));
        self.input_scroll
            .set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let weak = Rc::downgrade(self);
        self.input_scroll
            .vadjustment()
            .connect_changed(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.queue_composer_height();
                }
            });
        let input = gtk::Overlay::builder()
            .child(&self.input_scroll)
            .hexpand(true)
            .build();
        input.add_overlay(&self.composer_placeholder);
        composer.append(&input);
        let emoji = icon_button("face-smile-symbolic", "Emoji, stickers and GIFs · F9");
        self.weak_button(&emoji, |s| s.toggle_media());
        composer.append(&emoji);
        line.append(&composer);
        self.send_button.remove_css_class("flat");
        self.send_button.add_css_class("suggested-action");
        self.send_button.add_css_class("send");
        let plane = gtk::Image::from_file(asset("paper-plane-symbolic.svg"));
        plane.set_pixel_size(20);
        self.send_button.set_child(Some(&plane));
        self.weak_button(&self.send_button, |s| s.send());
        line.append(&self.send_button);
        shell.append(&line);
        let conversation = adw::ToolbarView::new();
        conversation.set_bottom_bar_style(adw::ToolbarStyle::Flat);
        conversation.set_content(Some(&chat));
        conversation.add_bottom_bar(&shell);
        self.pages.add_named(&conversation, Some("chat"));
        self.pages
            .add_named(&scroll(&self.search_page), Some("search"));
        view.set_content(Some(&self.pages));
        self.split.set_content(Some(&view));
        let weak = Rc::downgrade(self);
        self.composer.buffer().connect_changed(move |_| {
            if let Some(s) = weak.upgrade() {
                s.compose_changed()
            }
        });
        let emoji_completion = crate::emoji_completion::attach(&self.composer);
        crate::clipboard_upload::attach(self);
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, key, _, state| {
            if emoji_completion.key(key, state) {
                return glib::Propagation::Stop;
            }
            let Some(s) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && !state.contains(gdk::ModifierType::SHIFT_MASK)
            {
                s.send();
                glib::Propagation::Stop
            } else if key == gdk::Key::Escape {
                s.cancel_reply();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.composer.add_controller(key);
    }
    pub fn composer_text(&self) -> String {
        let b = self.composer.buffer();
        b.text(&b.start_iter(), &b.end_iter(), true).into()
    }
    fn queue_composer_height(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(s) = weak.upgrade() {
                s.sync_composer_height();
            }
        });
    }
    fn sync_composer_height(&self) {
        let end = self.composer.buffer().end_iter();
        let last_line = self.composer.iter_location(&end);
        let content_height = last_line
            .y()
            .saturating_add(last_line.height())
            .saturating_add(self.composer.top_margin())
            .saturating_add(self.composer.bottom_margin());
        let viewport_height = content_height.clamp(32, 108);
        if self.input_scroll.min_content_height() != viewport_height {
            self.input_scroll
                .set_min_content_height(viewport_height);
        }
        let policy = if content_height > 108 {
            gtk::PolicyType::Automatic
        } else {
            gtk::PolicyType::Never
        };
        if self.input_scroll.vscrollbar_policy() != policy {
            self.input_scroll.set_vscrollbar_policy(policy);
        }
    }
    pub fn compose_changed(self: &Rc<Self>) {
        let composer_text = self.composer_text();
        self.composer_placeholder
            .set_visible(composer_text.is_empty());
        self.queue_composer_height();
        self.send_button.set_sensitive(
            !composer_text.trim().is_empty() || !self.attachments.borrow().is_empty(),
        );
        if self.loading.get() {
            return;
        }
        if let Some(id) = self.draft_timer.borrow_mut().take() {
            id.remove()
        }
        let weak = Rc::downgrade(self);
        self.draft_timer.replace(Some(glib::timeout_add_local_once(
            Duration::from_millis(750),
            move || {
                if let Some(s) = weak.upgrade() {
                    s.draft_timer.borrow_mut().take();
                    s.save_draft();
                    s.refresh_chats(None)
                }
            },
        )));
        if self.online.get()
            && self.preference("typing", true)
            && self
                .last_typing
                .get()
                .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
        {
            self.last_typing.set(Some(Instant::now()));
            let peer = self.state.borrow().current().jid.clone();
            self.command(Command::Typing {
                peer,
                composing: !composer_text.is_empty(),
            });
        }
    }
    pub fn save_draft(&self) {
        if self.loading.get() || self.on_welcome() {
            return;
        }
        if let Some(id) = self.draft_timer.borrow_mut().take() {
            id.remove()
        }
        let text = self.composer_text();
        let mut state = self.state.borrow_mut();
        let chat = state.current_mut();
        chat.draft = text;
        chat.draft_reply = self.replying.borrow().clone().unwrap_or_default();
        drop(state);
        self.persist();
    }
    pub fn select_chat(self: &Rc<Self>, id: &str) {
        if !self.state.borrow().chats.iter().any(|c| c.id == id) {
            return;
        }
        self.save_draft();
        let switched = self.state.borrow().active != id;
        self.timeline_scope_changed();
        {
            let mut state = self.state.borrow_mut();
            if switched && !state.account.is_empty() {
                let previous = state.current_mut();
                previous.remember_latest();
                previous.messages.clear();
            }
            state.active = id.into();
            let c = state.current_mut();
            c.unread = 0;
            c.unread_mark = false;
            for m in &mut c.messages {
                m.unread = false;
            }
            let _ = self.store.mark_read(&state.account, &state.current().jid);
        }
        self.clear_selection();
        self.attachments.borrow_mut().clear();
        clear(&self.chips);
        self.chips.set_visible(false);
        self.load_current();
        if !self.state.borrow().account.is_empty() {
            self.load_timeline(None);
        }
        self.refresh_chats(None);
        self.persist();
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false)
        }
        if self.online.get() {
            let state = self.state.borrow();
            let chat = state.current();
            if chat.group && !chat.room_joined {
                self.command(Command::JoinRoom {
                    peer: chat.jid.clone(),
                    nick: None,
                });
            } else {
                drop(state);
                self.load_history(true)
            }
        }
    }
    pub fn load_current(self: &Rc<Self>) {
        let chat = self.state.borrow().current().clone();
        self.loading.set(true);
        self.composer.buffer().set_text(&chat.draft);
        self.replying
            .replace((!chat.draft_reply.is_empty()).then_some(chat.draft_reply));
        self.loading.set(false);
        self.update_reply();
        self.update_encryption_check();
        self.render_messages(true);
        self.show_chat();
    }
    pub fn show_chat(&self) {
        self.search_toggle.set_active(false);
        self.pages.set_visible_child_name("chat");
        self.title.set_visible(true);
        self.section_title.set_text("");
        let state = self.state.borrow();
        let chat = state.current();
        self.composer_placeholder
            .set_text(&format!("Message {}…", chat.name));
        self.composer_placeholder
            .set_visible(self.composer_text().is_empty());
        let title = bx(false, 8);
        title.append(&self.chat_avatar(chat, 28));
        let words = bx(true, 0);
        words.append(&text(&chat.name, "chat-title"));
        let subtitle = if self
            .typing
            .borrow()
            .get(&chat.jid)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        {
            "Typing…"
        } else {
            &chat.subtitle
        };
        words.append(&text(subtitle, "chat-subtitle dim-label"));
        title.append(&words);
        self.title.set_child(Some(&title));
        if let Some(a) = self
            .window
            .lookup_action("notifications")
            .and_downcast::<gio::SimpleAction>()
        {
            a.set_state(&chat.notifications.to_variant())
        }
    }
    pub fn show_page(&self, title: &str, widget: &impl IsA<gtk::Widget>) {
        self.search_toggle.set_active(false);
        if let Some(old) = self.pages.child_by_name("section") {
            self.pages.remove(&old)
        }
        self.pages.add_named(widget, Some("section"));
        self.pages.set_visible_child_name("section");
        self.title.set_visible(false);
        self.section_title.set_text(title);
    }
    fn search_mode(self: &Rc<Self>, active: bool) {
        self.stories
            .set_visible(!active && self.state.borrow().account.is_empty());
        if active {
            self.title.set_visible(false);
            self.titles.set_visible_child_name("search");
            self.pages.set_visible_child_name("search");
            self.search_results();
            self.search.grab_focus();
        } else {
            self.titles.set_visible_child_name("normal");
            self.pages.set_visible_child_name("chat");
            self.title.set_visible(true);
            self.search.set_text("");
        }
    }
    pub fn search_results(self: &Rc<Self>) {
        clear(&self.search_page);
        margins(&self.search_page, 24);
        let revision = self.search_revision.get().wrapping_add(1);
        self.search_revision.set(revision);
        let query = self.search.text().to_lowercase();
        if query.trim().is_empty() {
            self.search_page.append(
                &adw::StatusPage::builder()
                    .title("Search Chats and Messages")
                    .icon_name("system-search-symbolic")
                    .build(),
            );
            return;
        }
        let state = self.state.borrow();
        let owner = state.account.clone();
        for c in &state.chats {
            if c.name.to_lowercase().contains(&query) {
                let row = adw::ActionRow::builder()
                    .title(&c.name)
                    .subtitle(&c.subtitle)
                    .use_markup(false)
                    .activatable(true)
                    .build();
                let id = c.id.clone();
                let weak = Rc::downgrade(self);
                row.connect_activated(move |_| {
                    if let Some(s) = weak.upgrade() {
                        s.select_chat(&id)
                    }
                });
                self.search_page.append(&row);
            }
            if owner.is_empty() {
                for m in &c.messages {
                    if m.content_text().to_lowercase().contains(&query) {
                        self.append_search_match(c, m);
                    }
                }
            }
        }
        drop(state);
        if owner.is_empty() {
            self.empty_search_status();
            return;
        }
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .halign(gtk::Align::Center)
            .build();
        self.search_page.append(&spinner);
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::MainContext::default().spawn_local(async move {
            let Some(ui) = weak.upgrade() else { return };
            let account = owner.clone();
            let matches = ui
                .store
                .read_snapshot(move |store| store.search_archive(&account, &query))
                .await;
            let state = ui.state.borrow();
            if ui.generation.get() != generation
                || state.account != owner
                || ui.search_revision.get() != revision
            {
                return;
            }
            ui.search_page.remove(&spinner);
            match matches {
                Ok(matches) => {
                    for (peer, message) in matches {
                        if let Some(chat) = state.chats.iter().find(|c| c.jid == peer) {
                            ui.append_search_match(chat, &message);
                        }
                    }
                    ui.empty_search_status();
                }
                Err(_) => ui.notice("History search could not be loaded"),
            }
        });
    }
    fn append_search_match(self: &Rc<Self>, chat: &Chat, message: &Message) {
        let row = adw::ActionRow::builder()
            .title(message.preview())
            .subtitle(format!(
                "{} · {} · {}",
                chat.name, message.author, message.time
            ))
            .title_lines(2)
            .use_markup(false)
            .activatable(true)
            .build();
        let chat = chat.id.clone();
        let id = message.id.clone();
        let weak = Rc::downgrade(self);
        row.connect_activated(move |_| {
            if let Some(s) = weak.upgrade() {
                s.select_chat(&chat);
                s.highlight(&id);
            }
        });
        self.search_page.append(&row);
    }
    fn empty_search_status(&self) {
        if self.search_page.first_child().is_none() {
            self.search_page.append(
                &adw::StatusPage::builder()
                    .title("No Results")
                    .description("Try another search")
                    .icon_name("system-search-symbolic")
                    .build(),
            );
        }
    }
    pub fn command(&self, c: Command) -> bool {
        self.session.borrow().as_ref().is_some_and(|s| s.send(c))
    }
    pub fn send_content(self: &Rc<Self>, value: &str, kind: &str) -> Option<String> {
        self.send_content_attribution(value, kind, None)
    }
    pub fn send_content_attribution(
        self: &Rc<Self>,
        value: &str,
        kind: &str,
        attribution: Option<crate::forwarding::Attribution>,
    ) -> Option<String> {
        let live = !self.state.borrow().account.is_empty();
        if live && (kind != "text" || !self.online.get()) {
            self.notice(if kind != "text" {
                "Live media and rich messages are not available yet."
            } else {
                "Connect before sending. Your draft is kept."
            });
            return None;
        }
        let mode = if live {
            self.sending_mode(self.state.borrow().current())?
        } else {
            crate::encryption_policy::Mode::Plain
        };
        let Some(fingerprint) = self.fingerprint_for_send(mode) else {
            self.notice("The selected OMEMO version is not ready");
            return None;
        };
        let wire = attribution
            .as_ref()
            .map(|a| a.wire_body(value))
            .unwrap_or_else(|| value.into());
        let id = self.state.borrow_mut().send(&wire, kind)?;
        {
            let mut state = self.state.borrow_mut();
            let message = state
                .current_mut()
                .messages
                .iter_mut()
                .find(|m| m.id == id)
                .unwrap();
            message.text = value.into();
            message.forwarded = attribution;
            if live {
                message.set_encryption(mode, fingerprint);
            }
            if kind == "text" {
                message.poll = None;
                message.poll_source = crate::polls::Source::None;
                message.poll_tally = None;
            }
            message.hydrate_compatibility();
        }
        if live {
            let state = self.state.borrow();
            let message = state.message(&id).unwrap().clone();
            let peer = state.current().jid.clone();
            drop(state);
            if !self.persist() {
                self.state
                    .borrow_mut()
                    .current_mut()
                    .messages
                    .retain(|m| m.id != id);
                return None;
            }
            let wire = message.wire_id.clone();
            if !self.command(Command::Send {
                peer,
                message,
                quote: String::new(),
            }) {
                let peer = self.state.borrow().current().jid.clone();
                self.delivery(&peer, &wire, "failed");
                return None;
            }
        }
        self.render_messages(true);
        self.refresh_chats(None);
        Some(id)
    }
    pub fn send(self: &Rc<Self>) {
        let value = self.composer_text();
        if value.trim().is_empty() && self.attachments.borrow().is_empty() {
            return;
        }
        if !self.state.borrow().account.is_empty() {
            let files = self.attachments.borrow().clone();
            if files.is_empty() {
                self.queue_text(&value);
            } else {
                self.queue_local_files(files, &value, false, true);
            }
            return;
        }
        let files = self.attachments.borrow().clone();
        let reply = self.replying.borrow().clone();
        self.choose_sending(move |ui, mode| {
            if ui.composer_text() == value
                && *ui.replying.borrow() == reply
                && *ui.attachments.borrow() == files
            {
                ui.send_with_mode(&value, mode);
            }
        });
    }
    fn send_with_mode(self: &Rc<Self>, value: &str, mode: crate::encryption_policy::Mode) {
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.active_message(id));
        let live = !self.state.borrow().account.is_empty();
        if live && !self.online.get() {
            self.notice("Connect before sending. Your draft is kept.");
            return;
        }
        let fingerprint = if live {
            let Some(fingerprint) = self.fingerprint_for_send(mode) else {
                self.notice("The selected OMEMO version is not ready. Your draft is kept.");
                return;
            };
            fingerprint
        } else {
            String::new()
        };
        let files_sent = live && !self.attachments.borrow().is_empty();
        if files_sent && !self.send_files(value) {
            return;
        }
        if !files_sent && !value.trim().is_empty() {
            let Some(id) = self.state.borrow_mut().send(value, "text") else {
                return;
            };
            let mut state = self.state.borrow_mut();
            let peer = state.current().jid.clone();
            let group = state.current().group;
            let owner = state.account.clone();
            let message = state
                .current_mut()
                .messages
                .iter_mut()
                .find(|m| m.id == id)
                .unwrap();
            if live {
                message.set_encryption(mode, fingerprint);
            }
            if let Some(ref r) = reply {
                message.reply = format!(
                    "{} · {}",
                    r.author,
                    r.preview().chars().take(80).collect::<String>()
                );
                message.reply_id = if live {
                    r.wire_id.clone()
                } else {
                    r.id.clone()
                };
                message.reply_to = if group && !r.sender.is_empty() {
                    r.sender.clone()
                } else if r.own {
                    owner
                } else {
                    peer.clone()
                };
            }
            let message = message.clone();
            drop(state);
            if live {
                if !self.persist() {
                    self.state
                        .borrow_mut()
                        .current_mut()
                        .messages
                        .retain(|m| m.id != id);
                    return;
                }
                self.command(Command::Send {
                    peer,
                    message,
                    quote: reply.map(|r| r.preview()).unwrap_or_default(),
                });
            }
        }
        if !live {
            let files = self.attachments.borrow().clone();
            for file in files {
                self.state.borrow_mut().send(
                    file.basename()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                    "file",
                );
            }
        }
        self.loading.set(true);
        self.composer.buffer().set_text("");
        self.loading.set(false);
        self.attachments.borrow_mut().clear();
        clear(&self.chips);
        self.chips.set_visible(false);
        self.cancel_reply();
        self.save_draft();
        self.render_messages(true);
        self.refresh_chats(None);
    }
    pub fn update_reply(&self) {
        let reply = self
            .replying
            .borrow()
            .as_ref()
            .and_then(|id| self.active_message(id));
        if let Some(m) = reply {
            self.reply_label.set_text(&format!(
                "Reply to {} · {}",
                m.author,
                m.preview().chars().take(60).collect::<String>()
            ));
            self.reply_bar.set_reveal_child(true);
        } else {
            self.reply_bar.set_reveal_child(false)
        }
    }
    pub fn cancel_reply(&self) {
        self.replying.borrow_mut().take();
        self.reply_bar.set_reveal_child(false);
        if !self.loading.get() {
            self.save_draft()
        }
    }
    pub fn update_mode(&self) {
        let live = !self.state.borrow().account.is_empty();
        self.update_history_spinner();
        self.stories.set_visible(!live);
        for name in [
            "wallet",
            "stories",
            "spaces",
            "profile",
            "developer",
            "call",
            "article",
            "location",
            "payment",
        ] {
            if let Some(a) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(!live)
            }
        }
        if let Some(action) = self
            .window
            .lookup_action("poll")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(!live || self.online.get());
        }
        for (name, t) in self.media_toggles.borrow().iter() {
            t.set_enabled(
                matches!(name.as_str(), "emoji" | "stickers")
                    || name == "gifs" && self.gif_page.borrow().is_some()
                    || !live,
            )
        }
        if live {
            self.media_stack.set_visible_child_name("emoji")
        }
    }
    pub fn on_welcome(&self) -> bool {
        self.shell.visible_child_name().as_deref() == Some("welcome")
    }
    pub fn show_main(&self) {
        self.shell.set_visible_child_name("main");
    }
    pub fn show_welcome(&self) {
        self.shell.set_visible_child_name("welcome");
        self.welcome_account.borrow_mut().take();
        self.show_welcome_status("", false, false);
    }
    fn show_welcome_status(&self, status: &str, connecting: bool, error: bool) {
        self.welcome_status.set_text(status);
        self.welcome_status.set_visible(!status.is_empty());
        if error {
            self.welcome_status.add_css_class("error");
        } else {
            self.welcome_status.remove_css_class("error");
        }
        self.welcome_progress.set_spinning(connecting);
        self.welcome_progress.set_visible(connecting);
        self.welcome_cancel.set_visible(connecting);
        self.welcome_sign_in.set_visible(!connecting);
        self.welcome_register.set_visible(!connecting);
        self.welcome_sign_in
            .set_label(if error { "Try Again" } else { "Sign In" });
    }
    pub fn activate_state(self: &Rc<Self>, mut state: State) {
        self.stop_uploads();
        self.images.borrow_mut().clear();
        self.avatars.borrow_mut().clear();
        for chat in &mut state.chats {
            for m in &mut chat.messages {
                if let Some(draft) = &mut m.upload {
                    if m.delivery == "sending" {
                        m.delivery = "failed".into();
                        draft.error = Some("Upload interrupted. Retry to send.".into());
                    }
                }
            }
        }
        self.downloads.borrow_mut().clear();
        self.save_draft();
        if self.state.borrow().account.is_empty() && !self.on_welcome() {
            self.demo.replace(self.state.borrow().clone());
        }
        self.session.borrow_mut().take();
        self.login_secret.borrow_mut().take();
        self.generation.set(self.generation.get() + 1);
        self.online.set(false);
        self.crypto.replace(Default::default());
        self.legacy_crypto.replace(Default::default());
        self.pq_crypto.replace(Default::default());
        self.pq_reviews.borrow_mut().clear();
        self.encryption_checks.borrow_mut().clear();
        self.security_widgets.borrow_mut().take();
        self.contact_access_widgets.borrow_mut().take();
        self.link_preview_jobs.borrow_mut().cancel();
        self.read_ui.replace(Default::default());
        self.review_request.borrow_mut().take();
        if let Some(library) = self.pack_library.borrow_mut().take() {
            library.cancel();
        }
        self.pack_pages.borrow_mut().clear();
        if let Some(library) = self.gif_library.borrow_mut().take() {
            library.cancel();
        }
        self.gif_page.borrow_mut().take();
        self.clear_composer_inline_emoji();
        self.status.replace("Disconnected".into());
        let dialogs = std::mem::take(&mut *self.dialogs.borrow_mut());
        for d in dialogs {
            d.close();
        }
        let media = self.active_media.borrow().clone();
        if let Some(v) = media {
            v.close_immediate()
        }
        let story = self.active_story.borrow().clone();
        if let Some(v) = story {
            v.close()
        }
        self.state.replace(state);
        self.rebuild_pack_library();
        self.rebuild_gif_library();
        self.timeline_scope_changed();
        self.history_pages.borrow_mut().clear();
        self.history_pending.borrow_mut().clear();
        self.typing.borrow_mut().clear();
        self.rebuild_folders();
        self.refresh_chats(None);
        self.refresh_stories();
        self.load_current();
        self.update_mode();
        if !self.on_welcome() {
            let _ = self
                .store
                .set_pref("active_account", &self.state.borrow().account);
        }
    }
    pub fn connect_account(self: &Rc<Self>, config: Account, password: String) {
        self.start_account(config, password, true);
    }
    fn start_account(self: &Rc<Self>, config: Account, password: String, remember: bool) {
        self.save_draft();
        let onboarding = self.on_welcome();
        if !onboarding && self.store.save_account(&config).is_err() {
            self.notice("Account storage is unavailable");
            return;
        }
        let state = if onboarding {
            State::live(&config.jid, vec![])
        } else if self.state.borrow().account == config.jid {
            self.state.borrow().clone()
        } else {
            self.store.load_windowed(&config.jid)
        };
        self.activate_state(state);
        if remember && !self.smoke {
            self.login_secret
                .replace(Some(zeroize::Zeroizing::new(password.clone())));
        }
        self.status.replace("Connecting…".into());
        if onboarding {
            self.welcome_account.replace(Some(config.clone()));
            self.show_welcome_status("Connecting…", true, false);
        }
        let Ok(rooms) = self.store.room_choices(&config.jid) else {
            self.notice("Saved room settings could not be read");
            return;
        };
        let session = Session::start_with_rooms(
            config,
            password,
            self.generation.get(),
            self.event_tx.clone(),
            rooms,
        );
        self.session.replace(Some(session));
        self.update_mode();
    }
    pub fn reconnect_saved(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() || self.store.pref(&format!("auto-connect:{owner}"), "true") != "true" {
            return;
        }
        let Some(config) = self
            .store
            .accounts()
            .into_iter()
            .find(|account| account.jid == owner)
        else {
            return;
        };
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        self.status.replace("Looking up saved sign-in…".into());
        glib::MainContext::default().spawn_local(async move {
            let password = crate::credentials::load(&owner).await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.session.borrow().is_none()
                    && ui.state.borrow().account == owner
            }) else {
                return;
            };
            match password {
                Ok(password) => ui.start_account(config, password.to_string(), false),
                Err(crate::credentials::Error::Missing) => {
                    ui.status
                        .replace("Sign in to reconnect automatically".into());
                    if ui.dialogs.borrow().is_empty() {
                        ui.login(Some(config));
                    }
                }
                Err(error) => {
                    ui.status.replace("Disconnected".into());
                    ui.notice(&format!("Could not use saved sign-in. {error}"));
                }
            }
        });
    }
    fn remember_sign_in(self: &Rc<Self>, owner: &str) {
        let Some(password) = self.login_secret.borrow_mut().take() else {
            return;
        };
        let generation = self.generation.get();
        let owner = owner.to_string();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = crate::credentials::store(&owner, password).await;
            let Some(ui) = weak.upgrade().filter(|ui| {
                ui.generation.get() == generation && ui.state.borrow().account == owner
            }) else {
                return;
            };
            match result {
                Ok(()) => {
                    let _ = ui.store.set_pref(&format!("auto-connect:{owner}"), "true");
                }
                Err(error) => {
                    ui.notice(&format!("Could not save sign-in for next launch. {error}"))
                }
            }
        });
    }
    pub fn backend_event(self: &Rc<Self>, generation: u64, owner: &str, event: BackendEvent) {
        if generation != self.generation.get() || owner != self.state.borrow().account {
            return;
        }
        if self.on_welcome() {
            let BackendEvent::Status {
                online,
                error,
                text,
            } = &event
            else {
                return;
            };
            if !online {
                if *error {
                    self.session.borrow_mut().take();
                    self.login_secret.borrow_mut().take();
                    self.generation.set(self.generation.get() + 1);
                }
                self.show_welcome_status(text, !error, *error);
                return;
            }
            let config = self.welcome_account.borrow().clone();
            if config.is_none_or(|config| self.store.save_account(&config).is_err()) {
                self.session.borrow_mut().take();
                self.login_secret.borrow_mut().take();
                self.generation.set(self.generation.get() + 1);
                self.show_welcome_status("Account storage is unavailable", false, true);
                return;
            }
            let _ = self.store.set_pref("active_account", owner);
            self.welcome_account.borrow_mut().take();
            self.show_main();
        }
        match event {
            BackendEvent::Status {
                online,
                error,
                text,
            } => {
                if !online {
                    self.encryption_checks.borrow_mut().clear();
                    self.update_encryption_check();
                    self.encryption_revision
                        .set(self.encryption_revision.get().wrapping_add(1));
                    self.legacy_crypto.borrow_mut().ready = false;
                    self.pq_crypto.borrow_mut().ready = false;
                    self.history_pages.borrow_mut().clear();
                    self.history_pending.borrow_mut().clear();
                    self.update_history_spinner();
                }
                self.online.set(online);
                self.status.replace(text.clone());
                if !online {
                    self.crypto.borrow_mut().ready = false;
                    self.update_crypto_controls();
                    self.typing.borrow_mut().clear();
                    let mut s = self.state.borrow_mut();
                    for c in &mut s.chats {
                        if c.group {
                            c.subtitle = "Disconnected".into();
                        } else if c.jid != owner {
                            c.subtitle.clear();
                        }
                        c.room_joined = false;
                        c.room_can_send = false;
                        for m in &mut c.messages {
                            if m.delivery == "sending" {
                                m.delivery = "uncertain".into()
                            }
                        }
                    }
                }
                if !online {
                    let _ = self.store.interrupt_messages(owner);
                    let _ = self.store.interrupt_actions(owner);
                    let peers: Vec<_> = self
                        .state
                        .borrow()
                        .chats
                        .iter()
                        .map(|c| c.jid.clone())
                        .collect();
                    for peer in peers {
                        if self.store.reconcile_actions(owner, &peer).unwrap_or(false) {
                            self.sync_actions(owner, &peer);
                        }
                    }
                }
                self.update_mode();
                self.refresh_contact_access();
                if online {
                    self.flush_send_queue();
                    self.prepare_loaded_link_previews();
                    self.room_sync_retry.borrow_mut().clear();
                    self.sync_room_bookmarks();
                    self.remember_sign_in(owner);
                    self.load_history(true)
                } else if error {
                    self.login_secret.borrow_mut().take();
                    self.notice(&text)
                }
            }
            BackendEvent::Roster(update) => {
                let mut resolved = Vec::new();
                let mut s = self.state.borrow_mut();
                if update.full {
                    for chat in &mut s.chats {
                        if !chat.group {
                            chat.contact = false;
                        }
                    }
                }
                for item in update.items {
                    if matches!(item.subscription.as_str(), "from" | "both") {
                        resolved.push(item.jid.clone());
                    }
                    let c = s.chat_for(&item.jid);
                    if !item.name.is_empty() {
                        c.name = item.name;
                    }
                    c.contact = item.subscription != "remove" && !c.group && c.jid != owner;
                }
                drop(s);
                self.clear_contact_requests(&resolved);
                self.refresh_contact_access();
            }
            BackendEvent::ContactAccessError(error) => self.notice(&error),
            BackendEvent::RoomBookmarks(bookmarks) => {
                if self
                    .store
                    .reconcile_room_choices(owner, &bookmarks)
                    .is_err()
                {
                    self.notice("Saved room settings could not be updated");
                }
                let mut state = self.state.borrow_mut();
                for bookmark in bookmarks {
                    let room = state.chat_for(&bookmark.jid);
                    room.room_autojoin = Some(bookmark.autojoin);
                    room.room_nick = bookmark.nick.clone();
                    room.group = true;
                    room.contact = false;
                    room.name = if bookmark.name.is_empty() {
                        bookmark.jid
                    } else {
                        bookmark.name
                    };
                }
            }
            BackendEvent::RoomPresence(presence) => {
                let mut state = self.state.borrow_mut();
                let room = state.chat_for(&presence.jid);
                room.group = true;
                room.contact = false;
                room.room_joined = presence.joined;
                room.room_can_send = presence.can_send;
                room.subtitle = presence.detail;
                let active = state.current().jid == presence.jid;
                drop(state);
                if active && self.pages.visible_child_name().as_deref() == Some("chat") {
                    self.show_chat();
                    if presence.joined {
                        self.load_history(true);
                    }
                }
                if presence.joined {
                    self.release_upload_captions();
                }
            }
            BackendEvent::RoomError(error) => self.notice(&error),
            BackendEvent::CryptoStatus(status) => {
                self.crypto.replace(status);
                self.update_crypto_controls();
                self.update_mode();
                self.flush_send_queue();
            }
            BackendEvent::PqCrypto(status) => {
                let became_ready = status.ready && !self.pq_crypto.borrow().ready;
                self.pq_crypto.replace(status);
                if became_ready {
                    self.encryption_revision
                        .set(self.encryption_revision.get().wrapping_add(1));
                    for chat in &mut self.state.borrow_mut().chats {
                        if chat.encryption_preference()
                            == crate::encryption_policy::Preference::Automatic
                        {
                            chat.automatic_encryption = None;
                        }
                    }
                }
                self.update_crypto_controls();
                self.update_mode();
                self.flush_send_queue();
            }
            BackendEvent::PqDevices { peer, devices } => {
                self.pq_reviews.borrow_mut().insert(peer.clone(), devices);
                self.handle_pq_review(&peer);
            }
            BackendEvent::AvatarChanged { peer, hint } => {
                self.avatars.borrow_mut().invalidate(&peer, hint);
                return;
            }
            BackendEvent::LegacyStatus(status) => {
                self.legacy_crypto.replace(status);
                self.update_crypto_controls();
                self.release_upload_captions();
                self.flush_send_queue();
            }
            BackendEvent::LegacyReview { peer, rows, errors } => {
                self.handle_device_review(
                    &peer,
                    crate::encryption_policy::Mode::Legacy,
                    rows,
                    errors,
                );
            }
            BackendEvent::CryptoReview { peer, rows, errors } => {
                self.handle_device_review(
                    &peer,
                    crate::encryption_policy::Mode::Omemo2,
                    rows,
                    errors,
                );
            }
            BackendEvent::CryptoError { peer, text } => self.notice(&format!("{peer}: {text}")),
            BackendEvent::InlineEmoji { peer, id, emoji } => {
                if self
                    .store
                    .restore_inline_emoji(owner, &peer, &id, &emoji)
                    .unwrap_or(false)
                {
                    let mut state = self.state.borrow_mut();
                    if let Some(message) = state
                        .chats
                        .iter_mut()
                        .find(|chat| chat.jid == peer)
                        .and_then(|chat| chat.messages.iter_mut().find(|message| message.id == id))
                        .filter(|message| {
                            message.inline_emoji.is_none() && message.text == emoji.body
                        })
                    {
                        message.inline_emoji = Some(emoji);
                    }
                    let active = state.current().jid == peer;
                    drop(state);
                    if active {
                        self.render_messages(false);
                    }
                }
            }
            BackendEvent::PreviewSpoilers { peer, id, context } => {
                if self
                    .store
                    .restore_preview_spoilers(owner, &peer, &id, &context)
                    .unwrap_or(false)
                {
                    let mut state = self.state.borrow_mut();
                    if let Some(message) = state
                        .chats
                        .iter_mut()
                        .find(|chat| chat.jid == peer)
                        .and_then(|chat| chat.messages.iter_mut().find(|message| message.id == id))
                        .filter(|message| {
                            message.preview_spoilers.is_none() && context.matches(&message.text)
                        })
                    {
                        message.preview_spoilers = Some(context);
                    }
                    let active = state.current().jid == peer;
                    drop(state);
                    if active {
                        self.render_messages(false);
                    }
                }
            }
            BackendEvent::CryptoAliases {
                legacy_sticker,
                legacy_reply,
                markable_id,
                peer,
                own,
                id,
                aliases,
            } => {
                if self
                    .store
                    .merge_aliases(owner, &peer, own, &id, &aliases)
                    .and_then(|_| {
                        if !markable_id.is_empty() {
                            self.store
                                .restore_markable(owner, &peer, &id, &markable_id)?;
                        }
                        if legacy_sticker {
                            self.store.restore_legacy_sticker(owner, &peer, &id)?;
                        }
                        if let Some(reply) = legacy_reply {
                            self.store.restore_legacy_reply(owner, &peer, &id, reply)?;
                        }
                        Ok(())
                    })
                    .and_then(|_| self.store.reconcile_actions(owner, &peer).map(|_| ()))
                    .is_ok()
                {
                    self.sync_actions(owner, &peer);
                    if self.state.borrow().current().jid == peer {
                        self.render_messages(false);
                    }
                }
            }
            BackendEvent::EncryptedMessage {
                peer,
                message,
                historical,
                receipt_to,
                token,
            } => {
                if self.receive_message(owner, peer, message, historical, receipt_to) {
                    self.command(Command::Crypto(crate::crypto::service::Request::Accepted(
                        token,
                    )));
                }
                self.queue_chat_refresh();
                return;
            }
            BackendEvent::PqEncrypted {
                peer,
                message,
                historical,
                receipt_to,
                token,
            } => {
                if self.receive_message(owner, peer, message, historical, receipt_to) {
                    self.command(Command::PqCrypto(
                        crate::crypto::pq::service::Request::Accepted(token),
                    ));
                }
                self.queue_chat_refresh();
                return;
            }
            BackendEvent::LegacyEncryptedMessage {
                peer,
                message,
                historical,
                receipt_to,
                token,
            } => {
                if self.receive_message(owner, peer, message, historical, receipt_to) {
                    self.command(Command::Legacy(
                        crate::crypto::legacy::service::Request::Accepted(token),
                    ));
                }
                self.queue_chat_refresh();
                return;
            }
            BackendEvent::Message {
                peer,
                message,
                historical,
                receipt_to,
            } => {
                self.receive_message(owner, peer, message, historical, receipt_to);
                self.queue_chat_refresh();
                return;
            }
            BackendEvent::Presence {
                peer,
                online,
                visible,
            } => {
                let mut s = self.state.borrow_mut();
                if let Some(c) = s.chats.iter_mut().find(|c| c.jid == peer) {
                    c.subtitle = if !visible {
                        ""
                    } else if online {
                        "Online"
                    } else {
                        "Offline"
                    }
                    .into();
                }
                drop(s);
                if self.pages.visible_child_name().as_deref() == Some("chat") {
                    self.show_chat()
                }
                return;
            }
            BackendEvent::Typing { peer, composing } => {
                if composing {
                    self.typing.borrow_mut().insert(peer, Instant::now());
                } else {
                    self.typing.borrow_mut().remove(&peer);
                }
                if self.pages.visible_child_name().as_deref() == Some("chat") {
                    self.show_chat()
                }
                return;
            }
            BackendEvent::Uncertain { peer, id } => {
                self.delivery(&peer, &id, "uncertain");
                self.notice("Encrypted delivery is uncertain. Retrying may duplicate it.");
            }
            BackendEvent::Sent { peer, id } => {
                self.delivery(&peer, &id, "sent");
                self.release_upload_captions();
                self.prepare_loaded_link_previews();
            }
            BackendEvent::Read { peer, id } => self.receive_read_marker(owner, &peer, &id, false),
            BackendEvent::OwnRead { peer, id } => self.receive_read_marker(owner, &peer, &id, true),
            BackendEvent::Receipt { peer, id } => self.delivery(&peer, &id, "delivered"),
            BackendEvent::Failed { peer, id } => {
                self.delivery(&peer, &id, "failed");
                self.notice("Message could not be sent. Retry from its menu.");
            }
            BackendEvent::History {
                peer,
                before,
                complete,
                error,
            } => {
                self.history_pending.borrow_mut().remove(&peer);
                self.update_history_spinner();
                if error {
                    self.notice("Server history unavailable or unsupported")
                } else {
                    self.history_pages
                        .borrow_mut()
                        .insert(peer, (before, complete));
                }
            }
            BackendEvent::Request(peer) => {
                self.receive_contact_request(peer);
                self.refresh_contact_access();
            }
            BackendEvent::RequestCancelled(peer) => {
                self.clear_contact_request(&peer);
                self.refresh_contact_access();
            }
            BackendEvent::RoomInvitation(invitation) => {
                self.receive_room_invitation(invitation);
            }
        }
        self.persist();
        self.refresh_chats(None);
    }
    pub fn receive_message(
        self: &Rc<Self>,
        owner: &str,
        peer: String,
        mut message: Message,
        historical: bool,
        receipt_to: Option<String>,
    ) -> bool {
        if message.action.is_some() {
            message.author = if message.own {
                "You".into()
            } else {
                self.state.borrow_mut().chat_for(&peer).name.clone()
            };
            let visible = self.state.borrow().current().jid == peer && self.reading_current_chat();
            message.unread = matches!(
                message.action,
                Some(crate::message_actions::Action::Correct { .. })
            ) && !message.own
                && !historical
                && !visible;
            let saved = self
                .store
                .record_action(owner, &peer, &message, &message.delivery)
                .and_then(|_| self.store.reconcile_actions(owner, &peer).map(|_| ()))
                .is_ok();
            if saved {
                self.sync_actions(owner, &peer);
                if self.state.borrow().current().jid == peer {
                    self.render_messages(false);
                }
                if let Some(to) = receipt_to {
                    if self.preference("delivery_receipts", false) {
                        self.command(Command::Receipt {
                            peer: to,
                            id: message.wire_id,
                        });
                    }
                }
            } else {
                self.notice("Message change could not be saved");
            }
            return saved;
        }
        let visible = self.state.borrow().current().jid == peer && self.reading_current_chat();
        let incoming_id = message.id.clone();
        let incoming_aliases = message.aliases.clone();
        let known = self
            .store
            .known_message(owner, &peer, &message)
            .or_else(|| {
                self.state
                    .borrow()
                    .chats
                    .iter()
                    .find(|c| c.jid == peer)
                    .and_then(|c| {
                        c.messages.iter().find(|m| {
                            crate::storage::same_source(m, &message)
                                && m.aliases
                                    .iter()
                                    .any(|alias| message.aliases.contains(alias))
                        })
                    })
                    .map(|m| m.id.clone())
            });
        let follow = self.at_message_end();
        let mut s = self.state.borrow_mut();
        let active = s.current().jid == peer;
        let c = s.chat_for(&peer);
        let new_key = message.order_key();
        let outside = !active
            || c.messages.first().is_some_and(|edge| {
                new_key < edge.order_key() && self.history_window.has_older_gap()
            })
            || c.messages
                .last()
                .is_some_and(|edge| new_key > edge.order_key() && !self.history_window.newest());
        let mut inserted = None;
        let mut message_own = message.own;
        let receipt_id = message.wire_id.clone();
        let notify =
            known.is_none() && !visible && !message.own && !historical && c.notifications == "all";
        let notification_title = c.name.clone();
        let notification_body = message.preview();
        if let Some(known) = known.clone() {
            let old = c
                .messages
                .iter()
                .find(|m| m.id == known)
                .cloned()
                .or_else(|| self.store.message(owner, &peer, &known));
            if let Some(old) = old {
                let restored_markable = !message.markable_id.is_empty()
                    && self
                        .store
                        .restore_markable(owner, &peer, &known, &message.markable_id)
                        .unwrap_or(false);
                let restored_context = message
                    .preview_spoilers
                    .as_ref()
                    .filter(|context| {
                        old.preview_spoilers.is_none()
                            && context.matches(&old.text)
                            && old.mode() == message.mode()
                            && (!old.encrypted || old.fingerprint == message.fingerprint)
                    })
                    .cloned();
                if let Some(context) = &restored_context {
                    let _ = self
                        .store
                        .restore_preview_spoilers(owner, &peer, &known, context);
                }
                let restored_emoji = message
                    .inline_emoji
                    .as_ref()
                    .filter(|emoji| {
                        old.inline_emoji.is_none()
                            && emoji.body == old.text
                            && old.mode() == message.mode()
                            && (!old.encrypted || old.fingerprint == message.fingerprint)
                    })
                    .cloned();
                if let Some(emoji) = &restored_emoji {
                    let _ = self.store.restore_inline_emoji(owner, &peer, &known, emoji);
                }
                message_own = old.own;
                if old.unavailable && !message.unavailable {
                    message.id = known.clone();
                    message.own = old.own;
                    message.author = old.author;
                    message.timestamp = old.timestamp;
                    message.time = old.time;
                    message.unread = old.unread;
                    for alias in old.aliases {
                        if !message.aliases.contains(&alias) {
                            message.aliases.push(alias);
                        }
                    }
                    if let Some(row) = c.messages.iter_mut().find(|m| m.id == known) {
                        *row = message;
                    } else {
                        inserted = Some(message.id.clone());
                        c.messages.push(message);
                        c.messages.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
                    }
                } else if let Some(old) = c.messages.iter_mut().find(|m| m.id == known) {
                    if restored_markable {
                        old.markable_id = message.markable_id.clone();
                    }
                    if let Some(emoji) = restored_emoji {
                        old.inline_emoji = Some(emoji);
                    }
                    if let Some(context) = restored_context {
                        old.preview_spoilers = Some(context);
                    }
                    if old.own && message.own && message.legacy_room_sender.is_some() {
                        old.sender = message.sender.clone();
                        old.legacy_room_sender = message.legacy_room_sender.clone();
                    }
                    for alias in &incoming_aliases {
                        if !old.aliases.contains(alias) {
                            old.aliases.push(alias.clone());
                        }
                    }
                }
            }
        } else {
            message.author = if message.own {
                "You".into()
            } else if c.group && !message.author.is_empty() {
                message.author
            } else {
                c.name.clone()
            };
            if !message.reply_id.is_empty() {
                if let Some(target) =
                    self.store
                        .resolve_reply(owner, &peer, &message.reply_to, &message.reply_id)
                {
                    message.reply = format!(
                        "{} · {}",
                        target.author,
                        target.preview().chars().take(80).collect::<String>()
                    )
                } else if message.reply.is_empty() {
                    message.reply = "Original message is not loaded".into()
                }
            }
            message.unread = false;
            if !visible && !message.own && !historical {
                c.unread += 1;
                message.unread = true;
            }
            inserted = Some(message.id.clone());
            c.messages.push(message);
            c.messages.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        }
        let follow =
            active && follow && (!historical || !self.history_pages.borrow().contains_key(&peer));
        c.remember_latest();
        let changed = inserted
            .as_ref()
            .or(known.as_ref())
            .and_then(|id| c.messages.iter().find(|message| &message.id == id));
        let mut saved = self.store.save_chat_message(owner, c, changed).is_ok();
        drop(s);
        if !saved {
            self.notice("Local history could not be saved. Check free space and permissions.");
        }
        if saved && active {
            self.history_window.content_changed();
        }
        if saved && outside {
            if let Some(inserted) = &inserted {
                self.state
                    .borrow_mut()
                    .chat_for(&peer)
                    .messages
                    .retain(|m| &m.id != inserted);
                if active && historical {
                    self.history_window.invalidate_older();
                }
            }
        }
        if saved {
            if let Some(id) = known {
                saved = self
                    .store
                    .merge_aliases(owner, &peer, message_own, &id, &incoming_aliases)
                    .is_ok();
            }
        }
        if saved {
            if let Some(to) = receipt_to {
                if self.preference("delivery_receipts", false) {
                    self.command(Command::Receipt {
                        peer: to,
                        id: receipt_id,
                    });
                }
            }
        }
        if saved {
            match self.store.reconcile_actions(owner, &peer) {
                Ok(true) => self.sync_actions(owner, &peer),
                Ok(false) => {}
                Err(_) => {
                    self.notice("Message changes could not be reconciled");
                    saved = false;
                }
            }
        }
        if saved
            && notify
            && self
                .store
                .message(owner, &peer, &incoming_id)
                .is_some_and(|m| !m.retracted)
            && self.preference("notifications", true)
        {
            if let Some(app) = self.window.application() {
                let notification = gio::Notification::new(&notification_title);
                notification.set_body(Some(if self.preference("message_previews", false) {
                    &notification_body
                } else {
                    "New message"
                }));
                app.send_notification(None, &notification);
            }
        }
        if active && (!outside || inserted.is_none()) {
            self.render_messages(follow)
        } else if active {
            self.schedule_history_maintenance();
        }
        saved
    }
    pub fn delivery(self: &Rc<Self>, peer: &str, id: &str, status: &str) {
        let owner = self.state.borrow().account.clone();
        match self.store.action_delivery(&owner, peer, id, status) {
            Ok(true) => {
                self.sync_actions(&owner, peer);
                if matches!(status, "failed" | "uncertain") {
                    self.notice("Message change failed or delivery is uncertain. Reopen Edit to recover its text.");
                }
                self.render_messages(false);
                self.refresh_chats(None);
                return;
            }
            Err(_) => {
                self.notice("Message change status could not be saved");
                return;
            }
            Ok(false) => {}
        }
        match self.store.update_delivery(&owner, peer, id, status) {
            Ok(Some(message)) => {
                let mut state = self.state.borrow_mut();
                let active = state.current().jid == peer;
                let chat = state.chat_for(peer);
                if let Some(row) = chat.messages.iter_mut().find(|m| m.id == message.id) {
                    *row = message;
                }
                chat.remember_latest();
                drop(state);
                if active {
                    self.history_window.content_changed();
                    self.render_messages(false);
                }
            }
            Ok(None) => {}
            Err(_) => self.notice("Message delivery status could not be saved"),
        }
    }
    pub(crate) fn update_history_spinner(&self) {
        let state = self.state.borrow();
        let busy = !state.account.is_empty()
            && (self.history_window.busy()
                || self.history_pending.borrow().contains(&state.current().jid));
        self.history.set_visible(busy);
        self.history.set_spinning(busy);
    }
    pub fn load_history(self: &Rc<Self>, initial: bool) {
        let state = self.state.borrow();
        if state.account.is_empty() {
            return;
        }
        let peer = state.current().jid.clone();
        drop(state);
        if initial {
            if !self.history_pages.borrow().contains_key(&peer) {
                self.request_server_history(&peer);
            }
        } else {
            self.history_edge(crate::history_window::Side::Older);
        }
    }
    pub(crate) fn request_server_history(&self, peer: &str) {
        if !self.online.get() || self.history_pending.borrow().contains(peer) {
            return;
        }
        if self
            .state
            .borrow()
            .chats
            .iter()
            .any(|chat| chat.jid == peer && chat.group && !chat.room_joined)
        {
            return;
        }
        let before = match self.history_pages.borrow().get(peer) {
            Some((_, true)) => return,
            Some((before, false)) => before.clone(),
            None => String::new(),
        };
        if self.command(Command::History {
            peer: peer.into(),
            before,
        }) {
            self.history_pending.borrow_mut().insert(peer.into());
            self.update_history_spinner();
        }
    }
    pub fn preference(&self, key: &str, default: bool) -> bool {
        self.store.pref(
            &format!("{key}:{}", self.state.borrow().account),
            if default { "true" } else { "false" },
        ) == "true"
    }
}
