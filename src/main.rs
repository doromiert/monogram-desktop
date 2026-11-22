use adw::prelude::*;
mod account_deletion;
mod adhoc;
mod adhoc_ui;
mod action_store;
mod attachment_ui;
mod audio_ui;
mod avatar;
mod avatar_ui;
mod blocking;
mod blocking_ui;
mod clipboard_upload;
mod chat_states;
mod composer_view;
mod chat_navigation;
mod contact_access;
mod contact_details;
mod contact_details_ui;
mod contact_access_ui;
mod composer_attachments_ui;
mod credentials;
mod defaults;
mod keepassxc;
mod keepassxc_ui;
mod crypto;
mod dialogs;
mod download_smoke;
mod downloads;
mod emoji_completion;
mod emoji_picker_ui;
mod emoji_preferences;
mod favorite_order_store;
mod encryption_policy;
mod file_share;
mod folder_actions;
mod folders;
mod forwarding;
mod formatting;
mod formatting_ui;
mod math_ui;
mod renderer_sandbox;
mod reply_preview_ui;
mod navigation_ui;
mod monero_settings_ui;
mod monero_settings;
mod lan_sync;
mod lan_sync_ui;
mod gifs;
mod history_window;
mod history_window_ui;
mod image_loader;
mod image_ui;
mod inline_emoji_ui;
mod invitation_ui;
mod iq;
mod legacy_polls;
mod legacy_inline;
mod link_preview_logic;
mod link_preview_ui;
mod link_previews;
mod live_actions;
mod lottie;
mod markdown;
mod media;
mod media_caption;
mod media_library_ui;
mod media_store;
mod motion_photo;
mod motion_photo_ui;
mod message_actions;
mod messages;
mod model;
mod packs;
mod preview_cache;
mod polls;
mod polls_ui;
mod protocol;
mod profiles;
mod profile_ui;
mod pq_security_ui;
mod read_markers;
mod read_state;
mod request_logic;
#[allow(dead_code)] // Reached from the editor dialog in the next authoring step.
mod media_author_run;
#[allow(dead_code)] // Reached from the editor dialog in the next authoring step.
mod media_authoring;
mod animated_emoji_ui;
mod audio_bubble_ui;
mod audio_check;
mod audio_player;
mod audio_preview_run;
mod client_commands_ui;
mod confined;
mod crop_area;
mod noto_emoji;
mod picture_editor;
mod picture_editor_ui;
mod sysinfo;
mod message_failures;
mod text_safety;
mod toast_routing;
mod room_members_ui;
mod room_ui;
mod rooms;
mod send_policy_ui;
mod selection_actions;
mod send_queue;
mod send_animation;
mod message_gestures;
mod pack_editor;
mod pack_files;
mod picker_zoom;
mod smoke;
mod spoilers;
mod stories;
mod stories_ui;
mod storage;
mod transport;
mod ui;
mod upload_smoke;
mod upload_ui;
mod uploads;
mod video_ui;
mod wallet;
mod components;
mod contact_status;
mod hat_color;
mod last_activity;
mod last_activity_ui;
mod presence_privacy;
#[cfg(test)]
mod runtime_guard;
mod widgets;
thread_local! {static SMOKE_TICK: std::cell::Cell<std::time::Instant> = std::cell::Cell::new(std::time::Instant::now()); static SMOKE_MAX_GAP: std::cell::Cell<u128> = const{std::cell::Cell::new(0)};}
fn main() -> gtk::glib::ExitCode {
    if std::env::args().any(|a|a=="--smoke-test") {
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(4),||{let now=std::time::Instant::now();let gap=SMOKE_TICK.with(|t|now.duration_since(t.replace(now)).as_millis());SMOKE_MAX_GAP.with(|m|m.set(m.get().max(gap)));gtk::glib::ControlFlow::Continue});
    }

    let started = std::time::Instant::now();
    let mut monitor = None;
    let mut args = vec![];
    let mut raw = std::env::args();
    while let Some(arg) = raw.next() {
        if arg == "--monitor" {
            monitor = raw.next();
            if monitor.is_none() {
                eprintln!("--monitor requires a connector name, e.g. DP-2");
                return gtk::glib::ExitCode::FAILURE;
            }
        } else if !matches!(
            arg.as_str(),
            "--smoke-test" | "--live-smoke-test" | "--crypto-smoke-test" | "--show-media"
        ) {
            args.push(arg);
        }
    }
    if std::env::args().any(|s| s == "--crypto-smoke-test") {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1"),
            "Crypto smoke requires a disposable keyring"
        );
    }
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramDesktop")
        .flags(
            if std::env::args().any(|s| {
                matches!(
                    s.as_str(),
                    "--smoke-test" | "--live-smoke-test" | "--crypto-smoke-test"
                )
            }) {
                gtk::gio::ApplicationFlags::NON_UNIQUE | gtk::gio::ApplicationFlags::HANDLES_OPEN
            } else {
                gtk::gio::ApplicationFlags::HANDLES_OPEN
            },
        )
        .build();
    app.connect_open(|app,files,_| {
        app.activate();
        OPEN_FILES.with(|pending| pending.borrow_mut().extend(files.iter().take(16).cloned()));
        ROOT.with(|root| {if let Some(ui)=root.borrow().as_ref(){
            for file in OPEN_FILES.with(|pending|pending.take()){ui.open_pack_file(file);}
        }});
    });
    app.connect_activate(move |app| {
        crate::widgets::install_icons();
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let css = gtk::CssProvider::new();
        css.load_from_path(widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("GTK display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let smoke = std::env::args().any(|s| {
            matches!(
                s.as_str(),
                "--smoke-test" | "--live-smoke-test" | "--crypto-smoke-test"
            )
        });
        let window = ui::Ui::new_window(app);
        if std::env::var_os("MONOGRAM_CAPTURE_VIDEO").is_some() {
            window.set_default_size(1120, 760);
            if std::env::var_os("MONOGRAM_CAPTURE_PATH").is_some() {
                window.set_resizable(false);
            }
        }
        window.set_content(Some(&startup_page()));
        let app = app.clone();
        let ready_window = window.clone();
        after_first_paint(&window, move || {
            let loading_paint = std::time::Instant::now();
            eprintln!(
                "Monogram loading feedback: {} ms",
                started.elapsed().as_millis()
            );
            let ui = ui::Ui::new_with_window(&app, smoke, ready_window);
            #[cfg(debug_assertions)]
            {
                let probe = gtk::gio::SimpleAction::new("diagnose-encryption", None);
                let weak = std::rc::Rc::downgrade(&ui);
                probe.connect_activate(move |_, _| {
                    let Some(ui) = weak.upgrade() else { return };
                    let peer = ui.state.borrow().current().jid.clone();
                    let (reply, receive) = tokio::sync::oneshot::channel();
                    if ui.command(protocol::Command::ChooseEncryption { peer, reply, skip_unreachable: false }) {
                        gtk::glib::MainContext::default().spawn_local(async move {
                            match receive.await {
                                Ok(Ok(mode)) => eprintln!("Monogram diagnostic encryption choice: {mode:?}"),
                                Ok(Err(error)) => eprintln!("Monogram diagnostic encryption choice: {}", error.diagnostic()),
                                Err(_) => eprintln!("Monogram diagnostic encryption choice interrupted"),
                            }
                        });
                    }
                });
                app.add_action(&probe);
            }
            let reconnect = ui.clone();
            after_next_paint(&ui.window, move || {
                eprintln!(
                    "Monogram interface ready: {} ms (frame gap {} ms)",
                    started.elapsed().as_millis(),
                    loading_paint.elapsed().as_millis()
                );

                if std::env::args().any(|s|s=="--show-media") {
                    let ui=reconnect.clone();
                    gtk::glib::idle_add_local_once(move || ui.toggle_media());
                }
                if !reconnect.smoke {
                    // Let the interface paint before restoring the account and transport.
                    gtk::glib::idle_add_local_once(move || reconnect.reconnect_saved());
                }
            });
            if std::env::args().any(|s| s == "--crypto-smoke-test") {
                smoke::run_crypto(ui.clone());
            } else if std::env::args().any(|s| s == "--live-smoke-test") {
                smoke::run_live(ui.clone());
            } else if ui.smoke {
                smoke::run(ui.clone());
            }
            if let Ok(peer) = std::env::var("MONOGRAM_CAPTURE_PEER") {
                let weak = std::rc::Rc::downgrade(&ui);
                gtk::glib::idle_add_local_once(move || {
                    if let Some(ui) = weak.upgrade() {
                        let id = ui
                            .state
                            .borrow()
                            .chats
                            .iter()
                            .find(|chat| chat.jid == peer)
                            .map(|chat| chat.id.clone());
                        if let Some(id) = id {
                            ui.select_chat(&id);
                            if let Ok(video) = std::env::var("MONOGRAM_CAPTURE_VIDEO") {
                                ui.load_timeline(Some(video));
                            }
                        }
                    }
                });
            }
            if let Ok(id) = std::env::var("MONOGRAM_CAPTURE_VIDEO") {
                let weak = std::rc::Rc::downgrade(&ui);
                gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                    let Some(ui) = weak.upgrade() else { return };
                    ui.window.unfullscreen();
                    ui.window.unmaximize();
                    ui.window.set_default_size(1120, 760);
                    fn tile(widget: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
                        if widget.widget_name() == id && widget.has_css_class("message-media-tile")
                        {
                            return Some(widget.clone());
                        }
                        let mut next = widget.first_child();
                        while let Some(child) = next {
                            next = child.next_sibling();
                            if let Some(found) = tile(&child, id) {
                                return Some(found);
                            }
                        }
                        None
                    }
                    if let Some(tile) = tile(ui.timeline.upcast_ref(), &id) {
                        ui.media_viewer(Some(tile));
                    }
                });
            }
            if let Some(path) = std::env::var_os("MONOGRAM_CAPTURE_PATH") {
                let paintable = gtk::WidgetPaintable::new(Some(&ui.window));
                let window = ui.window.downgrade();
                let capture = gtk::gio::SimpleAction::new("capture", None);
                capture.connect_activate(move |_, _| {
                    if let Some(window) = window.upgrade().filter(|window| window.is_mapped()) {
                        capture_window(&window, &paintable, std::path::PathBuf::from(&path));
                    }
                });
                ui.window.add_action(&capture);
                gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || {
                    capture.activate(None);
                });
            }
            ROOT.with(|root| root.replace(Some(ui.clone())));
            for file in OPEN_FILES.with(|pending|pending.take()){ui.open_pack_file(file);}

        });
        present_window(&window, monitor.as_deref(), started);
    });
    let result = app.run_with_args(&args);
    let display = gtk::gdk::Display::default();
    for window in app.windows() {
        window.destroy();
    }
    ROOT.with(|root| root.borrow_mut().take());
    // Drain asynchronous video/GL releases before libEGL's process-exit teardown.
    let cleanup = gtk::glib::MainLoop::new(None, false);
    let finish = cleanup.clone();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        finish.quit();
    });
    cleanup.run();
    drop(display);
    result
}

fn startup_page() -> adw::ToolbarView {
    let view = components::titled_page();
    let spinner = gtk::Spinner::builder().spinning(true).build();
    let page = adw::StatusPage::builder()
        .icon_name("mail-unread-symbolic")
        .title("Opening Monogram")
        .description("Loading your conversations…")
        .child(&spinner)
        .build();
    view.set_content(Some(&page));
    view
}

fn after_first_paint(window: &adw::ApplicationWindow, callback: impl FnOnce() + 'static) {
    let callback = std::rc::Rc::new(std::cell::RefCell::new(Some(callback)));
    window.connect_map(move |window| {
        after_next_paint(window, {
            let callback = callback.clone();
            move || {
                let Some(callback) = callback.borrow_mut().take() else {
                    return;
                };
                gtk::glib::idle_add_local_once(callback);
            }
        });
    });
}

fn after_next_paint(window: &adw::ApplicationWindow, callback: impl FnOnce() + 'static) {
    let Some(clock) = window.frame_clock() else {
        return;
    };
    let callback = std::rc::Rc::new(std::cell::RefCell::new(Some(callback)));
    clock.connect_after_paint(move |_| {
        if let Some(callback) = callback.borrow_mut().take() {
            callback();
        }
    });
}

fn capture_window(
    window: &adw::ApplicationWindow,
    paintable: &gtk::WidgetPaintable,
    path: std::path::PathBuf,
) {
    let (width, height) = (window.width() as f32, window.height() as f32);
    eprintln!(
        "Monogram capture: {}x{}, fullscreen={}, maximized={}",
        width,
        height,
        window.is_fullscreen(),
        window.is_maximized()
    );
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    if let Some(node) = snapshot.to_node()
        && let Some(renderer) = window.renderer()
    {
        let texture = renderer.render_texture(
            &node,
            Some(&gtk::graphene::Rect::new(0., 0., width, height)),
        );
        gtk::gio::spawn_blocking(move || {
            if texture.save_to_png(path).is_ok() {
                eprintln!("Monogram progress screenshot saved");
            }
        });
    } else {
        eprintln!("Monogram capture: no render node available");
    }
    window.set_resizable(true);
}

fn present_window(
    window: &adw::ApplicationWindow,
    connector: Option<&str>,
    started: std::time::Instant,
) {
    use gtk::{gdk, glib};
    use std::{cell::Cell, time::Duration};
    if let Some(connector) = connector {
        let display = gtk::prelude::WidgetExt::display(window);
        let monitors = display.monitors();
        let target = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
            .find(|m| m.connector().as_deref() == Some(connector));
        if let Some(target) = target {
            // Wayland supports an explicit output for the first fullscreen map.
            window.fullscreen_on_monitor(&target);
            let placed = Cell::new(false);
            window.connect_map(move |window| {
                if placed.replace(true) {
                    return;
                }
                if let Some(clock) = window.frame_clock() {
                    let first = Cell::new(true);
                    clock.connect_after_paint(move |_| {
                        if first.replace(false) {
                            eprintln!("Monogram first paint: {} ms", started.elapsed().as_millis());
                        }
                    });
                }
                let weak = window.downgrade();
                let target = target.clone();
                glib::timeout_add_local_once(Duration::from_millis(200), move || {
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    window.unfullscreen();
                    let weak = window.downgrade();
                    glib::timeout_add_local_once(Duration::from_millis(250), move || {
                        let Some(window) = weak.upgrade() else {
                            return;
                        };
                        let Some(surface) = window.surface() else {
                            return;
                        };
                        let display = gtk::prelude::WidgetExt::display(&window);
                        if display.monitor_at_surface(&surface).as_ref() == Some(&target) {
                            eprintln!(
                                "Monogram monitor: {} ({})",
                                target.connector().unwrap_or_default(),
                                if window.is_fullscreen() {
                                    "fullscreen"
                                } else {
                                    "windowed"
                                }
                            );
                        } else {
                            window.fullscreen_on_monitor(&target);
                            let weak = window.downgrade();
                            glib::timeout_add_local_once(Duration::from_millis(250), move || {
                                if let Some(window) = weak.upgrade() {
                                    if let Some(surface) = window.surface() {
                                        let display = gtk::prelude::WidgetExt::display(&window);
                                        if let Some(actual) = display.monitor_at_surface(&surface) {
                                            eprintln!(
                                                "Monogram monitor: {} ({})",
                                                actual.connector().unwrap_or_default(),
                                                if window.is_fullscreen() {
                                                    "fullscreen"
                                                } else {
                                                    "windowed"
                                                }
                                            );
                                        }
                                    }
                                }
                            });
                        }
                    });
                });
            });
        } else {
            eprintln!("Monogram monitor {connector} is unavailable; using compositor placement");
        }
    }
    window.present();
}
thread_local! {static ROOT:std::cell::RefCell<Option<std::rc::Rc<ui::Ui>>>=const{std::cell::RefCell::new(None)};}

#[cfg(test)]
#[path = "../tests/picker_actions_native.rs"]
mod picker_actions_native;

#[cfg(test)]
#[path = "../tests/room_restore_native.rs"]
mod room_restore_native;

thread_local! {static OPEN_FILES:std::cell::RefCell<Vec<gtk::gio::File>>=const{std::cell::RefCell::new(Vec::new())};}

#[cfg(test)]
#[path="../tests/profile_invite_audio_native.rs"]
mod profile_invite_audio_native;
