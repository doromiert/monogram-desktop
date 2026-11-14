use crate::{
    media_library_ui::PackLibraryPage,
    model::Message,
    packs::{self, Job, Kind, Library},
    ui::Ui,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
fn wait(mut ready: impl FnMut() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(7);
    while Instant::now() < deadline {
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
    assert!(ready(), "native picker did not settle");
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
fn app() -> Rc<Ui> {
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramPickerActionsTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ui = Ui::new(&app, true);
    ui.window.set_title(Some("Monogram Gesture Fixture"));
    ui.window.set_default_size(1200, 800);
    let css = gtk::CssProvider::new();
    css.load_from_path(crate::widgets::asset("style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    ui.window.present();
    ui
}
fn physical(ui: &Ui, widget: &gtk::Widget, args: &[&str]) {
    let out = std::process::Command::new("xdotool")
        .args([
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
            "--name",
            "Monogram Gesture Fixture",
        ])
        .output()
        .unwrap();
    let id = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    let bounds = widget.compute_bounds(&ui.window).unwrap();
    assert!(bounds.width() > 0. && bounds.height() > 0.);
    let x = (bounds.x() + bounds.width() / 2.) as i32;
    let y = (bounds.y() + bounds.height() / 2.) as i32;
    let args = args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let task = std::thread::spawn(move || {
        assert!(
            std::process::Command::new("xdotool")
                .args(["mousemove", "--window", &id, &x.to_string(), &y.to_string()])
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    });
    wait(|| task.is_finished());
    task.join().unwrap();
}
#[test]
#[ignore = "requires private display, xdotool and Secret Service"]
fn double_click_defaults_and_preferences_apply_to_real_messages() {
    let ui = app();
    {
        let mut state = ui.state.borrow_mut();
        state.account.clear();
        state.current_mut().messages = vec![
            Message {
                id: "incoming".into(),
                text: "Incoming message".into(),
                kind: "text".into(),
                ..Default::default()
            },
            Message {
                id: "outgoing".into(),
                text: "Outgoing message".into(),
                kind: "text".into(),
                own: true,
                ..Default::default()
            },
        ];
    }
    ui.render_messages(true);
    wait(|| {
        ui.bubbles
            .borrow()
            .get("incoming")
            .is_some_and(|b| b.is_mapped() && b.width() > 0)
    });
    physical(
        &ui,
        &ui.bubbles.borrow()["incoming"],
        ["click", "--repeat", "2", "--delay", "90", "1"].as_slice(),
    );
    wait(|| ui.replying.borrow().as_deref() == Some("incoming"));
    ui.cancel_reply();
    physical(
        &ui,
        &ui.bubbles.borrow()["outgoing"],
        ["click", "--repeat", "2", "--delay", "90", "1"].as_slice(),
    );
    wait(|| find::<gtk::TextView>(&ui.bubbles.borrow()["outgoing"], &|_| true).is_some());
    let page = adw::PreferencesPage::new();
    ui.add_double_click_preferences(&page);
    let row = find::<adw::ComboRow>(&page.clone().upcast(), &|r| {
        r.title() == "Incoming messages"
    })
    .unwrap();
    row.set_selected(0);
    assert_eq!(ui.store.pref("double_click_incoming", ""), "none");
    assert!(!ui.double_click_message("incoming"));
    row.set_selected(3);
    assert!(ui.double_click_message("incoming"));
    ui.window.close();
}
#[test]
#[ignore = "requires private display, xdotool and Secret Service"]
fn picker_zoom_collapse_pack_editor_and_sticker_pending_bubble() {
    let ui = app();
    let owner = "picker-actions@fixture.invalid";
    ui.state.borrow_mut().account = owner.into();
    let context = glib::MainContext::default();
    let library = context.block_on(Library::open(owner)).unwrap();
    let path = std::env::temp_dir().join(format!("picker-actions-{}.png", uuid::Uuid::new_v4()));
    let bytes=base64::Engine::decode(&base64::engine::general_purpose::STANDARD,"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=").unwrap();
    std::fs::write(&path, bytes).unwrap();
    let pack = context
        .block_on(library.create(
            "Editable fixture".into(),
            Kind::Stickers,
            vec![
                packs::Input {
                    path: path.clone(),
                    name: "First fixture".into(),
                    emoji: "🙂".into(),
                },
                packs::Input {
                    path: path.clone(),
                    name: "Second fixture".into(),
                    emoji: "🙂".into(),
                },
            ],
            Job::new(),
        ))
        .unwrap();
    let _ = std::fs::remove_file(path);
    context
        .block_on(library.mutate(packs::Mutation::Favorite {
            item: pack.items[0].id,
            value: true,
        }))
        .unwrap();
    let sent = Rc::new(std::cell::Cell::new(0));
    let selected = sent.clone();
    let page = PackLibraryPage::new(
        library.clone(),
        Kind::Stickers,
        Rc::downgrade(&ui),
        None,
        move |_| selected.set(selected.get() + 1),
        |_| {},
        |_, _, _| {},
    );
    if let Some(old) = ui.media_stack.child_by_name("stickers") {
        ui.media_stack.remove(&old);
    }
    ui.media_stack.add_named(&page.root, Some("stickers"));
    ui.media_stack.set_visible_child_name("stickers");
    ui.utility.set_show_sidebar(true);
    page.refresh();
    let tile = || {
        find::<gtk::Button>(&page.root.clone().upcast(), &|b| {
            b.tooltip_text().as_deref() == Some("Second fixture")
        })
    };
    wait(|| tile().is_some_and(|b| b.is_mapped() && b.width() > 0));
    let first = find::<gtk::Button>(&page.root.clone().upcast(), &|b| {
        b.tooltip_text().as_deref() == Some("First fixture")
    })
    .unwrap();
    physical(&ui, &first.upcast(), &["mousedown", "1", "sleep", "0.5"]);
    assert!(ui.picker_zoom.is_active(), "hold did not preview");
    physical(&ui, &tile().unwrap().upcast(), &["sleep", "0.1"]);
    assert!(
        ui.picker_zoom.is_active(),
        "moving to another tile ended preview"
    );
    let preview = find::<gtk::Picture>(&ui.picker_zoom.root.clone().upcast(), &|_| true).unwrap();
    assert_eq!(
        preview
            .paintable()
            .unwrap()
            .downcast::<gtk::WidgetPaintable>()
            .unwrap()
            .widget(),
        tile().unwrap().first_child(),
        "preview did not browse to next sticker"
    );
    physical(&ui, &tile().unwrap().upcast(), &["mouseup", "1"]);
    wait(|| !ui.picker_zoom.is_active());
    assert_eq!(sent.get(), 0, "preview release sent sticker");
    let heading = find::<gtk::Button>(&page.root.clone().upcast(), &|b| {
        b.has_css_class("picker-section-toggle")
    })
    .unwrap();
    heading.emit_clicked();
    assert_eq!(ui.store.pref("picker:collapsed:stickers:-1", ""), "true");
    crate::pack_editor::open(&ui, &page, library.clone(), pack.id);
    wait(|| {
        ui.window.visible_dialog().is_some_and(|d| {
            find::<adw::EntryRow>(&d.upcast(), &|r| r.title() == "Pack name").is_some()
        })
    });
    let dialog = ui.window.visible_dialog().unwrap();
    let name =
        find::<adw::EntryRow>(&dialog.clone().upcast(), &|r| r.title() == "Pack name").unwrap();
    name.set_text("Renamed fixture");
    name.emit_by_name::<()>("apply", &[]);
    wait(|| {
        context
            .block_on(library.snapshot())
            .unwrap()
            .packs
            .iter()
            .any(|p| p.id == pack.id && p.name == "Renamed fixture")
    });
    dialog.close();
    wait(|| ui.window.visible_dialog().is_none());
    wait(|| tile().is_some_and(|b| b.is_mapped() && b.width() > 0));
    ui.send_animation.remember_sticker_source(&tile().unwrap());
    {
        let mut state = ui.state.borrow_mut();
        state.current_mut().messages = vec![Message {
            id: "pending-sticker".into(),
            kind: "file".into(),
            own: true,
            delivery: "queued".into(),
            upload: Some(crate::model::UploadDraft {
                uri: gio::File::for_path(&pack.items[0].file).uri().into(),
                name: "not shown.webp".into(),
                error: None,
                sticker: true,
                local_sticker: Some(pack.items[0].id),
            }),
            ..Default::default()
        }];
    }
    ui.animate_sent_sticker("pending-sticker");
    ui.render_messages(true);
    wait(|| {
        ui.bubbles.borrow().get("pending-sticker").is_some_and(|b| {
            find::<crate::widgets::AspectImage>(b, &|p| p.paintable().is_some()).is_some()
        })
    });
    let bubble = ui.bubbles.borrow()["pending-sticker"].clone();
    assert!(bubble.has_css_class("sticker-bubble"));
    assert!(find::<gtk::ProgressBar>(&bubble, &|_| true).is_none());
    assert!(find::<gtk::Spinner>(&bubble, &|_| true).is_some());
    assert!(find::<gtk::Label>(&bubble, &|l| l.text() == "not shown.webp").is_none());
    ui.window.close();
}
