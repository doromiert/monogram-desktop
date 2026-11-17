//! Mobile-style reply thumbnails reuse authenticated, account-scoped media previews.
use crate::{file_share::Attachment, model::Message, ui::Ui, widgets::AspectImage};
use adw::prelude::*;
use gtk::glib;
use std::{rc::Rc, time::Duration};

fn elide(label: &gtk::Label, source: &str, width: i32) -> String {
    let layout = label.layout().copy();
    layout.set_width(width.max(1) * gtk::pango::SCALE);
    layout.set_wrap(gtk::pango::WrapMode::WordChar);
    layout.set_ellipsize(gtk::pango::EllipsizeMode::None);
    layout.set_height(-1);
    layout.set_text(source);
    if layout.line_count() <= 2 { return source.into(); }
    let boundaries: Vec<_> = source.char_indices().map(|(i,_)|i).chain([source.len()]).collect();
    let (mut low, mut high) = (0, boundaries.len()-1);
    while low < high {
        let middle=(low+high+1)/2;
        layout.set_text(&format!("{}...",source[..boundaries[middle]].trim_end()));
        if layout.line_count() <= 2 { low=middle; } else { high=middle-1; }
    }
    let mut end=boundaries[low];
    if source[..end].chars().last().is_some_and(|c|!c.is_whitespace()) && source[end..].chars().next().is_some_and(|c|!c.is_whitespace()) {
        let start=source[..end].char_indices().rev().find(|(_,c)|c.is_whitespace()).map_or(0,|(i,c)|i+c.len_utf8());
        let word_end=source[start..].char_indices().find(|(_,c)|c.is_whitespace()).map_or(source.len(),|(i,_)|start+i);
        let word=label.create_pango_layout(Some(&source[start..word_end]));
        // Keep ordinary words whole; an over-wide word can use the available line.
        if word.pixel_size().0 <= width { end=start; }
    }
    format!("{}...",source[..end].trim_end())
}

mod snippet_imp {
    use super::*;
    use gtk::subclass::prelude::*;
    #[derive(Default)]
    pub struct Snippet {
        pub label: std::cell::RefCell<Option<gtk::Label>>,
        pub source: std::cell::RefCell<String>,
        pub width: std::cell::Cell<i32>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Snippet {
        const NAME: &'static str = "MonogramReplySnippet";
        type Type = super::Snippet;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Snippet {
        fn dispose(&self) { if let Some(label)=self.label.borrow_mut().take(){label.unparent();} }
    }
    impl WidgetImpl for Snippet {
        fn request_mode(&self)->gtk::SizeRequestMode { gtk::SizeRequestMode::HeightForWidth }
        fn measure(&self, orientation:gtk::Orientation, width:i32)->(i32,i32,i32,i32) {
            let label=self.label.borrow();let Some(label)=label.as_ref() else{return (0,0,-1,-1);};
            let layout=label.layout().copy();layout.set_text(&self.source.borrow());
            layout.set_width(if orientation==gtk::Orientation::Horizontal || width<0 {-1}else{width*gtk::pango::SCALE});
            layout.set_wrap(gtk::pango::WrapMode::WordChar);
            layout.set_ellipsize(gtk::pango::EllipsizeMode::End);layout.set_height(-2);
            let (w,h)=layout.pixel_size();
            if orientation==gtk::Orientation::Horizontal {(0,w,-1,-1)}else{(if width<0{0}else{h},h,-1,-1)}
        }
        fn size_allocate(&self,width:i32,height:i32,baseline:i32) {
            if let Some(label)=self.label.borrow().as_ref() {
                if self.width.replace(width)!=width { label.set_text(&elide(label,&self.source.borrow(),width)); }
                label.measure(gtk::Orientation::Horizontal,-1);
                label.measure(gtk::Orientation::Vertical,width);
                label.allocate(width,height,baseline,None);
            }
        }
        fn snapshot(&self,snapshot:&gtk::Snapshot) { if let Some(label)=self.label.borrow().as_ref(){self.obj().snapshot_child(label,snapshot);} }
    }
}
glib::wrapper! {pub struct Snippet(ObjectSubclass<snippet_imp::Snippet>) @extends gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
impl Snippet {
    pub fn new(source:&str)->Self {
        use gtk::subclass::prelude::*;
        let widget:Self=glib::Object::new();
        // Two visible lines never need an unbounded quoted body to be shaped.
        let mut preview:String=source.chars().take(4096).collect();if preview.len()<source.len(){preview.push_str("...");}
        let label=crate::widgets::wrap(&preview,"reply-preview");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);label.set_lines(2);label.set_yalign(0.);
        label.set_parent(&widget);widget.imp().source.replace(preview);widget.imp().label.replace(Some(label));widget
    }
}

fn image(message: &Message) -> Option<Attachment> {
    if message.retracted || message.unavailable || crate::formatting_ui::model(message).spoiler {
        return None;
    }
    message
        .attachment
        .as_ref()
        .filter(|file| file.image_candidate())
        .cloned()
}

pub fn attach(ui: &Rc<Ui>, reply: &Message, row: &gtk::Grid) {
    if reply.reply_id.is_empty() {
        return;
    }
    let picture = AspectImage::reply_thumbnail();
    let slot = gtk::Overlay::new();
    slot.set_child(Some(&picture));
    slot.set_visible(false);
    slot.set_halign(gtk::Align::Start);
    slot.set_valign(gtk::Align::Center);
    let pending = gtk::Image::from_icon_name("image-x-generic-symbolic");
    pending.set_pixel_size(18);
    slot.add_overlay(&pending);
    row.attach(&slot, 0, 0, 1, 1);
    let (owner, peer) = {
        let state = ui.state.borrow();
        (state.account.clone(), state.current().jid.clone())
    };
    let generation = ui.generation.get();
    let target = reply.reply_id.clone();
    let author = reply.reply_to.clone();
    let ui = Rc::downgrade(ui);
    let slot = slot.downgrade();
    let picture = picture.downgrade();
    let pending = pending.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Some(ui) = ui.upgrade() else {
            return;
        };
        let cached = if owner.is_empty() {
            ui.state
                .borrow()
                .current()
                .messages
                .iter()
                .find(|m| m.id == target)
                .cloned()
        } else {
            let (account, chat, to, id) = (owner.clone(), peer.clone(), author, target);
            ui.store
                .read_snapshot(move |store| Ok(store.resolve_reply(&account, &chat, &to, &id)))
                .await
                .ok()
                .flatten()
        };
        let Some(source) = cached else {
            return;
        };
        let Some(file) = image(&source) else {
            return;
        };
        let (Some(slot), Some(picture), Some(pending)) =
            (slot.upgrade(), picture.upgrade(), pending.upgrade())
        else {
            return;
        };
        if ui.generation.get() != generation
            || ui.state.borrow().account != owner
            || ui.state.borrow().current().jid != peer
        {
            return;
        }
        slot.set_visible(true);
        let auto_flags = (source.own, source.encrypted);
        let ui = Rc::downgrade(&ui);
        let slot = slot.downgrade();
        let picture = picture.downgrade();
        let pending = pending.downgrade();
        let load = move || {
            let (Some(ui), Some(slot), Some(picture), Some(pending)) = (
                ui.upgrade(),
                slot.upgrade(),
                picture.upgrade(),
                pending.upgrade(),
            ) else {
                return glib::ControlFlow::Break;
            };
            let state = ui.state.borrow();
            if ui.generation.get() != generation
                || state.account != owner
                || state.current().jid != peer
            {
                return glib::ControlFlow::Break;
            }
            if state
                .current()
                .messages
                .iter()
                .find(|m| m.id == source.id)
                .is_some_and(|m| image(m).as_ref() != Some(&file))
            {
                slot.set_visible(false);
                return glib::ControlFlow::Break;
            }
            let allow_auto = ui.auto_download_enabled.get() && crate::attachment_ui::automatic_media_allowed(ui.smoke, auto_flags.0, auto_flags.1, state.current());
            drop(state);
            if picture.paintable().is_some() || !ui.in_chat_prefetch(&slot) {
                return glib::ControlFlow::Continue;
            }
            if let Some(path) = ui.cached_image_source(&file) {
                let edge = 72 * picture.scale_factor().max(1) as u32;
                match ui.image(&file, &path, edge) {
                    Some(Ok(image)) => {
                        picture.set_paintable(Some(image.texture.upcast()));
                        pending.set_visible(false);
                    }
                    Some(Err(_)) => return glib::ControlFlow::Break,
                    None => {}
                }
            } else if allow_auto && ui.in_chat_viewport(&slot) {
                ui.downloads
                    .borrow_mut()
                    .start_automatic(&owner, file.clone(), if ui.auto_download_enabled.get(){ui.auto_download_limit.get()}else{0});
            }
            glib::ControlFlow::Continue
        };
        if load() == glib::ControlFlow::Continue {
            glib::timeout_add_local(Duration::from_millis(150), load);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, State};
    use gtk::{gdk, gio};
    use std::time::Instant;

    #[test]
    fn only_visible_image_content_gets_a_reply_thumbnail() {
        let mut file = Attachment::bare("https://fixture.invalid/image.png").unwrap();
        file.mime = "image/png".into();
        let mut message = Message {
            attachment: Some(file),
            ..Default::default()
        };
        assert!(image(&message).is_some());
        message.retracted = true;
        assert!(image(&message).is_none());
        message.retracted = false;
        message.formatting = Some(crate::formatting::Markup {
            spoiler: true,
            ..crate::formatting::Markup::plain("")
        });
        assert!(image(&message).is_none());
        message.formatting = None;
        message.attachment.as_mut().unwrap().sticker = true;
        assert!(image(&message).is_some());
        assert_eq!(message.preview(), "Sticker");
    }
    fn find(widget: &gtk::Widget) -> Option<AspectImage> {
        if widget.has_css_class("reply-image-preview") {
            return widget.clone().downcast().ok();
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    #[test]
    #[ignore = "private native GTK display and keyring"]
    fn reply_quote_uses_the_full_card_width() {
        adw::init().unwrap();
        let app=adw::Application::builder().application_id("com.negzero.MonogramQuoteWidth").flags(gio::ApplicationFlags::NON_UNIQUE).build();app.register(None::<&gio::Cancellable>).unwrap();
        let css=gtk::CssProvider::new();css.load_from_path(crate::widgets::asset("style.css"));gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(),&css,gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let ui=Ui::new(&app,true);
        let message=Message{id:"wide-reply".into(),text:"i did".into(),own:true,reply:"ralu · btw install github-cli (as known as gh)\nand let your GPT talk to claude in PR and describe the changes".into(),..Default::default()};
        ui.state.borrow_mut().current_mut().group=false;ui.state.borrow_mut().current_mut().messages=vec![message];
        ui.window.set_default_size(1400,800);ui.window.present();ui.render_messages(true);
        let context=glib::MainContext::default();let deadline=Instant::now()+Duration::from_millis(350);while Instant::now()<deadline{context.iteration(false);std::thread::sleep(Duration::from_millis(1));}
        fn quote(widget:&gtk::Widget)->Option<gtk::Label>{if let Some(label)=widget.downcast_ref::<gtk::Label>().filter(|w|w.has_css_class("reply-preview")){return Some(label.clone());}let mut c=widget.first_child();while let Some(w)=c{c=w.next_sibling();if let Some(found)=quote(&w){return Some(found);}}None}
        let bubble=ui.bubbles.borrow().get("wide-reply").unwrap().clone();let snippet=quote(&bubble).unwrap();let contents=snippet.parent().unwrap().parent().unwrap();let row=contents.parent().unwrap();
        eprintln!("Reply quote: text={}px, content={}px, row={}px",snippet.width(),contents.width(),row.width());
        assert!(snippet.width()>=row.width()-2,"Quote left {}px unused",row.width()-snippet.width());
        assert!(snippet.layout().line_count()<=2);
        assert!(!snippet.text().contains('\n'),"Reply previews should flow across original line breaks, like mobile");
        let ordinary="hello world some fairly ordinary words that should be cut at a whole word instead of halfway through one";
        let clipped=elide(&snippet,ordinary,140);assert!(clipped.ends_with("..."));let prefix=clipped.strip_suffix("...").unwrap();assert!(ordinary[prefix.len()..].starts_with(char::is_whitespace),"Mid-word cut: {clipped}");
        let long="pneumonoultramicroscopicsilicovolcanoconiosis".repeat(5);let clipped=elide(&snippet,&long,140);assert!(clipped.ends_with("...")&&clipped.len()>3&&clipped.len()<long.len());
        assert_eq!(elide(&snippet,"a short quote",300),"a short quote");
        ui.window.close();
    }

    #[test]
    #[ignore = "private native display, keyring and media cache"]
    fn archived_reply_image_loads_as_a_cropped_square() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        adw::init().unwrap();
        let application = adw::Application::builder()
            .application_id("com.negzero.MonogramDesktop.ReplyImageFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let css = gtk::CssProvider::new();
        css.load_from_path(crate::widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let ui = Ui::new(&application, true);
        let owner = "alice@fixture.invalid";
        let peer = "bob@fixture.invalid";
        ui.store
            .save_account(&Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        let bytes = glib::Bytes::from_owned(
            (0..128 * 64)
                .flat_map(|i| {
                    if i % 128 < 64 {
                        [255u8, 80, 40, 255]
                    } else {
                        [40, 180, 255, 255]
                    }
                })
                .collect::<Vec<_>>(),
        );
        let texture =
            gdk::MemoryTexture::new(128, 64, gdk::MemoryFormat::R8g8b8a8, &bytes, 128 * 4);
        let png = texture.save_to_png_bytes();
        let mut file = Attachment::bare("https://fixture.invalid/archived-reply.png").unwrap();
        file.mime = "image/png".into();
        file.size = Some(png.len() as u64);
        file.hashes.insert(
            "sha-256".into(),
            openssl::sha::sha256(png.as_ref()).to_vec(),
        );
        let root = crate::downloads::account_root(owner).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(file.key()), png.as_ref()).unwrap();
        let mut state = State::live(owner, vec![]);
        state.active = state.chat_for(peer).id.clone();
        state.current_mut().messages = vec![
            Message {
                id: "original-image".into(),
                author: "You".into(),
                own: true,
                kind: "file".into(),
                attachment: Some(file),
                timestamp: "2026-11-14T10:00:00Z".into(),
                ..Default::default()
            },
            Message {
                id: "reply".into(),
                reply: format!("You · 📎 {}...", "longattachmentfilename".repeat(4)),
                reply_id: "original-image".into(),
                reply_to: owner.into(),
                text: "Could be worse".into(),
                own: true,
                timestamp: "2026-11-14T10:01:00Z".into(),
                ..Default::default()
            },
        ];
        let mut reply = state.current_mut().messages.pop().unwrap();
        reply.timestamp = "2026-11-14T10:02:00Z".into();
        for n in 1..80 {
            state.current_mut().messages.push(Message {
                id: format!("earlier-{n}"),
                text: "Earlier message\n".repeat(6),
                timestamp: format!("2026-11-14T10:{:02}:{:02}Z", n / 60, n % 60),
                ..Default::default()
            });
        }
        let caption_reply = Message {
            id: "caption-reply".into(),
            reply: "You · A caption beside a thumbnail must still have room for two lines, including long quotations that need ellipsizing within the card.".into(),
            text: "Reply to a caption".into(),
            ..reply.clone()
        };
        state.current_mut().messages.push(reply);
        state.current_mut().messages.push(caption_reply);
        ui.store.save(&state).unwrap();
        state.current_mut().messages.remove(0);
        ui.activate_state(state);
        ui.window.set_default_size(820, 620);
        ui.window.present();
        let context = glib::MainContext::default();
        let begin = Instant::now();
        let mut gap = Duration::ZERO;
        let mut previous = begin;
        loop {
            while context.pending() {
                context.iteration(false);
            }
            let now = Instant::now();
            gap = gap.max(now - previous);
            previous = now;
            if find(ui.timeline.upcast_ref())
                .is_some_and(|p| p.paintable().is_some() && p.width() == 36 && p.height() == 36)
            {
                break;
            }
            assert!(
                begin.elapsed() < Duration::from_secs(5),
                "Reply thumbnail did not load"
            );
            std::thread::sleep(Duration::from_millis(4));
        }
        let picture = find(ui.timeline.upcast_ref()).unwrap();
        assert_eq!((picture.width(), picture.height()), (36, 36));
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_millis(150) {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
        fn quote(widget: &gtk::Widget) -> Option<gtk::Label> {
            if let Some(label) = widget.downcast_ref::<gtk::Label>().filter(|l| l.has_css_class("reply-preview")) { return Some(label.clone()); }
            let mut child = widget.first_child();
            while let Some(w) = child { child = w.next_sibling(); if let Some(label) = quote(&w) { return Some(label); } }
            None
        }
        let bubble = ui.bubbles.borrow()["reply"].clone();
        let snippet = quote(&bubble).unwrap();
        let card = snippet.ancestor(gtk::Button::static_type()).unwrap();
        let painted = snippet.layout().pixel_size();
        let bounds = snippet.compute_bounds(&card).unwrap();
        eprintln!("reply card={}x{}, snippet={}x{}, painted={:?}, origin={},{}",card.width(),card.height(),snippet.width(),snippet.height(),painted,bounds.x(),bounds.y());
        assert_eq!(snippet.layout().line_count(), 1, "Paperclip and filename must share one line");
        assert!(snippet.text().starts_with("📎 "));
        let caption_bubble = ui.bubbles.borrow()["caption-reply"].clone();
        let caption = quote(&caption_bubble).unwrap();
        let caption_card = caption.ancestor(gtk::Button::static_type()).unwrap();
        let caption_bounds = caption.compute_bounds(&caption_card).unwrap();
        assert_eq!(caption.layout().line_count(), 2, "Text captions retain their two-line preview");
        assert!(caption_bounds.y() + caption.layout().pixel_size().1 as f32 <= caption_card.height() as f32, "Wrapped caption escapes the card");
        assert!(painted.1 <= snippet.height(), "Reply text paints below its allocated height");
        assert!(bounds.y() + painted.1 as f32 <= card.height() as f32, "Reply text escapes the card");
        assert!(gap < Duration::from_millis(400), "UI gap: {gap:?}");
        assert!(
            !ui.state
                .borrow()
                .current()
                .messages
                .iter()
                .any(|m| m.id == "original-image")
        );
        if let Some(path) = std::env::var_os("MONOGRAM_REPLY_CAPTURE") {
            let deadline = Instant::now() + Duration::from_millis(400);
            loop {
                while context.pending() {
                    context.iteration(false);
                }
                let snapshot = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(
                    &snapshot,
                    ui.window.width() as f64,
                    ui.window.height() as f64,
                );
                if let Some(node) = snapshot.to_node() {
                    ui.window
                        .renderer()
                        .unwrap()
                        .render_texture(&node, None)
                        .save_to_png(&path)
                        .unwrap();
                    break;
                }
                assert!(Instant::now() < deadline, "Reply window did not paint");
                std::thread::sleep(Duration::from_millis(4));
            }
        }
        println!(
            "Reply image ready in {:?}; main-loop gap {gap:?}",
            begin.elapsed()
        );
        ui.window.close();
    }
}
