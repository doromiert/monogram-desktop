use crate::{
    protocol::BackendEvent,
    rooms::invitations::{Incoming, Kind},
    ui::Ui,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
#[track_caller]
fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    let ctx = glib::MainContext::default();
    while Instant::now() < end {
        for _ in 0..100 {
            if !ctx.pending() {
                break;
            }
            ctx.iteration(false);
        }
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(ready(), "native follow-up did not settle");
}
fn find<T: IsA<gtk::Widget> + glib::object::IsClass + Clone + 'static>(
    root: &gtk::Widget,
    predicate: &impl Fn(&T) -> bool,
) -> Option<T> {
    if let Ok(widget) = root.clone().downcast::<T>() {
        if predicate(&widget) {
            return Some(widget);
        }
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(found) = find(&widget, predicate) {
            return Some(found);
        }
    }
    None
}
#[gtk::test]
#[ignore = "requires a private GTK display and keyring"]
fn accepted_invites_profile_cards_audio_and_compact_pack_strip() {
    assert_eq!(
        std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
        Ok("1")
    );
    adw::init().unwrap();
    crate::widgets::install_icons();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramProfileAudioTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let css = gtk::CssProvider::new();
    css.load_from_path(crate::widgets::asset("style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let ui = Ui::new(&app, true);
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let owner = "own@fixture.invalid";
    ui.store
        .save_account(&crate::model::Account {
            jid: owner.into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        })
        .unwrap();
    ui.state.borrow_mut().account = owner.into();
    ui.window.set_default_size(1200, 900);
    ui.window.present();
    let room = "invited@rooms.fixture.invalid";
    ui.receive_room_invitation(Incoming {
        room: room.into(),
        inviter: "friend@fixture.invalid".into(),
        reason: "Join us".into(),
        password: None,
        kind: Kind::Mediated,
    });
    assert_eq!(ui.request_count(), 1);
    ui.requests_and_invitations();
    wait(|| ui.window.visible_dialog().is_some());
    let popup = ui.window.visible_dialog().unwrap();
    let row = find::<adw::ActionRow>(popup.upcast_ref(), &|row| row.title() == room).unwrap();
    row.emit_by_name::<()>("activated", &[]);
    wait(|| {
        ui.window
            .visible_dialog()
            .is_some_and(|d| d.title() == "Join Room")
    });
    let generation = ui.generation.get();
    ui.backend_event(
        generation,
        "other@fixture.invalid",
        BackendEvent::RoomInvitationsAccepted(vec![room.into()]),
    );
    assert_eq!(ui.request_count(), 1);
    ui.backend_event(
        generation,
        owner,
        BackendEvent::RoomInvitationsAccepted(vec![room.into()]),
    );
    assert_eq!(ui.request_count(), 0);
    wait(|| {
        ui.window
            .visible_dialog()
            .is_none_or(|d| d.title() != "Join Room")
    });
    if let Some(dialog) = ui.window.visible_dialog() {
        dialog.close();
    }
    wait(|| ui.window.visible_dialog().is_none());
    let peer = "alice@fixture.invalid";
    let id = {
        let mut state = ui.state.borrow_mut();
        let chat = state.chat_for(peer);
        chat.name = "Alice".into();
        chat.subtitle = "Online".into();
        chat.id.clone()
    };
    ui.select_chat(&id);
    ui.profile_cache.borrow_mut().insert(
        peer.into(),
        (
            Instant::now(),
            crate::profiles::Profile {
                fields: vec![
                    ("About".into(), "Hello from mobile".into()),
                    ("Website".into(), "https://example.org".into()),
                ],
            },
        ),
    );
    let begin = Instant::now();
    ui.title.emit_clicked();
    assert!(begin.elapsed() < Duration::from_millis(400));
    wait(|| {
        ui.active_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.is_mapped())
    });
    let popup = ui.active_profile.borrow().clone().unwrap();
    assert!(find::<gtk::Label>(popup.upcast_ref(), &|l| l.text() == "Hello from mobile").is_some());
    assert!(
        find::<gtk::Button>(popup.upcast_ref(), &|b| b.label().as_deref()
            == Some("Message"))
        .is_some()
    );
    let settle = Instant::now();
    wait(|| settle.elapsed() > Duration::from_millis(300));
    assert!(
        std::process::Command::new("import")
            .args(["-window", "root", "/tmp/monogram-profile-card.png"])
            .status()
            .unwrap()
            .success()
    );
    popup.popdown();
    wait(|| !popup.is_visible());
    ui.rebuild_pack_library();
    wait(|| !ui.pack_pages.borrow().is_empty());
    ui.toggle_media();
    ui.media_tabs.set_active_name(Some("stickers"));
    wait(|| {
        find::<gtk::Box>(ui.window.upcast_ref(), &|b| {
            b.has_css_class("pack-selector") && b.is_mapped()
        })
        .is_some()
    });
    let library = ui.pack_library.borrow().clone().unwrap();
    glib::MainContext::default()
        .block_on(
            library.create(
                "Video icon".into(),
                crate::packs::Kind::Stickers,
                vec![crate::packs::Input {
                    path: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/picker-video.webm"),
                    name: "Video".into(),
                    emoji: "🙂".into(),
                }],
                crate::packs::Job::new(),
            ),
        )
        .unwrap();
    ui.pack_pages.borrow()[0].refresh();
    wait(|| {
        find::<gtk::ToggleButton>(ui.window.upcast_ref(), &|b| {
            b.tooltip_text().as_deref() == Some("Video icon")
        })
        .is_some_and(|b| {
            find::<gtk::Picture>(b.upcast_ref(), &|p| {
                p.paintable().is_some_and(|p| p.intrinsic_height() > 0)
            })
            .is_some()
        })
    });
    let bar = find::<gtk::Box>(ui.window.upcast_ref(), &|b| {
        b.has_css_class("pack-selector") && b.is_mapped()
    })
    .unwrap();
    assert!(bar.height() < 64, "pack bar expanded to {}", bar.height());
    let page = ui.pack_pages.borrow()[0].root.clone();
    assert_eq!(page.margin_start(), 0);
    assert_eq!(page.margin_end(), 0);
    glib::MainContext::default()
        .block_on(gio::spawn_blocking(crate::video_ui::initialize))
        .unwrap();
    for name in ["silence.mp3", "voice.ogg"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/audio")
            .join(name);
        let input = crate::downloads::VerifiedFile(std::fs::File::open(path).unwrap());
        let player = crate::video_ui::Playback::new_audio(input).unwrap();
        player.media.play();
        wait(|| player.media.is_prepared() && player.media.timestamp() > 100_000);
        assert!(player.media.has_audio());
        assert!(!player.media.has_video());
        assert!(!player.media.is_loop());
        assert!(!player.media.is_muted());
        assert!(player.media.duration() > 7_000_000);
        assert!(player.media.is_seekable());
        player.media.seek(3_000_000);
        wait(|| !player.media.is_seeking() && player.media.timestamp() >= 2_900_000);
        player.media.pause();
        assert!(!player.media.is_playing());
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio/voice.ogg");
    let attachment = crate::file_share::Attachment {
        wire_url: "https://files.fixture.invalid/voice.ogg".into(),
        name: "Voice recording.ogg".into(),
        mime: "audio/ogg".into(),
        size: Some(std::fs::metadata(&path).unwrap().len()),
        width: None,
        height: None,
        hashes: Default::default(),
        manual: false,
        sticker: false,
    };
    let root = crate::downloads::account_root(owner).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(path, root.join(attachment.key())).unwrap();
    let message = crate::model::Message {
        id: "audio-fixture".into(),
        kind: "file".into(),
        author: "Alice".into(),
        sender: peer.into(),
        time: "12:34".into(),
        attachment: Some(attachment),
        ..Default::default()
    };
    ui.state.borrow_mut().chat_for(peer).messages.push(message);
    ui.render_messages_at(crate::messages::ScrollTarget::Bottom);
    wait(|| {
        find::<gtk::Box>(ui.timeline.upcast_ref(), &|b| {
            b.has_css_class("audio-attachment")
        })
        .is_some()
    });
    let audio = find::<gtk::Box>(ui.timeline.upcast_ref(), &|b| {
        b.has_css_class("audio-attachment")
    })
    .unwrap();
    find::<gtk::Button>(audio.upcast_ref(), &|b| {
        b.tooltip_text().as_deref() == Some("Play audio")
    })
    .unwrap()
    .emit_clicked();
    wait(|| {
        find::<gtk::MediaControls>(audio.upcast_ref(), &|c| {
            c.media_stream()
                .is_some_and(|m| m.is_prepared() && m.timestamp() > 100_000)
        })
        .is_some()
    });
    assert!(
        find::<gtk::Button>(audio.upcast_ref(), &|b| b.tooltip_text().as_deref()
            == Some("Save Audio As…")
            && b.is_visible())
        .is_some()
    );
    let settle = Instant::now();
    wait(|| settle.elapsed() > Duration::from_millis(300));
    assert!(
        std::process::Command::new("import")
            .args(["-window", "root", "/tmp/monogram-audio-preview.png"])
            .status()
            .unwrap()
            .success()
    );
    let fake = std::env::temp_dir().join(format!("{}.mp3", uuid::Uuid::new_v4()));
    std::fs::write(&fake, b"#EXTM3U\nhttps://private.invalid/never-fetch").unwrap();
    assert!(
        crate::video_ui::Playback::new_audio(crate::downloads::VerifiedFile(
            std::fs::File::open(&fake).unwrap()
        ))
        .is_none()
    );
    let _ = std::fs::remove_file(fake);
    ui.window.close();
}
