//! Portable Monero preferences, also validated when importing a device sync record.
use serde::{Deserialize, Serialize};
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
