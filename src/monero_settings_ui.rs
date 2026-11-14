use crate::ui::Ui;
use adw::prelude::*;
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use zeroize::Zeroizing;
#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub daemon: String,
    #[serde(default)]
    pub username: String,
    #[serde(default = "encrypted")]
    pub require_encryption: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            network: "MAINNET".into(),
            daemon: String::new(),
            username: String::new(),
            require_encryption: true,
        }
    }
}
fn encrypted() -> bool {
    true
}
pub fn validate(value: &Settings) -> Result<(), &'static str> {
    if !["MAINNET", "STAGENET", "TESTNET"].contains(&value.network.as_str()) {
        return Err("Choose a valid Monero network.");
    }
    if !value.daemon.is_empty() {
        let url = reqwest::Url::parse(&value.daemon)
            .map_err(|_| "Enter an HTTPS Monero node address.")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !["", "/"].contains(&url.path())
            || url.port() == Some(0)
        {
            return Err("Use an HTTPS node address without a path, query, or embedded password.");
        }
    }
    if value.username.len() > 256 || value.username.chars().any(char::is_control) {
        return Err("Invalid node username.");
    }
    Ok(())
}
pub fn add(ui: &Rc<Ui>, dialog: &adw::PreferencesDialog) {
    let owner = ui.state.borrow().account.clone();
    let value: Settings =
        serde_json::from_str(&ui.store.pref(&format!("monero-settings:{owner}"), "{}"))
            .unwrap_or_default();
    let page = adw::PreferencesPage::builder()
        .title("Monero")
        .icon_name("wallet-symbolic")
        .build();
    let group=adw::PreferencesGroup::builder().title("Monero").description("Account-specific wallet and payment settings. Private wallet material stays encrypted.").build();
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
    group.add(&privacy);
    page.add(&group);
    let node = adw::PreferencesGroup::builder()
        .title("Remote Node")
        .description("HTTPS protects node traffic. Use a node you trust.")
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
    node.add(&endpoint);
    node.add(&username);
    node.add(&password);
    let secret = password.clone();
    dialog.connect_closed(move |_| secret.set_text(""));
    page.add(&node);
    let save = gtk::Button::with_label("Save Monero Settings");
    save.add_css_class("suggested-action");
    save.set_sensitive(!owner.is_empty());
    let status = gtk::Label::new(Some(if owner.is_empty() {
        "Sign in to configure Monero."
    } else {
        ""
    }));
    status.set_wrap(true);
    status.set_xalign(0.);
    let actions = adw::PreferencesGroup::new();
    actions.add(&save);
    actions.add(&status);
    page.add(&actions);
    let wallets=adw::PreferencesGroup::builder().title("Synced Wallets").description("Encrypted recovery data is kept in Device Sync. Spending from the desktop wallet is not available yet.").build();
    for value in crate::lan_sync::public_records(&ui.store, &format!("synced-wallet:{owner}:")) {
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
    dialog.add(&page);
    let weak = Rc::downgrade(ui);
    let generation = ui.generation.get();
    let parent = dialog.downgrade();
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
            daemon: endpoint.text().trim().into(),
            username: username.text().trim().into(),
            require_encryption: privacy.is_active(),
        };
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
        });
    });
}
#[cfg(test)]
mod tests {
    use super::*;
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
