use adw::prelude::*;
mod dialogs;
mod folders;
mod media;
mod messages;
mod model;
mod protocol;
mod smoke;
mod storage;
mod transport;
mod ui;
mod wallet;
mod widgets;
fn main() -> gtk::glib::ExitCode {
    let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let app = adw::Application::builder()
        .application_id("com.negzero.MonogramDesktop")
        .flags(
            if std::env::args().any(|s| matches!(s.as_str(), "--smoke-test" | "--live-smoke-test"))
            {
                gtk::gio::ApplicationFlags::NON_UNIQUE
            } else {
                gtk::gio::ApplicationFlags::empty()
            },
        )
        .build();
    app.connect_activate(|app| {
        let ui = ui::Ui::new(
            app,
            std::env::args().any(|s| matches!(s.as_str(), "--smoke-test" | "--live-smoke-test")),
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
        if std::env::args().any(|s| s == "--live-smoke-test") {
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
                "--smoke-test" | "--live-smoke-test" | "--show-media"
            )
        })
        .collect();
    app.run_with_args(&args)
}
thread_local! {static ROOT:std::cell::RefCell<Option<std::rc::Rc<ui::Ui>>>=const{std::cell::RefCell::new(None)};}
