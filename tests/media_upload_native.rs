// Included in upload_ui::tests to drive transfer phases without a network session.
#[gtk::test]
#[ignore = "requires a private GTK display and keyring"]
fn approved_pq_upload_submits_after_snapshot_staging_without_a_cached_choice() {
    use gtk::gio;
    assert_eq!(std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(), Ok("1"));
    adw::init().unwrap();
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramUploadApprovalTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.register(None::<&gio::Cancellable>).unwrap();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let ui = Ui::new(&app, true);
    let owner = "owner@fixture.invalid";
    let peer = "peer@fixture.invalid";
    ui.store.save_account(&Account { jid:owner.into(), host:String::new(), port:5222, direct_tls:false }).unwrap();
    ui.state.replace(State::live(owner, vec![]));
    let mut photo = Message { id:"approved-photo".into(), wire_id:"approved-photo".into(),
        own:true, sender:owner.into(), kind:"file".into(), delivery:"sending".into(),
        upload:Some(Draft { uri:"file:///fixture.png".into(), name:"fixture.png".into(), error:None, sticker:false, voice:false, local_sticker:None }),
        ..Default::default() };
    photo.set_encryption(Mode::Pq, "fixture-fingerprint".into());
    let mut state = ui.state.borrow_mut();
    let chat = state.chat_for(peer);
    chat.remember_selection(Mode::Pq);
    assert_eq!(chat.selected_encryption(), None);
    chat.messages.push(photo.clone());
    state.active = chat.id.clone();
    ui.store.save(&state).unwrap();
    drop(state);
    ui.online.set(true);
    ui.pq_crypto.borrow_mut().ready = true;
    ui.pq_crypto.borrow_mut().fingerprint = photo.fingerprint.clone();
    let (network, mut received) = tokio::sync::mpsc::unbounded_channel();
    ui.session.replace(Some(crate::protocol::Session::fixture_channel(network.clone())));
    let (finished, result) = mpsc::channel();
    let snapshot = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("approved-wire.part");
    std::fs::write(&snapshot, b"fixture").unwrap();
    let mut file = crate::file_share::Attachment::bare(&format!("aesgcm://files.fixture.invalid/photo#{}", "00".repeat(44))).unwrap();
    file.name = "fixture.png".into();
    file.mime = "image/png".into();
    file.size = Some(7);
    ui.uploads.borrow_mut().tasks.push_back(Task {
        work:Work { owner:owner.into(), peer:peer.into(), id:photo.id.clone(), generation:ui.generation.get(),
            encryption_revision:ui.encryption_revision.get(), mode:Mode::Pq, fingerprint:photo.fingerprint.clone(), draft:photo.upload.clone().unwrap() },
        iq:crate::iq::Client::new(network), job:Arc::new(Job::default()), cancel:None, result:Some(result),
    });
    assert!(finished.send(Ok(Uploaded { attachment:file, snapshot })).is_ok());
    ui.poll_uploads();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let context = glib::MainContext::default();
    let mut submitted = false;
    while std::time::Instant::now() < deadline && !submitted {
        for _ in 0..100 { if !context.pending() { break } context.iteration(false); }
        while let Ok(command) = received.try_recv() {
            if let Command::Send { message, .. } = command {
                assert_eq!(message.wire_id, photo.wire_id);
                assert_eq!(message.mode(), Mode::Pq);
                assert!(message.upload.is_none() && message.attachment.is_some());
                submitted = true;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(submitted, "approved PQ upload was rejected after caching instead of submitted");
    let retained = ui.store.message(owner,peer,&photo.id).unwrap();
    assert_eq!(retained.delivery, "sending");
    ui.store.record_message_failure(owner,peer,&retained.wire_id,"XMPP server response","forbidden [type=auth code=403]").unwrap();
    ui.window.set_default_size(900, 760);
    ui.window.present();
    ui.message_details(&retained);
    if let Ok(path) = std::env::var("MONOGRAM_FAILURE_SCREENSHOT") {
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            for _ in 0..100 { if !context.pending() { break } context.iteration(false); }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(std::process::Command::new("import").args(["-window", "root", &path]).status().unwrap().success());
    }
    ui.window.close();
}

#[gtk::test]
#[ignore = "requires a private GTK display and keyring"]
fn native_media_upload_previews_progress_cancel_and_retry() {
    use gtk::gio;
    use std::time::Instant;
    fn wait(mut ready: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        let context = glib::MainContext::default();
        while Instant::now() < until {
            for _ in 0..100 {
                if !context.pending() {
                    break;
                }
                context.iteration(false);
            }
            if ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(ready(), "media upload UI did not settle");
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
    assert_eq!(
        std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
        Ok("1")
    );
    adw::init().unwrap();
    install_icons();
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramMediaUploadTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let css = gtk::CssProvider::new();
    css.load_from_path(asset("style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let ui = Ui::new(&app, true);
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let owner = "owner@fixture.invalid";
    let peer = "peer@fixture.invalid";
    ui.store
        .save_account(&Account {
            jid: owner.into(),
            host: String::new(),
            port: 5222,
            direct_tls: false,
        })
        .unwrap();
    ui.state.borrow_mut().account = owner.into();
    let chat = ui.state.borrow_mut().chat_for(peer).id.clone();
    ui.select_chat(&chat);
    wait(|| !ui.history_window.busy());
    let uri = gio::File::for_path(asset("icons/mobile-emoji/msg_emoji_cat.png"))
        .uri()
        .to_string();
    let photo = Message {
        id: "photo-upload".into(),
        wire_id: "photo-upload".into(),
        own: true,
        kind: "file".into(),
        delivery: "uploading".into(),
        time: "12:00".into(),
        upload: Some(Draft {
            uri: uri.clone(),
            name: "photo.png".into(),
            error: None,
            sticker: false,
            voice: false,
            local_sticker: None,
        }),
        ..Default::default()
    };
    let mut sticker = photo.clone();
    sticker.id = "sticker-upload".into();
    sticker.wire_id = sticker.id.clone();
    sticker.upload.as_mut().unwrap().sticker = true;
    let mut document = photo.clone();
    document.id = "document-upload".into();
    document.wire_id = document.id.clone();
    document.upload.as_mut().unwrap().name = "document.zip".into();
    assert!(pending_media(&document).is_none());
    ui.state.borrow_mut().chat_for(peer).messages = vec![photo.clone(), sticker.clone(), document];
    ui.store.save(&ui.state.borrow()).unwrap();
    let (tx, _network) = tokio::sync::mpsc::unbounded_channel();
    let (finish, result) = mpsc::channel();
    let job = Arc::new(Job::default());
    *job.phase.lock().unwrap() = Phase::Uploading {
        bytes: 512,
        total: 1024,
    };
    ui.uploads.borrow_mut().tasks.push_back(Task {
        work: Work {
            owner: owner.into(),
            peer: peer.into(),
            id: photo.id.clone(),
            generation: ui.generation.get(),
            encryption_revision: ui.encryption_revision.get(),
            mode: Mode::Plain,
            fingerprint: String::new(),
            draft: photo.upload.clone().unwrap(),
        },
        iq: crate::iq::Client::new(tx),
        job: job.clone(),
        cancel: None,
        result: Some(result),
    });
    let heartbeat = Rc::new(Cell::new(Instant::now()));
    let maximum_gap = Rc::new(Cell::new(Duration::ZERO));
    let (last, gap) = (heartbeat.clone(), maximum_gap.clone());
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        gap.set(gap.get().max(last.replace(Instant::now()).elapsed()));
        glib::ControlFlow::Continue
    });
    ui.window.set_default_size(1200, 1000);
    ui.window.present();
    let started = Instant::now();
    ui.render_messages(true);
    wait(|| ui.bubbles.borrow().contains_key(&photo.id));
    let bubble = ui.bubbles.borrow()[&photo.id].clone();
    assert!(find::<gtk::Overlay>(&bubble, &|w| w.has_css_class("media-upload")).is_some());
    let feedback = started.elapsed();
    assert!(
        feedback < Duration::from_millis(400),
        "upload feedback must be immediate"
    );
    wait(|| find::<AspectImage>(&bubble, &|p| p.paintable().is_some()).is_some());
    wait(|| {
        find::<gtk::Label>(&bubble, &|l| {
            l.text().contains("512") && l.text().contains("1.0")
        })
        .is_some()
    });
    assert!(find::<gtk::ProgressBar>(&bubble, &|_| true).is_none());
    assert!(
        find::<gtk::Spinner>(&bubble, &|s| s.has_css_class("message-status-spinner")
            && s.is_spinning())
        .is_some()
    );
    let ring = find::<gtk::DrawingArea>(&bubble, &|w| w.content_width() == 58).unwrap();
    assert_eq!(ring.width(), 58);
    let sticker_bubble = ui.bubbles.borrow()[&sticker.id].clone();
    wait(|| find::<AspectImage>(&sticker_bubble, &|p| p.paintable().is_some()).is_some());
    assert!(sticker_bubble.has_css_class("sticker-bubble"));
    assert!(find::<gtk::ProgressBar>(&ui.bubbles.borrow()["document-upload"], &|_| true).is_some());
    for (phase, expected) in [
        (Phase::Waiting, "Confirming upload…"),
        (Phase::Reconnecting, "Reconnecting upload…"),
        (Phase::Preparing, "Preparing upload…"),
    ] {
        *job.phase.lock().unwrap() = phase;
        wait(|| find::<gtk::Label>(&bubble, &|l| l.text() == expected).is_some());
    }
    assert!(
        std::process::Command::new("import")
            .args(["-window", "root", "/tmp/monogram-media-upload.png"])
            .status()
            .unwrap()
            .success()
    );
    find::<gtk::Button>(&bubble, &|b| {
        b.tooltip_text()
            .is_some_and(|t| t.starts_with("Cancel upload"))
    })
    .unwrap()
    .emit_clicked();
    assert!(job.cancel.load(std::sync::atomic::Ordering::Acquire));
    assert!(ui.uploads.borrow().phase(&photo.id).is_none());
    let saved = ui.store.message(owner, peer, &photo.id).unwrap();
    assert_eq!(saved.delivery, "failed");
    assert!(saved.upload.unwrap().error.unwrap().contains("cancelled"));
    wait(|| {
        ui.bubbles.borrow().get(&photo.id).is_some_and(|b| {
            find::<gtk::Button>(b, &|b| {
                b.icon_name().as_deref() == Some("view-refresh-symbolic")
            })
            .is_some()
        })
    });
    let failed = ui.bubbles.borrow()[&photo.id].clone();
    // Offline retries keep the original source and provide feedback rather than losing the draft.
    find::<gtk::Button>(&failed, &|b| {
        b.icon_name().as_deref() == Some("view-refresh-symbolic")
    })
    .unwrap()
    .emit_clicked();
    assert!(
        ui.store
            .message(owner, peer, &photo.id)
            .unwrap()
            .upload
            .is_some()
    );
    // The same final-style tile also loads a video poster without running a transfer.
    let mut video = photo.clone();
    video.text = "Video caption".into();
    video.id = "video-upload".into();
    video.wire_id = video.id.clone();
    video.upload.as_mut().unwrap().name = "video.webm".into();
    video.upload.as_mut().unwrap().uri =
        gio::File::for_path(asset("../tests/fixtures/picker-video-large.webm"))
            .uri()
            .into();
    ui.state.borrow_mut().chat_for(peer).messages = vec![video.clone()];
    ui.render_messages(true);
    wait(|| ui.bubbles.borrow().contains_key(&video.id));
    let video_bubble = ui.bubbles.borrow()[&video.id].clone();
    wait(|| find::<AspectImage>(&video_bubble, &|p| p.paintable().is_some()).is_some());
    assert!(find::<gtk::Label>(&video_bubble, &|l| l.text().starts_with("Video caption")).is_some());
    timer.remove();
    eprintln!(
        "media upload feedback={}ms maximum UI gap={}ms",
        feedback.as_millis(),
        maximum_gap.get().as_millis()
    );
    assert!(maximum_gap.get() < Duration::from_millis(400));
    drop(finish);
    ui.window.close();
}
