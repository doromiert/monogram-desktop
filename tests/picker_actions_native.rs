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
#[track_caller]
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
    let (tx, ty) = ui.window.surface_transform();
    let x = (bounds.x() + bounds.width() / 2. - tx as f32) as i32;
    let y = (bounds.y() + bounds.height() / 2. - ty as f32) as i32;
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
fn drag_between(ui:&Ui,source:&gtk::Widget,target:&gtk::Widget){
    // AdwDialog scales its contents while opening; screen coordinates need settled bounds.
    wait(||[source,target].iter().all(|widget|widget.is_mapped()&&widget.width()>0&&widget.compute_bounds(&ui.window).is_some_and(|bounds|bounds.width()+1.>=widget.width()as f32&&bounds.height()+1.>=widget.height()as f32)));

    let out=std::process::Command::new("xdotool").args(["search","--onlyvisible","--pid",&std::process::id().to_string(),"--name","Monogram Gesture Fixture"]).output().unwrap();let window=String::from_utf8(out.stdout).unwrap().lines().next().unwrap().to_string();
    let a=source.compute_bounds(&ui.window).unwrap();let b=target.compute_bounds(&ui.window).unwrap();assert!(a.width()>0.&&a.height()>0.&&b.width()>0.&&b.height()>0.);let(tx,ty)=ui.window.surface_transform();let sx=a.x()+a.width()/2.-tx as f32;let sy=a.y()+a.height()/2.-ty as f32;let ex=b.x()+b.width()/2.-tx as f32;let ey=b.y()+b.height()/2.-ty as f32;
    let task=std::thread::spawn(move||{let motion=|x:f32,y:f32|{assert!(std::process::Command::new("xdotool").args(["mousemove","--sync","--window",&window,&(x.round() as i32).to_string(),&(y.round() as i32).to_string()]).status().unwrap().success());};motion(sx,sy);std::process::Command::new("xdotool").args(["mousedown","1"]).status().unwrap();std::thread::sleep(Duration::from_millis(100));for n in 1..=8{motion(sx+(ex-sx)*n as f32/8.,sy+(ey-sy)*n as f32/8.);std::thread::sleep(Duration::from_millis(35));}std::thread::sleep(Duration::from_millis(150));std::process::Command::new("xdotool").args(["mouseup","1"]).status().unwrap();});wait(||task.is_finished());task.join().unwrap();
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
        preview.paintable(),
        find::<gtk::Picture>(&tile().unwrap().upcast(), &|p| p.paintable().is_some())
            .unwrap()
            .paintable(),
        "preview did not browse to raw artwork"
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
    let asset = context.block_on(library.asset(pack.items[0].id)).unwrap();
    let other = context
        .block_on(
            library.create(
                "Other fixture".into(),
                Kind::Stickers,
                (0..700)
                    .map(|i| packs::Input {
                        path: asset.path.clone(),
                        name: format!("Other sticker {i}"),
                        emoji: "🙂".into(),
                    })
                    .collect(),
                Job::new(),
            ),
        )
        .unwrap();
    page.refresh();
    let bar = |name: &str| {
        find::<gtk::ToggleButton>(&page.root.clone().upcast(), &|b| {
            b.tooltip_text().as_deref() == Some(name)
        })
    };
    wait(|| {
        bar("Editable fixture").is_some_and(|b| b.is_mapped() && b.width() > 0)
            && bar("Other fixture").is_some_and(|b| b.is_mapped() && b.width() > 0)
    });
    let before = context
        .block_on(library.snapshot())
        .unwrap()
        .picker_revision;
    drag_between(&ui,&bar("Editable fixture").unwrap().upcast(),&bar("Other fixture").unwrap().upcast());
    wait(|| {
        context
            .block_on(library.snapshot())
            .unwrap()
            .packs
            .first()
            .is_some_and(|p| p.id == pack.id)
    });
    assert!(
        context
            .block_on(library.snapshot())
            .unwrap()
            .picker_revision
            > before
    );
    crate::pack_editor::manage(&ui,&page,library.clone(),Kind::Stickers);
    wait(||ui.window.visible_dialog().is_some_and(|dialog|dialog.title()=="Manage Packs"&&find::<adw::ActionRow>(&dialog.upcast(),&|row|row.title()=="Other fixture"&&row.is_mapped()&&row.width()>0&&row.height()>0).is_some()));
    let manager=ui.window.visible_dialog().unwrap();
    let first=find::<adw::ActionRow>(&manager.clone().upcast(),&|row|row.title()=="Editable fixture").unwrap();
    let second=find::<adw::ActionRow>(&manager.clone().upcast(),&|row|row.title()=="Other fixture").unwrap();
    assert!(find::<gtk::Switch>(&first.clone().upcast(),&|_|true).is_some());
    drag_between(&ui,&first.clone().upcast(),&second.upcast());
    wait(||context.block_on(library.snapshot()).unwrap().packs[0].id==other.id);
    find::<gtk::Button>(&first.upcast(),&|b|b.tooltip_text().as_deref()==Some("Edit Pack")).unwrap().emit_clicked();
    wait(||ui.window.visible_dialog().is_some_and(|d|d.title()=="Editable fixture"));
    ui.window.visible_dialog().unwrap().close();manager.close();wait(||ui.window.visible_dialog().is_none());
    let baseline = std::fs::read_dir("/proc/self/fd").unwrap().count();
    let peak = Rc::new(std::cell::Cell::new(baseline));
    let gap = Rc::new(std::cell::Cell::new(Duration::ZERO));
    let last = Rc::new(std::cell::Cell::new(Instant::now()));
    let (p, g, l) = (peak.clone(), gap.clone(), last.clone());
    let heartbeat = glib::timeout_add_local(Duration::from_millis(16), move || {
        p.set(
            p.get()
                .max(std::fs::read_dir("/proc/self/fd").unwrap().count()),
        );
        g.set(g.get().max(l.replace(Instant::now()).elapsed()));
        glib::ControlFlow::Continue
    });
    crate::pack_editor::open(&ui, &page, library.clone(), other.id);
    wait(|| {
        ui.window.visible_dialog().is_some_and(|dialog| {
            find::<crate::widgets::AspectImage>(&dialog.upcast(), &|image| {
                image.paintable().is_some()
            })
            .is_some()
        })
    });
    let large = ui.window.visible_dialog().unwrap();
    if let Some(scroll) = find::<gtk::ScrolledWindow>(&large.clone().upcast(), &|_| true) {
        let a = scroll.vadjustment();
        a.set_value((a.upper() - a.page_size()).max(0.));
        glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(250)));
        a.set_value(0.);
    }
    large.close();
    drop(large);
    wait(|| ui.window.visible_dialog().is_none());
    heartbeat.remove();
    assert!(
        peak.get() < baseline + 128,
        "large editor pinned descriptors {}→{}",
        baseline,
        peak.get()
    );
    assert!(
        gap.get() < Duration::from_millis(400),
        "large editor froze GTK {:?}",
        gap.get()
    );
    println!(
        "Editor700: FD {baseline}→{}, maxGTKgap {:?}",
        peak.get(),
        gap.get()
    );
    crate::pack_editor::open(&ui, &page, library.clone(), pack.id);
    wait(|| {
        ui.window
            .visible_dialog()
            .is_some_and(|d| find::<gtk::GridView>(&d.upcast(), &|_| true).is_some())
    });
    let dialog = ui.window.visible_dialog().unwrap();
    if let Some(path) = std::env::var_os("MONOGRAM_PICKER_CAPTURE") {
        wait(|| {
            find::<crate::widgets::AspectImage>(&dialog.clone().upcast(), &|image| {
                image.paintable().is_some() && image.width() > 0
            })
            .is_some()
        });
        glib::MainContext::default().block_on(glib::timeout_future(Duration::from_millis(350)));
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(&ui.window)).snapshot(
            &snapshot,
            ui.window.width() as f64,
            ui.window.height() as f64,
        );
        let node = snapshot.to_node().unwrap();
        ui.window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(path)
            .unwrap();
    }
    dialog.activate_action("pack.reorder", None).unwrap();
    let editor_tile = |name: &str| {
        find::<gtk::Button>(&dialog.clone().upcast(), &|b| {
            b.tooltip_text().as_deref() == Some(name)
        })
    };
    wait(||find::<gtk::Label>(&dialog.clone().upcast(),&|l|l.text().starts_with("Drag stickers to reorder")).is_some()&&editor_tile("First fixture").is_some_and(|b| b.is_mapped()&&b.is_sensitive()&&b.width()>0));
    physical(
        &ui,
        &editor_tile("First fixture").unwrap().upcast(),
        &[
            "mousedown",
            "1",
            "mousemove_relative",
            "--sync",
            "16",
            "0",
            "sleep",
            "0.1",
        ],
    );
    physical(
        &ui,
        &editor_tile("Second fixture").unwrap().upcast(),
        &["sleep", "0.1", "mouseup", "1"],
    );
    wait(|| {
        context
            .block_on(library.snapshot())
            .unwrap()
            .packs
            .iter()
            .find(|p| p.id == pack.id)
            .unwrap()
            .items[0]
            .id
            == pack.items[1].id
    });
    dialog.activate_action("pack.rename", None).unwrap();
    wait(|| {
        ui.window
            .visible_dialog()
            .and_then(|d| find::<adw::EntryRow>(&d.upcast(), &|r| r.title() == "Pack name"))
            .is_some()
    });
    let name = ui
        .window
        .visible_dialog()
        .and_then(|d| find::<adw::EntryRow>(&d.upcast(), &|r| r.title() == "Pack name"))
        .unwrap();
    name.set_text("Renamed fixture");
    let alert = ui
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    find::<gtk::Button>(&alert.upcast(), &|b| b.label().as_deref() == Some("Save"))
        .unwrap()
        .emit_clicked();
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
    wait(||bar("Renamed fixture").is_some());bar("Renamed fixture").unwrap().emit_clicked();
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
