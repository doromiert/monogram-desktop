use crate::{dialogs::bind_amount, model::*, ui::Ui, wallet::WalletCard, widgets::*};
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
#[path = "media/zoom.rs"]
mod zoom;
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
        let live_access = !demo
            && !chat.group
            && chat.jid != self.state.borrow().account
            && crate::protocol::bare(&chat.jid).ok().as_deref() == Some(chat.jid.as_str());
        for (title, icon, key) in [
            ("Shared Media", "image-x-generic-symbolic", "media"),
            (
                "Conversation Security",
                "channel-secure-symbolic",
                "security",
            ),
            ("Control Access", "changes-prevent-symbolic", "access"),
        ] {
            if key == "access" && !demo && !live_access {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            let peer = chat.jid.clone();
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    match key {
                        "media" => ui.shared_media(),
                        "security" => ui.security(),
                        _ if ui.state.borrow().account.is_empty() => {
                            ui.preview_form("Control Access", &["Contact address"])
                        }
                        _ => ui.contact_access(&peer),
                    }
                }
            });
            g.add(&row)
        }
        body.append(&g);
        if !demo && chat.group {
            let saved = adw::ActionRow::builder()
                .title("Saved Room")
                .activatable(true)
                .build();
            saved.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            saved.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.room_preferences();
                }
            });
            body.append(&saved);
            let leave = gtk::Button::with_label("Leave Group");
            leave.add_css_class("destructive-action");
            leave.set_sensitive(chat.room_joined);
            self.weak_button(&leave, |ui| ui.leave_current_room());
            body.append(&leave);
        }
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
            let selected = source
                .as_ref()
                .and_then(|widget| self.store.message(&owner, &peer, &widget.widget_name()))
                .filter(|message| !message.retracted && message.attachment.is_some());
            (selected.into_iter().map(MediaItem::live).collect(), 0)
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
        pub zoom: Cell<f64>,
        pub pan: Cell<(f64, f64)>,
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
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().clamp_pan();
        }
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
                    if let (Some(frame), Some(pb)) =
                        (obj.compute_bounds(&overlay), picture.compute_bounds(&*obj))
                    {
                        let widget_w = pb.width().max(1.) as f64;
                        let widget_h = pb.height().max(1.) as f64;
                        let ratio = picture
                            .downcast_ref::<gtk::Picture>()
                            .and_then(|picture| picture.paintable())
                            .map(|paintable| paintable.intrinsic_aspect_ratio())
                            .filter(|ratio| *ratio > 0.)
                            .unwrap_or(widget_w / widget_h);
                        let target_w = widget_w.min(widget_h * ratio).max(1.);
                        let target_h = widget_h.min(widget_w / ratio).max(1.);
                        let target_x = pb.x() as f64 + (widget_w - target_w) / 2.;
                        let target_y = pb.y() as f64 + (widget_h - target_h) / 2.;
                        let start_x = origin.x() as f64 + origin.width() as f64 / 2.;
                        let start_y = origin.y() as f64 + origin.height() as f64 / 2.;
                        let end_x = frame.x() as f64 + target_x + target_w / 2.;
                        let end_y = frame.y() as f64 + target_y + target_h / 2.;
                        let dx = end_x - start_x;
                        let dy = end_y - start_y;
                        let control_x = (start_x + end_x) / 2. + dy * 0.22;
                        let control_y = (start_y + end_y) / 2. - (dx.abs() * 0.28).max(48.);
                        let one_minus = 1. - p;
                        let current_x = one_minus * one_minus * start_x
                            + 2. * one_minus * p * control_x
                            + p * p * end_x;
                        let current_y = one_minus * one_minus * start_y
                            + 2. * one_minus * p * control_y
                            + p * p * end_y;
                        let current_w =
                            origin.width() as f64 + (target_w - origin.width() as f64) * p;
                        let current_h =
                            origin.height() as f64 + (target_h - origin.height() as f64) * p;
                        s.translate(&graphene::Point::new(
                            (current_x - frame.x() as f64) as f32,
                            (current_y - frame.y() as f64) as f32,
                        ));
                        s.scale((current_w / target_w) as f32, (current_h / target_h) as f32);
                        s.translate(&graphene::Point::new(
                            -(target_x + target_w / 2.) as f32,
                            -(target_y + target_h / 2.) as f32,
                        ));
                    }
                }
            } else if self.zoom.get() > 1. {
                if let Some(bounds) = obj.media_bounds() {
                    let (x, y) = self.pan.get();
                    let cx = bounds.x() + bounds.width() / 2.;
                    let cy = bounds.y() + bounds.height() / 2.;
                    s.translate(&graphene::Point::new(cx + x as f32, cy + y as f32));
                    s.scale(self.zoom.get() as f32, self.zoom.get() as f32);
                    s.translate(&graphene::Point::new(-cx, -cy));
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
                .map(|f| f.image_label())
                .unwrap_or("Image")
                .into(),
            path: Default::default(),
            message: Some(message),
        }
    }
}
pub struct MediaViewer {
    pub root: gtk::Overlay,
    backdrop: gtk::Box,
    ui: Weak<Ui>,
    stack: gtk::Stack,
    frames: RefCell<Vec<FlyFrame>>,
    pictures: RefCell<Vec<gtk::Picture>>,
    source: Option<glib::WeakRef<gtk::Widget>>,
    index: Cell<usize>,
    pub(crate) neighbours_pending: Cell<bool>,
    initial_id: String,
    items: RefCell<Vec<MediaItem>>,
    owner: String,
    peer: String,
    generation: u64,
    preview_status: gtk::Label,
    title: adw::WindowTitle,
    fullscreen: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    video_controls: gtk::Revealer,
    controls_mount: gtk::Box,
    replay_mount: gtk::Box,
    active_video: RefCell<Option<Rc<crate::video_ui::Playback>>>,
    playbacks: RefCell<HashMap<String, Rc<crate::video_ui::Playback>>>,
    progress: Cell<f64>,
    closing: Cell<bool>,
    was_fullscreen: bool,
    animation: RefCell<Option<adw::TimedAnimation>>,
    fullscreen_signal: RefCell<Option<glib::SignalHandlerId>>,
    controls: Vec<gtk::Revealer>,
    controls_timer: RefCell<Option<glib::SourceId>>,
    zoom_reset: gtk::Button,
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
        let backdrop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        backdrop.add_css_class("media-viewer-backdrop");
        root.set_child(Some(&backdrop));
        let stack = gtk::Stack::builder()
            .transition_duration(230)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.add_overlay(&stack);
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
            pic.set_margin_top(72);
            pic.set_margin_bottom(20);
            pic.set_margin_start(20);
            pic.set_margin_end(20);
            let frame: FlyFrame = glib::Object::new();
            frame.set_hexpand(true);
            frame.set_vexpand(true);
            frame.imp().origin.replace(origin);
            frame.imp().overlay.set(Some(&root));
            frame.imp().zoom.set(1.);
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
        let top = bx(true, 0);
        top.add_css_class("media-controls-top");
        top.append(&header);
        let reveal = |child: &gtk::Widget, horizontal, vertical| {
            gtk::Revealer::builder()
                .child(child)
                .reveal_child(true)
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .transition_duration(180)
                .halign(horizontal)
                .valign(vertical)
                .build()
        };
        let top_revealer = reveal(top.upcast_ref(), gtk::Align::Fill, gtk::Align::Start);
        root.add_overlay(&top_revealer);
        let previous = icon_button("go-previous-symbolic", "Previous media");
        previous.add_css_class("circular");
        previous.add_css_class("media-viewer-arrow");
        previous.set_halign(gtk::Align::Start);
        let previous_revealer =
            reveal(previous.upcast_ref(), gtk::Align::Start, gtk::Align::Center);
        root.add_overlay(&previous_revealer);
        let next = icon_button("go-next-symbolic", "Next media");
        next.add_css_class("circular");
        next.add_css_class("media-viewer-arrow");
        next.set_halign(gtk::Align::End);
        let next_revealer = reveal(next.upcast_ref(), gtk::Align::End, gtk::Align::Center);
        root.add_overlay(&next_revealer);
        // Galeria keeps Clapper's real SimpleControls inside its owning Video widget.
        let controls_mount = bx(true, 0);
        controls_mount.add_css_class("galeria-video-controls");
        controls_mount.add_css_class("galeria-video");
        let video_controls = reveal(
            controls_mount.upcast_ref(),
            gtk::Align::Fill,
            gtk::Align::End,
        );
        video_controls.set_reveal_child(false);
        root.add_overlay(&video_controls);
        let replay_mount = bx(true, 0);
        replay_mount.set_halign(gtk::Align::Center);
        replay_mount.set_valign(gtk::Align::Center);
        root.add_overlay(&replay_mount);
        let preview_status = label("Loading preview…", "");
        preview_status.set_halign(gtk::Align::Center);
        preview_status.set_valign(gtk::Align::Center);
        preview_status.set_visible(false);
        root.add_overlay(&preview_status);
        let zoom_reset = gtk::Button::builder()
            .label("100%")
            .tooltip_text("Reset zoom (0)")
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(56)
            .visible(false)
            .build();
        zoom_reset.add_css_class("osd");
        zoom_reset.add_css_class("pill");
        let zoom_revealer = reveal(
            zoom_reset.upcast_ref(),
            gtk::Align::Center,
            gtk::Align::Start,
        );
        root.add_overlay(&zoom_revealer);
        let viewer = Rc::new(Self {
            root,
            backdrop,
            ui: Rc::downgrade(ui),
            stack,
            frames: RefCell::new(frames),
            pictures: RefCell::new(pictures),
            source: source.map(|w| w.downgrade()),
            index: Cell::new(initial),
            neighbours_pending: Cell::new(false),
            initial_id: items[initial].id.clone(),
            owner: ui.state.borrow().account.clone(),
            peer: ui.state.borrow().current().jid.clone(),
            generation: ui.generation.get(),
            preview_status,
            items: RefCell::new(items),
            title,
            fullscreen,
            previous,
            next,
            video_controls,
            controls_mount,
            replay_mount,
            active_video: RefCell::new(None),
            playbacks: RefCell::new(HashMap::new()),
            progress: Cell::new(0.),
            closing: Cell::new(false),
            was_fullscreen: ui.window.is_fullscreen(),
            animation: RefCell::new(None),
            fullscreen_signal: RefCell::new(None),
            controls: vec![
                top_revealer,
                previous_revealer,
                next_revealer,
                zoom_revealer,
            ],
            controls_timer: RefCell::new(None),
            zoom_reset,
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
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&viewer);
        motion.connect_motion(move |_, _, _| {
            if let Some(viewer) = weak.upgrade() {
                viewer.reveal_controls();
            }
        });
        viewer.root.add_controller(motion);
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&viewer);
        key.connect_key_pressed(move |_, key, _, _| {
            if let Some(v) = weak.upgrade() {
                v.reveal_controls();
                match key {
                    gdk::Key::Escape => v.close(),
                    gdk::Key::Left => v.move_by(-1),
                    gdk::Key::Right => v.move_by(1),
                    gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => v.zoom_by(1.25),
                    gdk::Key::minus | gdk::Key::KP_Subtract => v.zoom_by(0.8),
                    gdk::Key::_0 | gdk::Key::KP_0 => v.reset_zoom(),
                    gdk::Key::space if v.active_video.borrow().is_some() => {
                        let video = v.active_video.borrow();
                        let media = &video.as_ref().unwrap().media;
                        media.set_playing(!media.is_playing());
                    }
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        viewer.root.add_controller(key);
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(&viewer);
        click.connect_released(move |_, presses, x, y| {
            let Some(viewer) = weak
                .upgrade()
                .filter(|v| presses == 1 && v.progress.get() >= 0.99)
            else {
                return;
            };
            let mut target = viewer.root.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(widget) = target {
                if widget.is::<gtk::Button>()
                    || widget.has_css_class("galeria-video-controls")
                    || widget.is::<adw::HeaderBar>()
                {
                    return;
                }
                target = widget.parent();
            }
            let inside = viewer.media_contains(x, y);
            if !inside {
                viewer.close();
            } else {
                viewer.reveal_controls();
            }
        });
        viewer.root.add_controller(click);
        let swipe = gtk::GestureSwipe::new();
        let weak = Rc::downgrade(&viewer);
        swipe.connect_swipe(move |_, x, y| {
            if x.abs() > y.abs() {
                if let Some(v) = weak.upgrade().filter(|v| !v.is_zoomed()) {
                    v.move_by(if x > 0. { -1 } else { 1 })
                }
            }
        });
        viewer.root.add_controller(swipe);
        viewer.install_zoom();
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
        let p = p * p * (3. - 2. * p);
        self.progress.set(p);
        self.backdrop.set_opacity(p);
        for control in &self.controls {
            control.set_opacity(p);
        }
        self.preview_status.set_opacity(p);
        self.replay_mount.set_opacity(p);
        self.video_controls.set_opacity(p);
        for f in self.frames.borrow().iter() {
            f.imp().progress.set(p);
            f.queue_draw()
        }
    }
    pub fn open(self: &Rc<Self>) {
        if let Some(source) = self.source.as_ref().and_then(|source| source.upgrade()) {
            source.set_opacity(0.);
        }
        self.backdrop.set_opacity(0.);
        for control in &self.controls {
            control.set_opacity(0.);
        }
        self.preview_status.set_opacity(0.);
        self.replay_mount.set_opacity(0.);
        self.video_controls.set_opacity(0.);
        self.root.grab_focus();
        let weak = Rc::downgrade(self);
        let a = adw::TimedAnimation::new(
            &self.root,
            0.,
            1.,
            360,
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
        if self.closing.get() {
            return;
        }
        let previous = self.index.get();
        let index =
            (previous as i32 + delta).clamp(0, self.items.borrow().len() as i32 - 1) as usize;
        if index == previous && delta != 0 {
            return;
        }
        self.reset_zoom();
        self.set_active_video(None);
        self.index.set(index);
        self.update_position(delta);
        if !self.owner.is_empty() {
            let Some(ui) = self.ui.upgrade() else { return };
            let id = self.items.borrow()[index].id.clone();
            let Some(message) = ui
                .store
                .message(&self.owner, &self.peer, &id)
                .filter(|m| !m.retracted && m.attachment.is_some())
            else {
                self.close();
                return;
            };
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
            self.refresh_neighbours();
        }
    }
    fn refresh_neighbours(self: &Rc<Self>) {
        if self.neighbours_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = self.items.borrow()[self.index.get()].id.clone();
        glib::MainContext::default().spawn_local(async move {
            let Some(viewer) = weak.upgrade() else { return };
            let Some(ui) = viewer.ui.upgrade() else {
                return;
            };
            let owner = viewer.owner.clone();
            let peer = viewer.peer.clone();
            let target = id.clone();
            let result = ui
                .store
                .read_snapshot(move |store| store.media_window(&owner, &peer, &target))
                .await;
            viewer.neighbours_pending.set(false);
            if viewer.closing.get()
                || ui.generation.get() != viewer.generation
                || ui.state.borrow().account != viewer.owner
                || ui.state.borrow().current().jid != viewer.peer
            {
                return;
            }
            if viewer.items.borrow()[viewer.index.get()].id != id {
                viewer.refresh_neighbours();
                return;
            }
            match result {
                Ok(window) if !window.messages.is_empty() => viewer.apply_media_window(window),
                Ok(_) => viewer.close(),
                Err(_) => ui.notice("Adjacent photos could not be loaded"),
            }
        });
    }
    fn apply_media_window(self: &Rc<Self>, window: crate::media_store::MediaWindow) {
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
                pic.set_margin_top(72);
                pic.set_margin_bottom(20);
                pic.set_margin_start(20);
                pic.set_margin_end(20);
                let frame: FlyFrame = glib::Object::new();
                frame.set_hexpand(true);
                frame.set_vexpand(true);
                frame.imp().overlay.set(Some(&self.root));
                frame.imp().progress.set(self.progress.get());
                frame.imp().zoom.set(1.);
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
        self.fullscreen_changed();
        self.update_position(0);
    }
    fn update_position(&self, delta: i32) {
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
        let mut playing = false;
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
            let active = viewer.index.get();
            if active.abs_diff(index) > 10 {
                return glib::ControlFlow::Continue;
            }
            let current = index == active;
            let edge = if current { 4096 } else { 384 };
            if !current && loaded_edge.get() == edge {
                return glib::ControlFlow::Continue;
            }
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
            if file.video_candidate() {
                if !current {
                    if let Some(player) = ui.images.borrow_mut().video(&file.key()) {
                        if player.media.is_prepared() {
                            picture.set_paintable(Some(&player.media.current_image()));
                        }
                    }
                    return glib::ControlFlow::Continue;
                }
                let Some(path) = ui.cached_image_source(&file) else {
                    if current {
                        viewer.preview_status.set_text("Loading video…");
                        viewer.preview_status.set_visible(true);
                    }
                    return glib::ControlFlow::Continue;
                };
                let player = viewer
                    .playbacks
                    .borrow()
                    .get(&id)
                    .cloned()
                    .or_else(|| ui.video(&file, &path));
                let Some(player) = player else {
                    if current {
                        viewer.preview_status.set_text("Preparing video…");
                        viewer.preview_status.set_visible(true);
                    }
                    return glib::ControlFlow::Continue;
                };
                if player.media.error().is_some() {
                    if current {
                        viewer.set_active_video(None);
                        viewer.preview_status.set_text("Video playback unavailable");
                        viewer.preview_status.set_visible(true);
                    }
                    return glib::ControlFlow::Continue;
                }
                {
                    let mut playbacks = viewer.playbacks.borrow_mut();
                    playbacks.clear();
                    playbacks.insert(id.clone(), player.clone());
                }
                if current {
                    // Clapper must be mapped before it can preroll its real controls and sink.
                    viewer.set_active_video(Some(player.clone()));
                }
                if !player.media.is_prepared() || !player.media.has_video() {
                    if current {
                        viewer.preview_status.set_text("Preparing video…");
                        viewer.preview_status.set_visible(true);
                    }
                    return glib::ControlFlow::Continue;
                }
                picture.set_paintable(Some(&player.media));
                if current {
                    viewer.preview_status.set_visible(false);
                }
                return glib::ControlFlow::Continue;
            }
            if loaded_edge.get() == edge {
                if index == viewer.index.get() {
                    viewer.preview_status.set_visible(false);
                }
                return glib::ControlFlow::Continue;
            }
            if index == viewer.index.get() {
                let status = ui.downloads.borrow().status(&viewer.owner, &file);
                let feedback = match status {
                    Some(
                        crate::downloads::Status::Failed(reason)
                        | crate::downloads::Status::Deferred { reason, .. },
                    ) => reason,
                    Some(crate::downloads::Status::Cancelled) => "Download cancelled",
                    _ => "Loading preview…",
                };
                viewer.preview_status.set_text(feedback);
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
                            if current && image.animated && !playing {
                                playing = true;
                                let weak = Rc::downgrade(&viewer);
                                let id = id.clone();
                                crate::image_loader::play(
                                    &picture,
                                    path,
                                    file,
                                    2048,
                                    move |_| {
                                        weak.upgrade().is_some_and(|viewer| {
                                            !viewer.closing.get()
                                                && viewer.items.borrow()[viewer.index.get()].id
                                                    == id
                                        })
                                    },
                                    |picture, frame| picture.set_paintable(Some(&frame)),
                                );
                            }
                            loaded_edge.set(edge);
                            if index == viewer.index.get() {
                                viewer.preview_status.set_visible(false);
                            }
                        }
                    }
                    Some(Err(_)) if index == viewer.index.get() => {
                        viewer.preview_status.set_text("Image preview unavailable");
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
    fn reveal_controls(self: &Rc<Self>) {
        for control in &self.controls {
            control.set_reveal_child(true);
        }
        self.video_controls
            .set_reveal_child(self.active_video.borrow().is_some());
        if let Some(timer) = self.controls_timer.borrow_mut().take() {
            timer.remove();
        }
        if self
            .ui
            .upgrade()
            .is_some_and(|ui| ui.window.is_fullscreen())
        {
            let weak = Rc::downgrade(self);
            let timer = glib::timeout_add_local_once(Duration::from_secs(3), move || {
                if let Some(viewer) = weak.upgrade() {
                    viewer.controls_timer.borrow_mut().take();
                    for control in &viewer.controls {
                        control.set_reveal_child(false);
                    }
                    viewer.video_controls.set_reveal_child(false);
                }
            });
            self.controls_timer.replace(Some(timer));
        }
    }
    fn fullscreen_changed(self: &Rc<Self>) {
        self.reset_zoom();
        self.reveal_controls();
        let Some(ui) = self.ui.upgrade() else { return };
        let full = ui.window.is_fullscreen();
        for pic in self.pictures.borrow().iter() {
            pic.set_margin_top(if full { 0 } else { 72 });
            pic.set_margin_bottom(if full { 0 } else { 20 });
            pic.set_margin_start(if full { 0 } else { 20 });
            pic.set_margin_end(if full { 0 } else { 20 })
        }
        self.fullscreen.set_icon_name(if full {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
    }
    fn set_active_video(&self, player: Option<Rc<crate::video_ui::Playback>>) {
        let unchanged = self
            .active_video
            .borrow()
            .as_ref()
            .zip(player.as_ref())
            .is_some_and(|(current, next)| Rc::ptr_eq(current, next));
        if unchanged {
            return;
        }
        if let Some(current) = self.active_video.borrow_mut().take() {
            if current.controls.parent().as_ref() == Some(self.controls_mount.upcast_ref()) {
                self.controls_mount.remove(&current.controls);
            }
            if current.replay.parent().as_ref() == Some(self.replay_mount.upcast_ref()) {
                self.replay_mount.remove(&current.replay);
            }
            let poster = current.media.current_image();
            for picture in self.pictures.borrow().iter() {
                if picture.paintable().is_some_and(|image| {
                    image.downcast_ref::<gtk::MediaStream>() == Some(&current.media)
                }) {
                    picture.set_paintable(Some(&poster));
                }
            }
            current.media.pause();
            current.media.set_muted(true);
            current.media.set_loop(true);
            current.expanded.set(false);
        }
        if let Some(player) = player {
            player.expanded.set(true);
            player.media.set_loop(false);
            player.media.set_muted(false);
            self.controls_mount.append(&player.controls);
            self.replay_mount.append(&player.replay);
            self.video_controls.set_reveal_child(true);
            player.media.play();
            self.active_video.replace(Some(player));
        } else {
            self.video_controls.set_reveal_child(false);
        }
    }
    pub fn close_immediate(self: &Rc<Self>) {
        self.closing.set(true);
        self.set_active_video(None);
        if let Some(source) = self.source.as_ref().and_then(|source| source.upgrade()) {
            source.set_opacity(1.);
        }
        if let Some(timer) = self.controls_timer.borrow_mut().take() {
            timer.remove();
        }
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
        self.reset_zoom();
        self.set_active_video(None);
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
            360,
            adw::CallbackAnimationTarget::new(move |p| {
                if let Some(v) = weak.upgrade() {
                    v.animate(p)
                }
            }),
        );
        let v = self.clone();
        a.connect_done(move |_| {
            if let Some(source) = v.source.as_ref().and_then(|source| source.upgrade()) {
                source.set_opacity(1.);
            }
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
