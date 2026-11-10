use crate::{dialogs::bind_amount, model::*, ui::Ui, wallet::WalletCard, widgets::*};
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
impl Ui {
    pub fn track_sidebar(&self, widget: &impl IsA<gtk::Widget>) {
        widget.set_visible(self.utility.shows_sidebar());
        let utility = self.utility.downgrade();
        widget.connect_unmap(move |pane| {
            if let Some(u) = utility.upgrade() {
                if !u.shows_sidebar() {
                    pane.set_visible(false)
                }
            }
        });
    }
    pub fn make_media(self: &Rc<Self>) {
        self.media_pane.add_css_class("utility-sidebar");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .build();
        let close = icon_button("sidebar-show-symbolic", "Close media pane");
        let utility = self.utility.downgrade();
        close.connect_clicked(move |_| {
            if let Some(u) = utility.upgrade() {
                u.set_show_sidebar(false)
            }
        });
        header.pack_start(&close);
        let toggle = adw::ToggleGroup::builder()
            .homogeneous(false)
            .can_shrink(false)
            .build();
        for (name, title) in [
            ("emoji", "Emoji"),
            ("stickers", "Stickers"),
            ("gifs", "GIFs"),
            ("monero", "Monero"),
        ] {
            let t = adw::Toggle::builder().name(name).label(title).build();
            toggle.add(t.clone());
            self.media_toggles.borrow_mut().insert(name.into(), t);
        }
        let stack = self.media_stack.downgrade();
        toggle.connect_active_name_notify(move |t| {
            if let (Some(stack), Some(name)) = (stack.upgrade(), t.active_name()) {
                stack.set_visible_child_name(&name)
            }
        });
        header.set_title_widget(Some(&toggle));
        self.media_pane.add_top_bar(&header);
        let body = bx(true, 12);
        body.set_margin_top(12);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search")
            .margin_start(12)
            .margin_end(12)
            .build();
        body.append(&search);
        let entries = Rc::new(RefCell::new(vec![]));
        let gifs_grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .column_spacing(4)
            .row_spacing(4)
            .build();
        for (kind, items) in [
            (
                "emoji",
                vec![
                    "😀", "😄", "🥹", "😊", "😍", "🥰", "😎", "🤔", "🫡", "🙃", "😭", "😂", "❤️",
                    "💜", "✨", "🎉", "👍", "👋", "☕", "🍀", "🌿", "🌸", "🌻", "🌈", "🌙", "🔥",
                    "🦊", "🐈", "🐻", "🐸", "🍋", "🍊", "🍓", "🍕", "🎨", "📷", "💻", "🚀", "🎧",
                    "🏕️", "🌊", "🏔️", "💫", "🙌", "🤝", "💬", "🫶", "😴",
                ],
            ),
            (
                "stickers",
                vec!["🦊", "🐻", "🐸", "🐈", "☕", "🎉", "🌸", "🚀"],
            ),
            (
                "gifs",
                vec![
                    "Happy dance",
                    "Good morning",
                    "High five",
                    "You got this",
                    "Weekend mood",
                    "Well done",
                ],
            ),
        ] {
            let area = bx(true, 8);
            margins(&area, 12);
            area.set_margin_top(0);
            area.append(&text(
                if kind == "emoji" {
                    "Recently used"
                } else {
                    "Favorites"
                },
                "caption dim-label",
            ));
            let cols = if kind == "emoji" { 6 } else { 4 };
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(4)
                .row_spacing(4)
                .min_children_per_line(cols)
                .max_children_per_line(cols)
                .build();
            flow.add_css_class("media-grid");
            gifs_grid.add_css_class("media-grid");
            for (index, item) in items.into_iter().enumerate() {
                let b = gtk::Button::builder().tooltip_text(item).build();
                if kind == "gifs" {
                    b.set_child(Some(&AnimatedImage::new(index)));
                    b.add_css_class("gif-button");
                    gifs_grid.attach(&b, index as i32 % 2, index as i32 / 2, 1, 1);
                    entries.borrow_mut().push((
                        b.clone().upcast::<gtk::Widget>(),
                        item.to_string(),
                        true,
                    ));
                } else {
                    b.set_label(item);
                    b.add_css_class("flat");
                    b.add_css_class(if kind == "emoji" {
                        "emoji-item"
                    } else {
                        "sticker-item"
                    });
                    flow.insert(&b, -1);
                    let child = b.parent().unwrap();
                    child.set_focusable(false);
                    entries.borrow_mut().push((child, item.to_string(), false));
                }
                let weak = Rc::downgrade(self);
                b.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        if kind == "emoji" {
                            ui.composer.buffer().insert_at_cursor(item);
                            ui.composer.grab_focus();
                        } else if let Some(id) =
                            ui.send_content(item, if kind == "gifs" { "gif" } else { "sticker" })
                        {
                            if let Some(m) = ui
                                .state
                                .borrow_mut()
                                .current_mut()
                                .messages
                                .iter_mut()
                                .find(|m| m.id == id)
                            {
                                m.gif_index = index
                            }
                            ui.render_messages(true);
                        }
                    }
                });
            }
            if kind == "gifs" {
                area.append(&gifs_grid)
            } else {
                area.append(&flow)
            }
            let scroll = scroll(&area);
            scroll.set_vexpand(true);
            self.media_stack.add_named(&scroll, Some(kind));
        }
        let money = bx(true, 12);
        margins(&money, 12);
        let card = WalletCard::new("Everyday", "1.2048", true, || {});
        money.append(&card.root);
        self.sidebar_card.replace(Some(card));
        let mode = adw::ToggleGroup::builder().homogeneous(true).build();
        for (n, l) in [("send", "Send"), ("request", "Request")] {
            mode.add(adw::Toggle::builder().name(n).label(l).build())
        }
        money.append(&mode);
        let amount = gtk::Entry::builder()
            .placeholder_text("Amount")
            .input_purpose(gtk::InputPurpose::Number)
            .build();
        money.append(&amount);
        let currency = gtk::DropDown::from_strings(&["XMR", "USD", "EUR", "PLN"]);
        money.append(&currency);
        let note = gtk::Entry::builder().placeholder_text("Add a note").build();
        money.append(&note);
        let review = gtk::Button::with_label("Review");
        review.add_css_class("suggested-action");
        bind_amount(&amount, &review);
        let weak = Rc::downgrade(self);
        review.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let value = format!(
                    "{} {}",
                    amount.text(),
                    ["XMR", "USD", "EUR", "PLN"][currency.selected() as usize]
                );
                if mode.active_name().as_deref() == Some("request") {
                    ui.send_content(&value, "payment");
                    ui.show_chat()
                } else {
                    let d = adw::AlertDialog::builder()
                        .heading("Payment Review")
                        .body(format!(
                            "{value}\n{}\n\nOffline preview. No funds will be sent.",
                            note.text()
                        ))
                        .build();
                    d.add_response("close", "Close");
                    d.present(Some(&ui.window));
                }
            }
        });
        money.append(&review);
        self.media_stack.add_named(&scroll(&money), Some("monero"));
        let t = toggle.downgrade();
        let search_weak = search.downgrade();
        self.media_stack
            .connect_visible_child_name_notify(move |s| {
                if let Some(name) = s.visible_child_name() {
                    if let Some(t) = t.upgrade() {
                        t.set_active_name(Some(&name))
                    }
                    if let Some(search) = search_weak.upgrade() {
                        search.set_visible(name != "monero")
                    }
                }
            });
        search.connect_search_changed(move |s| {
            let q = s.text().to_lowercase();
            let mut index = 0;
            for (widget, title, gif) in entries.borrow().iter() {
                let visible = q.is_empty() || title.to_lowercase().contains(&q);
                widget.set_visible(visible);
                if *gif && visible {
                    gifs_grid.remove(widget);
                    gifs_grid.attach(widget, index % 2, index / 2, 1, 1);
                    index += 1
                }
            }
        });
        body.append(&self.media_stack);
        let footer = label("Local preview collection", "caption dim-label");
        footer.set_margin_bottom(12);
        body.append(&footer);
        self.media_pane.set_content(Some(&body));
        self.track_sidebar(&self.media_pane);
        self.utility.set_sidebar(Some(&self.media_pane));
        self.utility.connect_show_sidebar_notify(|u| {
            if let Some(sidebar) = u.sidebar() {
                if u.shows_sidebar() || !sidebar.is_mapped() {
                    sidebar.set_visible(u.shows_sidebar())
                }
            }
        });
    }
    pub fn toggle_media(&self) {
        if self.media_mode.borrow().as_str() != "media" {
            self.utility.set_sidebar(gtk::Widget::NONE);
            self.utility.set_sidebar(Some(&self.media_pane));
            self.media_pane.set_visible(true);
            self.media_mode.replace("media".into());
            self.utility.set_show_sidebar(true);
        } else {
            self.utility.set_show_sidebar(!self.utility.shows_sidebar())
        }
    }
    pub fn show_info(self: &Rc<Self>) {
        let chat = self.state.borrow().current().clone();
        let demo = self.state.borrow().account.is_empty();
        let pane = adw::ToolbarView::new();
        pane.add_css_class("utility-sidebar");
        let h = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .title_widget(&label("Conversation Info", "heading"))
            .build();
        let close = icon_button("sidebar-show-symbolic", "Close conversation info");
        let u = self.utility.downgrade();
        close.connect_clicked(move |_| {
            if let Some(u) = u.upgrade() {
                u.set_show_sidebar(false)
            }
        });
        h.pack_start(&close);
        pane.add_top_bar(&h);
        let body = bx(true, 16);
        margins(&body, 12);
        body.append(&self.chat_avatar(&chat, 76));
        body.append(&label(&chat.name, "title-2"));
        body.append(&label(&chat.subtitle, "dim-label"));
        let g = adw::PreferencesGroup::new();
        for (title, icon, key) in [
            ("Shared Media", "image-x-generic-symbolic", "media"),
            (
                "Conversation Security",
                "channel-secure-symbolic",
                "security",
            ),
            ("Control Access", "changes-prevent-symbolic", "access"),
        ] {
            if key == "access" && !demo {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    match key {
                        "media" => ui.shared_media(),
                        "security" => ui.security(),
                        _ => ui.preview_form("Control Access", &["Contact address"]),
                    }
                }
            });
            g.add(&row)
        }
        body.append(&g);
        if demo && chat.group {
            let members = adw::PreferencesGroup::builder().title("Members").build();
            for name in ["Alex · Owner", "Sam", "Mira", "Robin"] {
                members.add(&adw::ActionRow::builder().title(name).build())
            }
            body.append(&members)
        }
        pane.set_content(Some(&scroll(&body)));
        self.track_sidebar(&pane);
        self.utility.set_sidebar(gtk::Widget::NONE);
        self.utility.set_sidebar(Some(&pane));
        pane.set_visible(true);
        self.media_mode.replace("info".into());
        self.utility.set_show_sidebar(true);
    }
    pub fn shared_media(self: &Rc<Self>) {
        if !self.state.borrow().account.is_empty() {
            self.live_shared_media();
            return;
        }
        let pane = adw::ToolbarView::new();
        pane.add_css_class("utility-sidebar");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .title_widget(&label("Shared Media", "heading"))
            .build();
        let back = icon_button("go-previous-symbolic", "Conversation info");
        self.weak_button(&back, |ui| ui.show_info());
        header.pack_start(&back);
        let close = icon_button("sidebar-show-symbolic", "Close shared media");
        let utility = self.utility.downgrade();
        close.connect_clicked(move |_| {
            if let Some(u) = utility.upgrade() {
                u.set_show_sidebar(false);
            }
        });
        header.pack_end(&close);
        pane.add_top_bar(&header);
        let items = self.shared_media_items();
        if items.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("image-x-generic-symbolic")
                .title("No Shared Media")
                .description("Photos and GIFs from this conversation appear here.")
                .build();
            pane.set_content(Some(&empty));
        } else {
            let grid = gtk::Grid::builder()
                .column_homogeneous(true)
                .row_homogeneous(true)
                .column_spacing(4)
                .row_spacing(4)
                .valign(gtk::Align::Start)
                .build();
            grid.add_css_class("shared-media-grid");
            grid.set_widget_name("shared-media-grid");
            for (i, item) in items.iter().enumerate() {
                let image = gtk::Picture::for_filename(&item.path);
                image.set_can_shrink(true);
                image.set_content_fit(gtk::ContentFit::Cover);
                let square = gtk::AspectFrame::builder()
                    .ratio(1.)
                    .obey_child(false)
                    .child(&image)
                    .build();
                let tile = gtk::Button::builder()
                    .child(&square)
                    .tooltip_text(&item.title)
                    .hexpand(true)
                    .build();
                tile.set_widget_name(&item.id);
                tile.update_property(&[gtk::accessible::Property::Label(&item.title)]);
                tile.add_css_class("shared-media-tile");
                let weak = Rc::downgrade(self);
                tile.connect_clicked(move |tile| {
                    if let Some(ui) = weak.upgrade() {
                        ui.media_viewer(Some(tile.clone().upcast()));
                    }
                });
                grid.attach(&tile, i as i32 % 3, i as i32 / 3, 1, 1);
            }
            pane.set_content(Some(&scroll(&grid)));
        }
        self.track_sidebar(&pane);
        self.utility.set_sidebar(gtk::Widget::NONE);
        self.utility.set_sidebar(Some(&pane));
        pane.set_visible(true);
        self.media_mode.replace("shared-media".into());
        self.utility.set_show_sidebar(true);
    }
    fn shared_media_items(&self) -> Vec<MediaItem> {
        self.state
            .borrow()
            .current()
            .messages
            .iter()
            .filter_map(|m| {
                let path = match m.kind.as_str() {
                    "photo" => asset("landscape.svg"),
                    "gif" => asset(&format!("preview-{}.gif", m.gif_index % 6)),
                    _ => return None,
                };
                Some(MediaItem {
                    id: m.id.clone(),
                    title: m.text.clone(),
                    path,
                    message: None,
                })
            })
            .collect()
    }
    pub fn media_viewer(self: &Rc<Self>, source: Option<gtk::Widget>) {
        let old = self.active_media.borrow().clone();
        if let Some(old) = old {
            old.close()
        }
        let (items, initial) = if self.state.borrow().account.is_empty() {
            let items = self.shared_media_items();
            let index = source
                .as_ref()
                .and_then(|w| items.iter().position(|item| item.id == w.widget_name()))
                .unwrap_or(0);
            (items, index)
        } else {
            let s = self.state.borrow();
            let owner = s.account.clone();
            let peer = s.current().jid.clone();
            drop(s);
            let id = source.as_ref().map(|w| w.widget_name().to_string());
            let Some(window) = id.and_then(|id| self.store.media_window(&owner, &peer, &id).ok())
            else {
                self.notice("Shared media could not be loaded");
                return;
            };
            (
                window.messages.into_iter().map(MediaItem::live).collect(),
                window.index,
            )
        };
        if items.is_empty() {
            self.notice("No shared media in this conversation");
            return;
        }
        let viewer = MediaViewer::new(self, source, items, initial);
        self.active_media.replace(Some(viewer.clone()));
        self.overlay.add_overlay(&viewer.root);
        viewer.open();
    }
    pub fn refresh_stories(self: &Rc<Self>) {
        clear(&self.stories);
        let mut owners = vec!["You".to_string()];
        for story in &self.state.borrow().stories {
            if !owners.contains(&story.owner) {
                owners.push(story.owner.clone())
            }
        }
        for owner in owners {
            let stories: Vec<_> = self
                .state
                .borrow()
                .stories
                .iter()
                .filter(|s| s.owner == owner)
                .cloned()
                .collect();
            let b = gtk::Button::new();
            b.add_css_class("story-button");
            let who = Chat {
                name: owner.clone(),
                color: match owner.as_str() {
                    "Mira" => "pink",
                    "Sam" => "orange",
                    "Alex" => "purple",
                    _ => "blue",
                }
                .into(),
                ..Default::default()
            };
            let a = avatar(&who, 44);
            a.add_css_class(if stories.iter().any(|s| !s.seen) {
                "story-unread"
            } else {
                "story-seen"
            });
            let icon = gtk::Overlay::builder().child(&a).build();
            if owner == "You" {
                let add = gtk::Image::from_icon_name("list-add-symbolic");
                add.set_halign(gtk::Align::End);
                add.set_valign(gtk::Align::End);
                add.add_css_class("story-add");
                icon.add_overlay(&add);
                self.context_actions(&b, vec![("Add Story…".into(), "story-new".into())]);
            }
            b.set_child(Some(&icon));
            b.set_tooltip_text(Some(&if owner == "You" && stories.is_empty() {
                "Add Story".into()
            } else {
                format!("{owner}’s stories")
            }));
            let content = bx(true, 4);
            content.append(&b);
            content.append(&label(
                if owner == "You" && !stories.is_empty() {
                    "My Story"
                } else {
                    &owner
                },
                "caption",
            ));
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_story(&owner)
                }
            });
            self.stories.append(&content)
        }
    }
    pub fn show_stories(&self) {
        self.search_toggle.set_active(false);
        self.split.set_show_sidebar(true);
        let adjustment = self.sidebar_scroll.vadjustment();
        let target = adjustment.clone();
        let animation = adw::TimedAnimation::new(
            &self.sidebar_scroll,
            adjustment.value(),
            0.,
            180,
            adw::CallbackAnimationTarget::new(move |value| target.set_value(value)),
        );
        self.story_reveal.replace(Some(animation.clone()));
        animation.play();
    }
    pub fn open_story(self: &Rc<Self>, owner: &str) {
        let index = {
            let s = self.state.borrow();
            s.stories
                .iter()
                .position(|s| s.owner == owner && !s.seen)
                .or_else(|| s.stories.iter().position(|s| s.owner == owner))
        };
        let Some(index) = index else {
            self.new_story();
            return;
        };
        let viewer = StoryViewer::new(self, index);
        self.active_story.replace(Some(viewer.clone()));
        self.overlay.add_overlay(&viewer.root);
        viewer.open();
    }
    pub fn new_story(self: &Rc<Self>) {
        let chooser = gtk::FileDialog::builder()
            .title("Add Story")
            .modal(true)
            .build();
        let filter = gtk::FileFilter::new();
        filter.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        chooser.set_filters(Some(&filters));
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(file) = chooser.open_future(Some(&window)).await else {
                return;
            };
            let Some(ui) = weak.upgrade() else { return };
            if !ui.state.borrow().account.is_empty() {
                return;
            }
            let Some(path) = file.path() else {
                ui.notice("This preview needs a local image");
                return;
            };
            let page = adw::PreferencesPage::new();
            let g = adw::PreferencesGroup::builder()
                .title("Story")
                .description("Local preview. Nothing will be published.")
                .build();
            let photo = gtk::Picture::for_file(&file);
            photo.set_can_shrink(true);
            photo.set_content_fit(gtk::ContentFit::Contain);
            photo.set_size_request(280, 200);
            g.add(&photo);
            let caption = adw::EntryRow::builder().title("Caption").build();
            g.add(&caption);
            g.add(
                &adw::ComboRow::builder()
                    .title("Expires after")
                    .model(&gtk::StringList::new(&["24 hours"]))
                    .build(),
            );
            g.add(
                &adw::ComboRow::builder()
                    .title("Visible to")
                    .model(&gtk::StringList::new(&["Contacts", "Everyone"]))
                    .build(),
            );
            page.add(&g);
            let (d, h) = ui.dialog("New Story", &page, 480, 600);
            let post = gtk::Button::with_label("Post Preview");
            post.add_css_class("suggested-action");
            h.pack_end(&post);
            let weak = Rc::downgrade(&ui);
            post.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if !ui.state.borrow().account.is_empty() {
                        return;
                    }
                    ui.state.borrow_mut().stories.insert(
                        0,
                        Story {
                            owner: "You".into(),
                            asset: path.to_string_lossy().into(),
                            caption: caption.text().into(),
                            own: true,
                            ..Default::default()
                        },
                    );
                    ui.refresh_stories();
                    d.close();
                    ui.show_stories();
                }
            });
        });
    }
}
mod fly_imp {
    use super::*;
    #[derive(Default)]
    pub struct FlyFrame {
        pub progress: Cell<f64>,
        pub origin: RefCell<Option<graphene::Rect>>,
        pub overlay: glib::WeakRef<gtk::Overlay>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for FlyFrame {
        const NAME: &'static str = "MonogramFlyFrame";
        type Type = super::FlyFrame;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for FlyFrame {}
    impl WidgetImpl for FlyFrame {
        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let Some(picture) = obj.first_child() else {
                return;
            };
            let p = self.progress.get();
            s.save();
            if p < 1. {
                if let (Some(origin), Some(overlay)) =
                    (*self.origin.borrow(), self.overlay.upgrade())
                {
                    if let (Some(target), Some(pb)) =
                        (obj.compute_bounds(&overlay), picture.compute_bounds(&*obj))
                    {
                        let w = picture.width().max(1) as f64;
                        let h = picture.height().max(1) as f64;
                        let iw = w.min(h * 2.);
                        let ih = iw / 2.;
                        let ix = (w - iw) / 2.;
                        let iy = (h - ih) / 2.;
                        let sx = origin.width() as f64 / iw;
                        let sy = origin.height() as f64 / ih;
                        s.translate(&graphene::Point::new(
                            ((origin.x() as f64 - target.x() as f64 - (pb.x() as f64 + ix) * sx)
                                * (1. - p)) as f32,
                            ((origin.y() as f64 - target.y() as f64 - (pb.y() as f64 + iy) * sy)
                                * (1. - p)) as f32,
                        ));
                        s.scale((p + (1. - p) * sx) as f32, (p + (1. - p) * sy) as f32);
                    }
                }
            }
            obj.snapshot_child(&picture, s);
            s.restore();
        }
    }
    impl BoxImpl for FlyFrame {}
}
glib::wrapper! {pub struct FlyFrame(ObjectSubclass<fly_imp::FlyFrame>) @extends gtk::Box,gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget,gtk::Orientable;}
#[derive(Clone)]
pub struct MediaItem {
    id: String,
    title: String,
    path: std::path::PathBuf,
    message: Option<Message>,
}
impl MediaItem {
    fn live(message: Message) -> Self {
        Self {
            id: message.id.clone(),
            title: message
                .attachment
                .as_ref()
                .map(|f| f.name.clone())
                .unwrap_or_default(),
            path: Default::default(),
            message: Some(message),
        }
    }
}
pub struct MediaViewer {
    pub root: gtk::Overlay,
    ui: Weak<Ui>,
    stack: gtk::Stack,
    frames: RefCell<Vec<FlyFrame>>,
    pictures: RefCell<Vec<gtk::Picture>>,
    source: Option<glib::WeakRef<gtk::Widget>>,
    index: Cell<usize>,
    initial_id: String,
    items: RefCell<Vec<MediaItem>>,
    owner: String,
    peer: String,
    generation: u64,
    footer: gtk::Box,
    preview_status: gtk::Label,
    title: adw::WindowTitle,
    fullscreen: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    progress: Cell<f64>,
    closing: Cell<bool>,
    was_fullscreen: bool,
    animation: RefCell<Option<adw::TimedAnimation>>,
    fullscreen_signal: RefCell<Option<glib::SignalHandlerId>>,
}
impl MediaViewer {
    pub fn new(
        ui: &Rc<Ui>,
        source: Option<gtk::Widget>,
        items: Vec<MediaItem>,
        initial: usize,
    ) -> Rc<Self> {
        let root = gtk::Overlay::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        root.add_css_class("media-viewer");
        let stack = gtk::Stack::builder()
            .transition_duration(230)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.set_child(Some(&stack));
        let mut pictures = vec![];
        let mut frames = vec![];
        let origin = source.as_ref().and_then(|w| w.compute_bounds(&ui.overlay));
        for item in &items {
            let pic = if item.message.is_some() {
                gtk::Picture::new()
            } else {
                gtk::Picture::for_filename(&item.path)
            };
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_can_shrink(true);
            pic.set_hexpand(true);
            pic.set_vexpand(true);
            pic.set_margin_top(70);
            pic.set_margin_bottom(60);
            pic.set_margin_start(70);
            pic.set_margin_end(70);
            let frame: FlyFrame = glib::Object::new();
            frame.set_hexpand(true);
            frame.set_vexpand(true);
            frame.imp().origin.replace(origin);
            frame.imp().overlay.set(Some(&root));
            frame.append(&pic);
            stack.add_named(&frame, Some(&item.id));
            pictures.push(pic);
            frames.push(frame);
        }
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .valign(gtk::Align::Start)
            .build();
        let back = icon_button("go-previous-symbolic", "Back");
        header.pack_start(&back);
        let title = adw::WindowTitle::new("Riverside trail", "1 of 3");
        header.set_title_widget(Some(&title));
        let fullscreen = icon_button("view-fullscreen-symbolic", "Fullscreen");
        header.pack_end(&fullscreen);
        root.add_overlay(&header);
        let previous = icon_button("go-previous-symbolic", "Previous media");
        previous.add_css_class("circular");
        previous.set_halign(gtk::Align::Start);
        previous.set_margin_start(16);
        root.add_overlay(&previous);
        let next = icon_button("go-next-symbolic", "Next media");
        next.add_css_class("circular");
        next.set_halign(gtk::Align::End);
        next.set_margin_end(16);
        root.add_overlay(&next);
        let footer = bx(true, 4);
        footer.set_valign(gtk::Align::End);
        margins(&footer, 12);
        root.add_overlay(&footer);
        let preview_status = label("Loading preview…", "");
        preview_status.set_halign(gtk::Align::Center);
        preview_status.set_valign(gtk::Align::Center);
        preview_status.set_visible(false);
        root.add_overlay(&preview_status);
        let viewer = Rc::new(Self {
            root,
            ui: Rc::downgrade(ui),
            stack,
            frames: RefCell::new(frames),
            pictures: RefCell::new(pictures),
            source: source.map(|w| w.downgrade()),
            index: Cell::new(initial),
            initial_id: items[initial].id.clone(),
            owner: ui.state.borrow().account.clone(),
            peer: ui.state.borrow().current().jid.clone(),
            generation: ui.generation.get(),
            footer,
            preview_status,
            items: RefCell::new(items),
            title,
            fullscreen,
            previous,
            next,
            progress: Cell::new(0.),
            closing: Cell::new(false),
            was_fullscreen: ui.window.is_fullscreen(),
            animation: RefCell::new(None),
            fullscreen_signal: RefCell::new(None),
        });
        let weak = Rc::downgrade(&viewer);
        back.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.close()
            }
        });
        let weak = Rc::downgrade(&viewer);
        viewer.previous.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.move_by(-1)
            }
        });
        let weak = Rc::downgrade(&viewer);
        viewer.next.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.move_by(1)
            }
        });
        let window = ui.window.downgrade();
        viewer.fullscreen.connect_clicked(move |_| {
            if let Some(w) = window.upgrade() {
                if w.is_fullscreen() {
                    w.unfullscreen()
                } else {
                    w.fullscreen()
                }
            }
        });
        let weak = Rc::downgrade(&viewer);
        let signal = ui.window.connect_fullscreened_notify(move |_| {
            if let Some(v) = weak.upgrade() {
                v.fullscreen_changed()
            }
        });
        viewer.fullscreen_signal.replace(Some(signal));
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&viewer);
        key.connect_key_pressed(move |_, key, _, _| {
            if let Some(v) = weak.upgrade() {
                match key {
                    gdk::Key::Escape => v.close(),
                    gdk::Key::Left => v.move_by(-1),
                    gdk::Key::Right => v.move_by(1),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        viewer.root.add_controller(key);
        let swipe = gtk::GestureSwipe::new();
        let weak = Rc::downgrade(&viewer);
        swipe.connect_swipe(move |_, x, y| {
            if x.abs() > y.abs() {
                if let Some(v) = weak.upgrade() {
                    v.move_by(if x > 0. { -1 } else { 1 })
                }
            }
        });
        viewer.root.add_controller(swipe);
        if !viewer.owner.is_empty() {
            let pictures = viewer.pictures.borrow().clone();
            let items = viewer.items.borrow().clone();
            for (item, picture) in items.iter().zip(&pictures) {
                viewer.watch_image(item.id.clone(), picture);
            }
            if let Some(source) = viewer.source.as_ref().and_then(|s| s.upgrade()) {
                fn find(w: &gtk::Widget) -> Option<gdk::Paintable> {
                    if let Some(p) = w.downcast_ref::<gtk::Picture>() {
                        return p.paintable();
                    }
                    if let Some(p) = w.downcast_ref::<AspectImage>() {
                        return p.paintable();
                    }
                    let mut c = w.first_child();
                    while let Some(w) = c {
                        c = w.next_sibling();
                        if let Some(p) = find(&w) {
                            return Some(p);
                        }
                    }
                    None
                }
                if let Some(p) = find(&source) {
                    pictures[initial].set_paintable(Some(&p));
                }
            }
        }
        viewer.move_by(0);
        viewer.fullscreen_changed();
        viewer
    }
    fn animate(&self, p: f64) {
        self.progress.set(p);
        self.root.set_opacity(p);
        for f in self.frames.borrow().iter() {
            f.imp().progress.set(p);
            f.queue_draw()
        }
    }
    pub fn open(self: &Rc<Self>) {
        self.root.set_opacity(0.);
        self.root.grab_focus();
        let weak = Rc::downgrade(self);
        let a = adw::TimedAnimation::new(
            &self.root,
            0.,
            1.,
            280,
            adw::CallbackAnimationTarget::new(move |p| {
                if let Some(v) = weak.upgrade() {
                    v.animate(p)
                }
            }),
        );
        a.play();
        self.animation.replace(Some(a));
    }
    fn move_by(self: &Rc<Self>, delta: i32) {
        let id = {
            let items = self.items.borrow();
            let index = (self.index.get() as i32 + delta).clamp(0, items.len() as i32 - 1) as usize;
            items[index].id.clone()
        };
        if !self.owner.is_empty() {
            let Some(ui) = self.ui.upgrade() else {
                return;
            };
            let Ok(window) = ui.store.media_window(&self.owner, &self.peer, &id) else {
                ui.notice("Shared media could not be loaded");
                return;
            };
            if window.messages.is_empty() {
                self.close();
                return;
            }
            self.index.set(window.index);
            let items: Vec<_> = window.messages.into_iter().map(MediaItem::live).collect();
            let mut frames = vec![];
            let mut pictures = vec![];
            for item in &items {
                if let Some(frame) = self
                    .stack
                    .child_by_name(&item.id)
                    .and_downcast::<FlyFrame>()
                {
                    let pic = frame.first_child().and_downcast::<gtk::Picture>().unwrap();
                    frames.push(frame);
                    pictures.push(pic);
                } else {
                    let pic = gtk::Picture::builder()
                        .can_shrink(true)
                        .content_fit(gtk::ContentFit::Contain)
                        .hexpand(true)
                        .vexpand(true)
                        .build();
                    let frame: FlyFrame = glib::Object::new();
                    frame.set_hexpand(true);
                    frame.set_vexpand(true);
                    frame.imp().overlay.set(Some(&self.root));
                    frame.imp().progress.set(self.progress.get());
                    frame.append(&pic);
                    self.stack.add_named(&frame, Some(&item.id));
                    self.watch_image(item.id.clone(), &pic);
                    frames.push(frame);
                    pictures.push(pic);
                }
            }
            let old = self.frames.replace(frames);
            self.pictures.replace(pictures);
            self.items.replace(items);
            for frame in old {
                if !self.frames.borrow().iter().any(|f| f == &frame) {
                    self.stack.remove(&frame);
                }
            }
            clear(&self.footer);
            let message = self.items.borrow()[self.index.get()]
                .message
                .clone()
                .unwrap();
            let file = message.attachment.as_ref().unwrap();
            let state = ui.downloads.borrow().status(&self.owner, file);
            if !matches!(
                state,
                Some(crate::downloads::Status::Ready(_) | crate::downloads::Status::Running { .. })
            ) {
                if let Err(reason) = ui.downloads.borrow_mut().start(&self.owner, file.clone()) {
                    ui.notice(reason);
                }
            }
            self.footer.append(&ui.file_controls(&message, false));
            self.fullscreen_changed();
        } else {
            let i = (self.index.get() as i32 + delta).clamp(0, self.items.borrow().len() as i32 - 1)
                as usize;
            self.index.set(i);
        }
        let i = self.index.get();
        let items = self.items.borrow();
        self.stack.set_transition_type(if delta > 0 {
            gtk::StackTransitionType::SlideLeft
        } else {
            gtk::StackTransitionType::SlideRight
        });
        self.stack.set_visible_child_name(&items[i].id);
        self.title.set_title(&items[i].title);
        self.title.set_subtitle(
            &items[i]
                .message
                .as_ref()
                .map(|m| m.text.clone())
                .unwrap_or_else(|| format!("{} of {}", i + 1, items.len())),
        );
        self.previous.set_visible(items.len() > 1);
        self.next.set_visible(items.len() > 1);
        self.previous.set_sensitive(i > 0);
        self.next.set_sensitive(i + 1 < items.len());
    }
    fn watch_image(self: &Rc<Self>, id: String, picture: &gtk::Picture) {
        let weak = Rc::downgrade(self);
        let p = picture.downgrade();
        let loaded_edge = Cell::new(0u32);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(viewer), Some(picture)) = (weak.upgrade(), p.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            if viewer.closing.get() {
                return glib::ControlFlow::Break;
            }
            let Some(ui) = viewer.ui.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.generation.get() != viewer.generation
                || ui.state.borrow().account != viewer.owner
                || ui.state.borrow().current().jid != viewer.peer
            {
                viewer.close();
                return glib::ControlFlow::Break;
            }
            let Some(index) = viewer.items.borrow().iter().position(|item| item.id == id) else {
                return glib::ControlFlow::Break;
            };
            let current = index == viewer.index.get();
            let edge = if current { 4096 } else { 384 };
            if !current
                && viewer.pictures.borrow()[viewer.index.get()]
                    .paintable()
                    .is_none()
            {
                return glib::ControlFlow::Continue;
            }
            let Some(message) = ui
                .store
                .message(&viewer.owner, &viewer.peer, &id)
                .filter(|m| !m.retracted)
            else {
                if index == viewer.index.get() {
                    viewer.close();
                }
                return glib::ControlFlow::Break;
            };
            let Some(file) = message.attachment else {
                viewer.close();
                return glib::ControlFlow::Break;
            };
            if loaded_edge.get() == edge {
                if index == viewer.index.get() {
                    viewer.preview_status.set_visible(false);
                }
                return glib::ControlFlow::Continue;
            }
            if index == viewer.index.get() {
                viewer.preview_status.set_text("Loading preview…");
                viewer
                    .preview_status
                    .set_visible(picture.paintable().is_none());
            }
            if let Some(path) = ui.cached_image_source(&file) {
                match ui.image(&file, &path, edge) {
                    Some(Ok(image)) => {
                        if ui
                            .store
                            .remember_image(
                                &viewer.owner,
                                &viewer.peer,
                                &id,
                                &file,
                                &image.mime,
                                image.width,
                                image.height,
                                image.animated,
                            )
                            .unwrap_or(false)
                        {
                            picture.set_paintable(Some(&image.texture));
                            loaded_edge.set(edge);
                            if index == viewer.index.get() {
                                viewer.preview_status.set_visible(false);
                            }
                        }
                    }
                    Some(Err(_)) if index == viewer.index.get() => {
                        viewer
                            .preview_status
                            .set_text("Preview unavailable. You can save this file.");
                        viewer
                            .preview_status
                            .set_visible(picture.paintable().is_none());
                    }
                    _ => {}
                }
            }
            glib::ControlFlow::Continue
        });
    }
    fn fullscreen_changed(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let full = ui.window.is_fullscreen();
        for pic in self.pictures.borrow().iter() {
            pic.set_margin_top(if full { 0 } else { 70 });
            pic.set_margin_bottom(if full { 0 } else { 60 });
            pic.set_margin_start(if full { 0 } else { 70 });
            pic.set_margin_end(if full { 0 } else { 70 })
        }
        self.fullscreen.set_icon_name(if full {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
    }
    pub fn close_immediate(self: &Rc<Self>) {
        self.closing.set(true);
        if let Some(animation) = self.animation.borrow_mut().take() {
            animation.pause();
        }
        if let Some(ui) = self.ui.upgrade() {
            if let Some(signal) = self.fullscreen_signal.borrow_mut().take() {
                ui.window.disconnect(signal);
            }
            if !self.was_fullscreen && ui.window.is_fullscreen() {
                ui.window.unfullscreen();
            }
            if self.root.parent().is_some() {
                ui.overlay.remove_overlay(&self.root);
            }
            if ui
                .active_media
                .borrow()
                .as_ref()
                .is_some_and(|active| Rc::ptr_eq(active, self))
            {
                ui.active_media.borrow_mut().take();
            }
        }
    }
    pub fn close(self: &Rc<Self>) {
        if self.closing.replace(true) {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        if let Some(a) = self.animation.borrow().as_ref() {
            a.pause()
        }
        let origin = if self.items.borrow()[self.index.get()].id == self.initial_id {
            self.source
                .as_ref()
                .and_then(|s| s.upgrade())
                .and_then(|s| s.compute_bounds(&ui.overlay))
        } else {
            None
        };
        for f in self.frames.borrow().iter() {
            f.imp().origin.replace(origin);
        }
        if let Some(signal) = self.fullscreen_signal.borrow_mut().take() {
            ui.window.disconnect(signal)
        }
        if !self.was_fullscreen && ui.window.is_fullscreen() {
            ui.window.unfullscreen()
        }
        let weak = Rc::downgrade(self);
        let a = adw::TimedAnimation::new(
            &self.root,
            self.progress.get(),
            0.,
            240,
            adw::CallbackAnimationTarget::new(move |p| {
                if let Some(v) = weak.upgrade() {
                    v.animate(p)
                }
            }),
        );
        let v = self.clone();
        a.connect_done(move |_| {
            if let Some(ui) = v.ui.upgrade() {
                ui.overlay.remove_overlay(&v.root);
                if ui
                    .active_media
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &v))
                {
                    ui.active_media.borrow_mut().take();
                }
            }
            v.animation.borrow_mut().take();
        });
        self.animation.replace(Some(a.clone()));
        a.play();
    }
}
pub struct StoryViewer {
    pub root: gtk::Overlay,
    ui: Weak<Ui>,
    index: Cell<usize>,
    picture: gtk::Picture,
    caption: gtk::Label,
    owner: gtk::Label,
    avatar: adw::Avatar,
    segments: gtk::Box,
    bars: RefCell<Vec<(usize, gtk::ProgressBar)>>,
    pub pause: gtk::ToggleButton,
    reply: gtk::Entry,
    like: gtk::ToggleButton,
    elapsed: Cell<f64>,
    last: Cell<Instant>,
    held: Cell<bool>,
    pressed: Cell<Instant>,
    closing: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    animation: RefCell<Option<adw::TimedAnimation>>,
}
impl StoryViewer {
    pub fn new(ui: &Rc<Ui>, index: usize) -> Rc<Self> {
        let root = gtk::Overlay::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        root.add_css_class("story-viewer");
        let stage = gtk::Overlay::new();
        stage.add_css_class("story-stage");
        let frame = gtk::AspectFrame::builder()
            .ratio(9. / 16.)
            .obey_child(false)
            .child(&stage)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.set_child(Some(&frame));
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Cover)
            .hexpand(true)
            .vexpand(true)
            .build();
        stage.set_child(Some(&picture));
        let top = bx(true, 10);
        top.set_valign(gtk::Align::Start);
        top.add_css_class("story-top");
        let segments = bx(false, 4);
        segments.set_homogeneous(true);
        top.append(&segments);
        let controls = bx(false, 8);
        let avatar = adw::Avatar::new(32, None, true);
        controls.append(&avatar);
        let names = bx(true, 1);
        names.set_hexpand(true);
        let owner = text("", "heading");
        names.append(&owner);
        names.append(&text("Today", "caption"));
        controls.append(&names);
        let pause = gtk::ToggleButton::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Pause story")
            .build();
        pause.add_css_class("flat");
        controls.append(&pause);
        let close = icon_button("window-close-symbolic", "Close story");
        controls.append(&close);
        top.append(&controls);
        stage.add_overlay(&top);
        let bottom = bx(true, 12);
        bottom.set_valign(gtk::Align::End);
        bottom.add_css_class("story-bottom");
        let caption = wrap("", "");
        bottom.append(&caption);
        let actions = bx(false, 8);
        let reply = gtk::Entry::builder()
            .placeholder_text("Reply privately…")
            .hexpand(true)
            .build();
        actions.append(&reply);
        let like = gtk::ToggleButton::with_label("♡");
        like.add_css_class("flat");
        actions.append(&like);
        let send = icon_button("mail-send-symbolic", "Send story reply");
        send.set_child(Some(&gtk::Image::from_file(asset(
            "paper-plane-symbolic.svg",
        ))));
        actions.append(&send);
        bottom.append(&actions);
        stage.add_overlay(&bottom);
        let previous = icon_button("go-previous-symbolic", "Previous story");
        previous.add_css_class("circular");
        previous.set_halign(gtk::Align::Start);
        previous.set_margin_start(16);
        root.add_overlay(&previous);
        let next = icon_button("go-next-symbolic", "Next story");
        next.add_css_class("circular");
        next.set_halign(gtk::Align::End);
        next.set_margin_end(16);
        root.add_overlay(&next);
        let v = Rc::new(Self {
            root,
            ui: Rc::downgrade(ui),
            index: Cell::new(index),
            picture,
            caption,
            owner,
            avatar,
            segments,
            bars: RefCell::new(vec![]),
            pause,
            reply,
            like,
            elapsed: Cell::new(0.),
            last: Cell::new(Instant::now()),
            held: Cell::new(false),
            pressed: Cell::new(Instant::now()),
            closing: Cell::new(false),
            timer: RefCell::new(None),
            animation: RefCell::new(None),
        });
        let weak = Rc::downgrade(&v);
        close.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.close()
            }
        });
        let weak = Rc::downgrade(&v);
        previous.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.move_by(-1)
            }
        });
        let weak = Rc::downgrade(&v);
        next.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.move_by(1)
            }
        });
        let weak = Rc::downgrade(&v);
        v.like.connect_toggled(move |b| {
            b.set_label(if b.is_active() { "♥" } else { "♡" });
            if let Some(v) = weak.upgrade() {
                if let Some(ui) = v.ui.upgrade() {
                    if let Some(story) = ui.state.borrow_mut().stories.get_mut(v.index.get()) {
                        story.liked = b.is_active();
                    }
                }
            }
        });
        let weak = Rc::downgrade(&v);
        send.connect_clicked(move |_| {
            if let Some(v) = weak.upgrade() {
                v.send_reply()
            }
        });
        let weak = Rc::downgrade(&v);
        v.reply.connect_activate(move |_| {
            if let Some(v) = weak.upgrade() {
                v.send_reply()
            }
        });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&v);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(v) = weak.upgrade() {
                v.held.set(true);
                v.pressed.set(Instant::now())
            }
        });
        let weak = Rc::downgrade(&v);
        click.connect_released(move |_, _, x, _| {
            if let Some(v) = weak.upgrade() {
                v.held.set(false);
                if v.pressed.get().elapsed() < Duration::from_millis(250) {
                    v.move_by(if x < v.picture.width() as f64 / 3. {
                        -1
                    } else {
                        1
                    })
                }
            }
        });
        v.picture.add_controller(click);
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&v);
        key.connect_key_pressed(move |_, k, _, _| {
            if let Some(v) = weak.upgrade() {
                if k == gdk::Key::Escape {
                    v.close();
                    return glib::Propagation::Stop;
                }
                if v.reply
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
                {
                    return glib::Propagation::Proceed;
                }
                match k {
                    gdk::Key::Left => v.move_by(-1),
                    gdk::Key::Right => v.move_by(1),
                    gdk::Key::space => v.pause.set_active(!v.pause.is_active()),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        v.root.add_controller(key);
        v.load();
        v
    }
    fn load(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let story = {
            let mut s = ui.state.borrow_mut();
            let Some(story) = s.stories.get_mut(self.index.get()) else {
                return;
            };
            story.seen = true;
            story.clone()
        };
        self.elapsed.set(0.);
        self.owner.set_text(&story.owner);
        self.avatar.set_text(Some(&story.owner));
        self.caption.set_text(&story.caption);
        self.like.set_active(story.liked);
        self.reply.set_text("");
        self.reply.set_visible(!story.own);
        let path = std::path::PathBuf::from(&story.asset);
        self.picture.set_filename(Some(if path.is_absolute() {
            path
        } else {
            asset(&story.asset)
        }));
        clear(&self.segments);
        self.bars.borrow_mut().clear();
        for (i, other) in ui.state.borrow().stories.iter().enumerate() {
            if other.owner == story.owner {
                let p = gtk::ProgressBar::new();
                p.set_hexpand(true);
                self.segments.append(&p);
                self.bars.borrow_mut().push((i, p));
            }
        }
        self.progress();
        ui.refresh_stories();
    }
    fn progress(&self) {
        for (i, b) in self.bars.borrow().iter() {
            b.set_fraction(if *i < self.index.get() {
                1.
            } else if *i == self.index.get() {
                (self.elapsed.get() / 5.).min(1.)
            } else {
                0.
            })
        }
    }
    pub fn open(self: &Rc<Self>) {
        self.root.set_opacity(0.);
        self.root.grab_focus();
        let weak = Rc::downgrade(self);
        let a = adw::TimedAnimation::new(
            &self.root,
            0.,
            1.,
            200,
            adw::CallbackAnimationTarget::new(move |p| {
                if let Some(v) = weak.upgrade() {
                    v.root.set_opacity(p)
                }
            }),
        );
        a.play();
        self.animation.replace(Some(a));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(v) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if v.closing.get() {
                return glib::ControlFlow::Break;
            }
            let delta = v
                .last
                .replace(Instant::now())
                .elapsed()
                .as_secs_f64()
                .min(0.1);
            let active = v.ui.upgrade().is_some_and(|u| u.window.is_active());
            if active
                && !v.held.get()
                && !v.pause.is_active()
                && !v
                    .reply
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
            {
                v.elapsed.set(v.elapsed.get() + delta);
                v.progress();
                if v.elapsed.get() >= 5. {
                    v.move_by(1)
                }
            }
            glib::ControlFlow::Continue
        });
        self.timer.replace(Some(id));
    }
    pub fn move_by(self: &Rc<Self>, delta: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let index = (self.index.get() as i32 + delta).max(0) as usize;
        if index >= ui.state.borrow().stories.len() {
            self.close();
            return;
        }
        self.index.set(index);
        self.load();
    }
    fn send_reply(self: &Rc<Self>) {
        let value = self.reply.text();
        if value.trim().is_empty() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(story) = ui.state.borrow().stories.get(self.index.get()).cloned() else {
            return;
        };
        let id = {
            let mut s = ui.state.borrow_mut();
            if let Some(c) = s.chats.iter().find(|c| c.name == story.owner) {
                c.id.clone()
            } else {
                let id = new_id();
                s.chats.push(Chat {
                    id: id.clone(),
                    name: story.owner,
                    subtitle: "Last seen recently".into(),
                    color: "blue".into(),
                    notifications: "all".into(),
                    ..Default::default()
                });
                id
            }
        };
        ui.select_chat(&id);
        ui.send_content(value.as_str(), "text");
        self.close();
    }
    pub fn close(self: &Rc<Self>) {
        if self.closing.replace(true) {
            return;
        }
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove()
        }
        if let Some(a) = self.animation.borrow().as_ref() {
            a.pause()
        }
        let weak = Rc::downgrade(self);
        let a = adw::TimedAnimation::new(
            &self.root,
            self.root.opacity(),
            0.,
            180,
            adw::CallbackAnimationTarget::new(move |p| {
                if let Some(v) = weak.upgrade() {
                    v.root.set_opacity(p)
                }
            }),
        );
        let v = self.clone();
        a.connect_done(move |_| {
            if let Some(ui) = v.ui.upgrade() {
                ui.overlay.remove_overlay(&v.root);
                if ui
                    .active_story
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| Rc::ptr_eq(active, &v))
                {
                    ui.active_story.borrow_mut().take();
                }
            }
            v.animation.borrow_mut().take();
        });
        self.animation.replace(Some(a.clone()));
        a.play();
    }
}
