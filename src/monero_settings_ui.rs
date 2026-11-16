use crate::ui::Ui;
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;
use zeroize::Zeroizing;
pub use crate::monero_settings::{Settings,validate};
pub fn add(ui: &Rc<Ui>, dialog: &adw::PreferencesDialog) { dialog.add(&page(ui)); }
pub(crate) fn page(ui: &Rc<Ui>) -> adw::PreferencesPage {
    let owner = ui.state.borrow().account.clone();
    let value: Settings =
        serde_json::from_str(&ui.store.pref(&format!("monero-settings:{owner}"), "{}"))
            .unwrap_or_default();
    let page = adw::PreferencesPage::builder()
        .title("Monero")
        .icon_name("monogram-monero-symbolic")
        .build();
    let group=adw::PreferencesGroup::builder().title("Monero").build();
    let enabled = adw::SwitchRow::builder()
        .title("Enable Monero")
        .active(value.enabled)
        .sensitive(!owner.is_empty())
        .build();
    let network = adw::ComboRow::builder()
        .title("Network")
        .model(&gtk::StringList::new(&["Mainnet", "Stagenet", "Testnet"]))
        .selected(match value.network.as_str() {
            "STAGENET" => 1,
            "TESTNET" => 2,
            _ => 0,
        })
        .build();
    let privacy = adw::SwitchRow::builder()
        .title("Require encrypted payment messages")
        .active(value.require_encryption)
        .build();
    group.add(&enabled);
    group.add(&network);
    let payments = adw::PreferencesGroup::builder().title("Payments").build();
    payments.add(&privacy);
    page.add(&group);
    let node = adw::PreferencesGroup::builder()
        .title("Monero Node")
        .description("Choose a public node or enter your own HTTPS address, just as on your phone.")
        .build();
    let endpoint = adw::EntryRow::builder()
        .title("HTTPS address")
        .text(value.daemon)
        .build();
    let username = adw::EntryRow::builder()
        .title("Username")
        .text(value.username)
        .build();
    let password = adw::PasswordEntryRow::builder()
        .title("Password · leave empty to keep saved password")
        .build();
    const NODES: &[(&str, &str)] = &[
        ("Cake Wallet", "https://xmr-node.cakewallet.com:18081"),
        ("Seth For Privacy", "https://node.sethforprivacy.com"),
        ("Monero.one · Europe", "https://eu.node.monero.one"),
        ("Monero.one · United States", "https://us.node.monero.one"),
    ];
    let choices = gtk::StringList::new(&[]);
    let populate = move |network: u32, choices: &gtk::StringList| {
        let mut names = vec!["Not configured"];
        if network == 0 { names.extend(NODES.iter().map(|(name, _)| *name)); }
        names.push("Custom HTTPS node");
        choices.splice(0, choices.n_items(), &names);
    };
    populate(network.selected(), &choices);
    let selected = if endpoint.text().is_empty() { 0 } else if network.selected() == 0 {
        NODES.iter().position(|(_, url)| *url == endpoint.text()).map(|i| i as u32 + 1).unwrap_or(5)
    } else { 1 };
    let picker = adw::ComboRow::builder().title("Node").model(&choices).selected(selected).build();
    node.add(&picker);
    let custom = adw::PreferencesGroup::builder().title("Custom Node").build();
    custom.add(&endpoint);
    custom.add(&username);
    custom.add(&password);
    custom.set_visible(selected == choices.n_items()-1);
    let custom_ref = custom.clone(); let entries = choices.clone();
    picker.connect_selected_notify(move |row| custom_ref.set_visible(row.selected() == entries.n_items()-1));
    let entries = choices.clone(); let node_picker = picker.clone();
    network.connect_selected_notify(move |row| { populate(row.selected(), &entries); node_picker.set_selected(entries.n_items()-1); });
    let secret = password.clone();
    page.connect_unmap(move |_| secret.set_text(""));
    page.add(&node);
    page.add(&custom);
    page.add(&payments);
    let save = gtk::Button::with_label("Apply");
    save.add_css_class("suggested-action");
    save.set_sensitive(!owner.is_empty());
    let status = gtk::Label::new(Some(if owner.is_empty() {
        "Sign in to configure Monero."
    } else {
        ""
    }));
    status.set_wrap(true);
    status.set_xalign(0.);
    group.set_header_suffix(Some(&save));
    status.set_visible(owner.is_empty());
    let feedback = adw::PreferencesGroup::new();
    feedback.add(&status);
    page.add(&feedback);
    let wallets=adw::PreferencesGroup::builder().title("Synced Wallets").description("Encrypted recovery data is kept in Device Sync. Spending from the desktop wallet is not available yet.").build();
    let records = crate::lan_sync::public_records(&ui.store, &format!("synced-wallet:{owner}:"));
    wallets.set_visible(!records.is_empty());
    for value in records {
        wallets.add(
            &adw::ActionRow::builder()
                .title(value["name"].as_str().unwrap_or("Wallet"))
                .subtitle(format!(
                    "{} · {}",
                    value["network"].as_str().unwrap_or(""),
                    value["address"].as_str().unwrap_or("")
                ))
                .use_markup(false)
                .subtitle_selectable(true)
                .build(),
        );
    }
    page.add(&wallets);
    let weak = Rc::downgrade(ui);
    let generation = ui.generation.get();
    let parent = page.downgrade();
    save.connect_clicked(move |button| {
        let Some(ui) = weak
            .upgrade()
            .filter(|ui| ui.generation.get() == generation && ui.state.borrow().account == owner)
        else {
            return;
        };
        let value = Settings {
            enabled: enabled.is_active(),
            network: ["MAINNET", "STAGENET", "TESTNET"][network.selected().min(2) as usize].into(),
            daemon: if picker.selected() == 0 { String::new() }
                else if network.selected() == 0 && picker.selected() <= 4 { NODES[picker.selected() as usize-1].1.into() }
                else { endpoint.text().trim().into() },
            username: username.text().trim().into(),
            require_encryption: privacy.is_active(),
        };
        status.set_visible(true);
        if let Err(error) = validate(&value) {
            status.set_text(error);
            return;
        }
        let secret = Zeroizing::new(password.text().to_string());
        password.set_text("");
        button.set_sensitive(false);
        status.set_text("Saving…");
        let owner = owner.clone();
        let status = status.clone();
        let button = button.clone();
        let ui = Rc::downgrade(&ui);
        let parent = parent.clone();
        glib::MainContext::default().spawn_local(async move {
            if !secret.is_empty() {
                if let Err(error) =
                    crate::credentials::save_service(&owner, "monero-daemon", secret).await
                {
                    status.set_text(&error.to_string());
                    button.set_sensitive(true);
                    return;
                }
            }
            let Some(ui) = ui.upgrade().filter(|ui| {
                ui.generation.get() == generation
                    && ui.state.borrow().account == owner
                    && parent.upgrade().is_some()
            }) else {
                return;
            };
            let result = ui.store.set_pref(
                &format!("monero-settings:{owner}"),
                &serde_json::to_string(&value).unwrap(),
            );
            status.set_text(if result.is_ok() {
                "Saved."
            } else {
                "Could not save Monero settings."
            });
            button.set_sensitive(true);
            if result.is_ok() { ui.refresh_monero_sidebar(); }
        });
    });
    page
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore="requires private GTK display"]
    fn configured_account_shows_monero_tab_and_account_switch_hides_it() {
        let app=adw::Application::builder().application_id("com.negzero.MoneroTabTest").flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();app.register(gtk::gio::Cancellable::NONE).unwrap();let ui=Ui::new(&app,true);
        let owner="wallet@fixture.invalid";ui.state.borrow_mut().account=owner.into();ui.refresh_monero_sidebar();assert!(!ui.media_toggles.borrow().contains_key("monero"));
        let settings=Settings{enabled:true,daemon:"https://node.example".into(),..Default::default()};ui.store.set_pref(&format!("monero-settings:{owner}"),&serde_json::to_string(&settings).unwrap()).unwrap();ui.refresh_monero_sidebar();assert!(ui.media_toggles.borrow()["monero"].is_enabled());
        ui.store.set_pref(&format!("synced-wallet:{owner}:fixture"),r#"{"name":"Main","network":"MAINNET","address":"fixture-receiving-address"}"#).unwrap();ui.refresh_monero_sidebar();assert!(!ui.sidebar_cards.borrow().is_empty(),"synced wallets use the real 3D card widget");
        ui.store.set_pref(&format!("synced-wallet:{owner}:second"),r#"{"name":"Savings","network":"MAINNET","address":"second-fixture-address"}"#).unwrap();ui.refresh_monero_sidebar();assert_eq!(ui.sidebar_cards.borrow().len(),2,"all cards retain hover/resize handlers");
        assert!(ui.media_toggles.borrow()["monero"].icon_name().is_none());
        assert!(!ui.media_stack.child_by_name("monero").unwrap().is::<adw::PreferencesPage>());
        ui.media_tabs.set_active_name(Some("monero"));assert_eq!(ui.media_stack.visible_child_name().as_deref(),Some("monero"));ui.refresh_monero_sidebar();assert_eq!(ui.media_stack.visible_child_name().as_deref(),Some("monero"));
        ui.state.borrow_mut().account="other@fixture.invalid".into();ui.refresh_monero_sidebar();assert!(!ui.media_toggles.borrow().contains_key("monero"));assert!(ui.media_stack.child_by_name("monero").is_none());assert!(ui.sidebar_cards.borrow().is_empty());ui.window.close();
    }
    #[test]
    fn node_requires_https_and_separate_credentials() {
        let mut s = Settings {
            network: "MAINNET".into(),
            daemon: "https://node.example:18081".into(),
            ..Default::default()
        };
        assert!(validate(&s).is_ok());
        for url in [
            "http://node.example",
            "https://user:pass@node.example",
            "https://node.example/path",
            "https://node.example/?token=secret",
        ] {
            s.daemon = url.into();
            assert!(validate(&s).is_err());
        }
    }
}

pub(crate) fn wallet_sidebar(ui: &Rc<Ui>) -> gtk::Widget {
    let body = crate::widgets::bx(true, 16);
    crate::widgets::margins(&body, 12);
    let owner = ui.state.borrow().account.clone();
    let records = crate::lan_sync::public_records(&ui.store, &format!("synced-wallet:{owner}:"));
    ui.sidebar_cards.borrow_mut().clear();
    for value in &records {
        let name = value["name"].as_str().unwrap_or("Wallet");
        let address = value["address"].as_str().unwrap_or("").to_owned();
        let weak = Rc::downgrade(ui);
        let receiving = address.clone();
        let card = crate::wallet::WalletCard::new(name, "—", false, move || {
            if let Some(ui) = weak.upgrade() { receiving_address(&ui, &receiving); }
        });
        body.append(&card.root);
        ui.sidebar_cards.borrow_mut().push(card);
        body.append(&adw::ActionRow::builder().title("Receiving address")
            .subtitle(&address).subtitle_selectable(true).use_markup(false).build());
    }
    if records.is_empty() {
        body.append(&adw::StatusPage::builder().icon_name("wallet-symbolic")
            .title("No Wallets").description("Pair your phone to sync a wallet.").build());
    }
    body.append(&crate::widgets::wrap(
        "Balance and spending are not available on desktop yet.", "caption dim-label"));
    let settings = gtk::Button::with_label("Wallet Settings");
    let weak = Rc::downgrade(ui);
    settings.connect_clicked(move |_| { if let Some(ui) = weak.upgrade() { ui.preferences(); } });
    body.append(&settings);
    crate::widgets::scroll(&body).upcast()
}

fn receiving_address(ui: &Rc<Ui>, address: &str) {
    let body = crate::widgets::bx(true, 12);
    if let Ok(code) = qrcode::QrCode::new(address.as_bytes()) {
        let n = code.width();
        let colors = code.to_colors();
        let area = gtk::DrawingArea::builder().content_width(256).content_height(256)
            .halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        area.set_draw_func(move |_, c, w, h| {
            let size = w.min(h) as f64;
            let scale = size / (n + 8) as f64;
            c.translate((w as f64 - size) / 2., (h as f64 - size) / 2.);
            c.set_source_rgb(1., 1., 1.);
            c.rectangle(0., 0., size, size);
            let _ = c.fill();
            c.set_source_rgb(0., 0., 0.);
            for y in 0..n { for x in 0..n {
                if colors[y * n + x] == qrcode::Color::Dark {
                    c.rectangle((x + 4) as f64 * scale, (y + 4) as f64 * scale, scale, scale);
                }
            }}
            let _ = c.fill();
        });
        body.append(&area);
    }
    body.append(&gtk::Label::builder().label(address).selectable(true).wrap(true).build());
    ui.dialog("Receiving Address", &body, 360, 380);
}
