use adw::prelude::*;
mod action_store;
mod attachment_ui;
mod avatar;
mod avatar_ui;
mod contact_access;
mod contact_access_ui;
mod credentials;
mod crypto;
mod dialogs;
mod download_smoke;
mod downloads;
mod emoji_completion;
mod encryption_policy;
mod file_share;
mod folders;
mod forwarding;
mod history_window;
mod history_window_ui;
mod image_loader;
mod image_ui;
mod invitation_ui;
mod iq;
mod legacy_polls;
mod live_actions;
mod media;
mod media_caption;
mod media_store;
mod message_actions;
mod messages;
mod model;
mod polls;
mod polls_ui;
mod protocol;
mod request_logic;
mod room_ui;
mod rooms;
mod send_policy_ui;
mod smoke;
mod storage;
mod transport;
mod ui;
mod upload_smoke;
mod upload_ui;
mod uploads;
mod video_ui;
mod wallet;
mod widgets;
fn main() -> gtk::glib::ExitCode {
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
                gtk::gio::ApplicationFlags::NON_UNIQUE
            } else {
                gtk::gio::ApplicationFlags::empty()
            },
        )
        .build();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let ui = ui::Ui::new(
            app,
            std::env::args().any(|s| {
                matches!(
                    s.as_str(),
                    "--smoke-test" | "--live-smoke-test" | "--crypto-smoke-test"
                )
            }),
        );
        let css = gtk::CssProvider::new();
        css.load_from_path(widgets::asset("style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("GTK display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        if std::env::args().any(|s| s == "--show-media") {
            ui.toggle_media();
        }
        if std::env::var_os("MONOGRAM_CAPTURE_VIDEO").is_some() {
            ui.window.set_default_size(1120, 760);
            if std::env::var_os("MONOGRAM_CAPTURE_PATH").is_some() {
                ui.window.set_resizable(false);
            }
        }
        present_window(&ui.window, monitor.as_deref(), started);
        if !ui.smoke {
            ui.reconnect_saved();
        }
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
                    if widget.widget_name() == id && widget.has_css_class("message-media-tile") {
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
        ROOT.with(|root| root.replace(Some(ui)));
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

fn capture_window(
    window: &adw::ApplicationWindow,
    paintable: &gtk::WidgetPaintable,
    path: std::path::PathBuf,
) {
    let (width, height) = (window.width() as f32, window.height() as f32);
    eprintln!(
        "Monogram capture: {}x{}, fullscreen={}, maximized={}",
        width, height, window.is_fullscreen(), window.is_maximized()
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
