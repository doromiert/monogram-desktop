//! Mobile-style reply thumbnails reuse authenticated, account-scoped media previews.
use crate::{file_share::Attachment, model::Message, ui::Ui, widgets::AspectImage};
use adw::prelude::*;
use gtk::glib;
use std::{rc::Rc, time::Duration};

fn image(message: &Message) -> Option<Attachment> {
    if message.retracted || message.unavailable || crate::formatting_ui::model(message).spoiler {
        return None;
    }
    message
        .attachment
        .as_ref()
        .filter(|file| file.image_candidate() && !file.sticker)
        .cloned()
}

pub fn attach(ui: &Rc<Ui>, reply: &Message, row: &gtk::Box) {
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
    row.append(&slot);
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
        let allow_auto =
            !ui.smoke && (source.own || source.encrypted || ui.state.borrow().current().contact);
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
                    .start_automatic(&owner, file.clone());
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
        assert!(image(&message).is_none());
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
                reply: "You · 📎 image.png".into(),
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
        state.current_mut().messages.push(reply);
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
