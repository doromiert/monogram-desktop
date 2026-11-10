use adw::prelude::*;
mod action_store;
mod attachment_ui;
mod crypto;
mod dialogs;
mod download_smoke;
mod downloads;
mod file_share;
mod folders;
mod forwarding;
mod iq;
mod legacy_polls;
mod live_actions;
mod media;
mod message_actions;
mod messages;
mod model;
mod polls;
mod polls_ui;
mod protocol;
mod smoke;
mod storage;
mod transport;
mod ui;
mod wallet;
mod widgets;
fn main() -> gtk::glib::ExitCode {
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
    app.connect_activate(|app| {
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
        ui.window.present();
        if std::env::args().any(|s| s == "--crypto-smoke-test") {
            smoke::run_crypto(ui.clone());
        } else if std::env::args().any(|s| s == "--live-smoke-test") {
            smoke::run_live(ui.clone());
        } else if ui.smoke {
            smoke::run(ui.clone());
        }
        ROOT.with(|root| root.replace(Some(ui)));
    });
    let args: Vec<_> = std::env::args()
        .filter(|s| {
            !matches!(
                s.as_str(),
                "--smoke-test" | "--live-smoke-test" | "--crypto-smoke-test" | "--show-media"
            )
        })
        .collect();
    app.run_with_args(&args)
}
thread_local! {static ROOT:std::cell::RefCell<Option<std::rc::Rc<ui::Ui>>>=const{std::cell::RefCell::new(None)};}
