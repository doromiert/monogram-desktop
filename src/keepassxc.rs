//! KeePassXC's native messaging protocol; secrets never cross an unencrypted channel.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use crypto_box::{
    PublicKey, SalsaBox, SecretKey,
    aead::{Aead, AeadCore, OsRng},
};
use gtk::gio::{self, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use zeroize::Zeroizing;

const SERVICE: &str = "keepassxc-association";
const LIMIT: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, &'static str>;
#[derive(Serialize, Deserialize)]
struct Association {
    id: String,
    key: String,
    hash: String,
}
pub enum Action {
    Fill,
    Save(Zeroizing<String>),
}
pub struct Login {
    pub password: Zeroizing<String>,
}
struct Channel {
    _child: Child,
    input: ChildStdin,
    output: ChildStdout,
    client: String,
    public: String,
    cipher: Option<SalsaBox>,
}
fn nonce() -> [u8; 24] {
    SalsaBox::generate_nonce(&mut OsRng).into()
}
fn increment(mut nonce: [u8; 24]) -> [u8; 24] {
    for byte in &mut nonce {
        let (next, carry) = byte.overflowing_add(1);
        *byte = next;
        if !carry {
            break;
        }
    }
    nonce
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("KeePassXC returned an incomplete response.")
}
fn error(value: &Value) -> &'static str {
    match value["errorCode"].as_str() {
        Some("1" | "2") => "Unlock your KeePassXC database, then try again.",
        Some("6") => "The request was cancelled or denied in KeePassXC.",
        Some("8") => "KeePassXC authorization was removed. Disconnect and reconnect below.",
        Some("15") => "No matching entry. Save an entry with your XMPP address as its username.",
        _ => "KeePassXC could not complete the request. Check that browser integration is enabled.",
    }
}
impl Channel {
    async fn open() -> Result<Self> {
        let mut child = Command::new("keepassxc-proxy")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(
                |_| "Install KeePassXC with keepassxc-proxy and enable browser integration.",
            )?;
        let secret = SecretKey::generate(&mut OsRng);
        let public = B64.encode(secret.public_key().as_bytes());
        let mut channel = Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            _child: child,
            client: B64.encode(nonce()),
            public: public.clone(),
            cipher: None,
        };
        let n = nonce();
        let reply = channel
            .exchange(json!({"action":"change-public-keys","publicKey":public,
            "nonce":B64.encode(n),"clientID":channel.client}))
            .await?;
        if reply["success"] != "true" || reply["nonce"] != B64.encode(increment(n)) {
            return Err(error(&reply));
        }
        let public: [u8; 32] = B64
            .decode(string(&reply, "publicKey")?)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or("KeePassXC returned an invalid encryption key.")?;
        channel.cipher = Some(SalsaBox::new(&PublicKey::from(public), &secret));
        Ok(channel)
    }
    async fn exchange(&mut self, request: Value) -> Result<Value> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(&request).map_err(|_| "Could not encode KeePassXC request.")?,
        );
        self.input
            .write_u32_le(bytes.len() as u32)
            .await
            .map_err(|_| "KeePassXC disconnected.")?;
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| "KeePassXC disconnected.")?;
        self.input
            .flush()
            .await
            .map_err(|_| "KeePassXC disconnected.")?;
        for _ in 0..8 {
            let len =
                self.output.read_u32_le().await.map_err(
                    |_| "KeePassXC disconnected. Open it and enable browser integration.",
                )? as usize;
            if len == 0 || len > LIMIT {
                return Err("KeePassXC response exceeded the safe size limit.");
            }
            let mut bytes = Zeroizing::new(vec![0; len]);
            self.output
                .read_exact(&mut bytes)
                .await
                .map_err(|_| "KeePassXC disconnected.")?;
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid KeePassXC response.")?;
            if matches!(
                value["action"].as_str(),
                Some("database-locked" | "database-unlocked")
            ) {
                continue;
            }
            if value["action"] != request["action"] {
                return Err("Unexpected KeePassXC response.");
            }
            return Ok(value);
        }
        Err("KeePassXC sent too many unexpected notifications.")
    }
    async fn request(&mut self, action: &str, mut data: Value) -> Result<Value> {
        let n = nonce();
        let expected = B64.encode(increment(n));
        data["action"] = action.into();
        data["nonce"] = B64.encode(n).into();
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&data).map_err(|_| "Invalid KeePassXC request.")?);
        let encrypted = self
            .cipher
            .as_ref()
            .unwrap()
            .encrypt((&n).into(), plaintext.as_slice())
            .map_err(|_| "KeePassXC encryption failed.")?;
        let reply=self.exchange(json!({"action":action,"message":B64.encode(encrypted),"nonce":B64.encode(n),"clientID":self.client})).await?;
        if reply.get("errorCode").is_some() {
            return Err(error(&reply));
        }
        if reply["nonce"] != expected {
            return Err("KeePassXC response verification failed.");
        }
        let encrypted = B64
            .decode(string(&reply, "message")?)
            .map_err(|_| "Invalid KeePassXC ciphertext.")?;
        let plaintext = Zeroizing::new(
            self.cipher
                .as_ref()
                .unwrap()
                .decrypt((&increment(n)).into(), encrypted.as_slice())
                .map_err(|_| "KeePassXC response verification failed.")?,
        );
        let value: Value =
            serde_json::from_slice(&plaintext).map_err(|_| "Invalid KeePassXC response.")?;
        if value["success"] != "true" || value["nonce"] != expected {
            return Err("KeePassXC response verification failed.");
        }
        if action == "set-login" && value["error"] == "error" {
            return Err(
                "KeePassXC did not save the entry. Check its approval prompt and try again.",
            );
        }
        Ok(value)
    }
}
fn url(jid: &str) -> Result<String> {
    let (_, domain) = jid
        .split_once('@')
        .ok_or("Enter your full XMPP address first.")?;
    if domain.is_empty() || jid.contains('/') || jid.chars().any(char::is_whitespace) {
        return Err("Enter your full XMPP address first.");
    }
    Ok(format!("xmpp://{domain}"))
}
fn matching<'a>(entries: &'a Value, jid: &str) -> Result<Option<&'a Value>> {
    let local = jid.split_once('@').map(|(local, _)| local).unwrap_or("");
    let found: Vec<_> = entries
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["expired"] != "true"
                && entry["expired"] != true
                && entry["login"]
                    .as_str()
                    .is_some_and(|login| login == jid || login == local)
        })
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => Err(
            "Multiple entries match. Give the desired entry a unique XMPP URL and username in KeePassXC.",
        ),
    }
}
async fn operation(jid: &str, action: Action) -> Result<Option<Login>> {
    let url = url(jid)?;
    let mut channel = Channel::open().await?;
    let hash = string(
        &channel.request("get-databasehash", json!({})).await?,
        "hash",
    )?
    .to_owned();
    let saved = crate::credentials::load_service(jid, SERVICE)
        .await
        .map_err(|_| "Unlock your system keyring to remember the KeePassXC connection.")?;
    let association: Association = if let Some(saved) = saved {
        let saved: Association = serde_json::from_str(&saved)
            .map_err(|_| "Disconnect and reconnect KeePassXC below.")?;
        if saved.hash != hash {
            return Err(
                "Open the previously connected KeePassXC database, or disconnect and reconnect below.",
            );
        }
        let reply = channel
            .request("test-associate", json!({"id":saved.id,"key":saved.key}))
            .await?;
        if reply["hash"] != hash || reply["id"] != saved.id {
            return Err("KeePassXC association verification failed.");
        }
        saved
    } else {
        let key = B64.encode(SecretKey::generate(&mut OsRng).public_key().as_bytes());
        let reply = channel
            .request("associate", json!({"key":channel.public,"idKey":key}))
            .await?;
        if reply["hash"] != hash {
            return Err("KeePassXC database changed during authorization.");
        }
        let saved = Association {
            id: string(&reply, "id")?.into(),
            key,
            hash: hash.clone(),
        };
        crate::credentials::save_service(
            jid,
            SERVICE,
            Zeroizing::new(serde_json::to_string(&saved).unwrap()),
        )
        .await
        .map_err(|_| "Could not remember the KeePassXC connection in your system keyring.")?;
        saved
    };
    let logins = channel
        .request(
            "get-logins",
            json!({"url":url,"submitUrl":url,
        "keys":[{"id":association.id,"key":association.key}]}),
        )
        .await;
    let entries = match logins {
        Ok(value) => {
            if value["hash"] != hash {
                return Err("KeePassXC database changed during the request.");
            }
            value
        }
        Err("No matching entry. Save an entry with your XMPP address as its username.") => {
            json!({"entries":[]})
        }
        Err(e) => return Err(e),
    };
    let entry = matching(&entries["entries"], jid)?;
    match action {
        Action::Fill => {
            let password = entry
                .and_then(|e| e["password"].as_str())
                .filter(|p| !p.is_empty())
                .ok_or(
                    "No matching entry. Save an entry with your XMPP address as its username.",
                )?;
            if password.len() > 65536 {
                return Err("KeePassXC password is too large.");
            }
            Ok(Some(Login {
                password: Zeroizing::new(password.into()),
            }))
        }
        Action::Save(password) => {
            if password.is_empty() || password.len() > 65536 {
                return Err("Enter a password to save first.");
            }
            let mut request = json!({"url":url,"submitUrl":url,"id":association.id,"login":jid,
                "password":password.as_str(),"downloadFavicon":"false"});
            if let Some(entry) = entry {
                request["uuid"] = string(entry, "uuid")?.into();
            }
            let response = channel.request("set-login", request).await?;
            if response["hash"] != hash {
                return Err("KeePassXC database changed during the request.");
            }
            Ok(None)
        }
    }
}
pub async fn run(jid: String, action: Action, cancel: gio::Cancellable) -> Result<Option<Login>> {
    gio::spawn_blocking(move || {
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| "Could not start KeePassXC connection.")?;
        runtime.block_on(async move {
            tokio::select! {
                result=tokio::time::timeout(Duration::from_secs(90),operation(&jid,action)) => result.unwrap_or(Err("KeePassXC timed out. Unlock its database and try again.")),
                _=async {loop {if cancel.is_cancelled(){break;}tokio::time::sleep(Duration::from_millis(50)).await;}} => Err("KeePassXC request cancelled."),
            }
        })
    }).await.map_err(|_| "KeePassXC connection failed.")?
}
pub async fn disconnect(jid: &str) -> Result<()> {
    crate::credentials::remove_service(jid, SERVICE)
        .await
        .map_err(|_| "Could not remove KeePassXC authorization from the system keyring.")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_scope_nonce_and_account_matching() {
        assert!(error(&json!({"errorCode":"6"})).contains("cancelled"));
        assert!(error(&json!({"errorCode":"8"})).contains("authorization was removed"));
        assert_eq!(url("alice@example.org").unwrap(), "xmpp://example.org");
        assert!(url("alice").is_err());
        assert!(url("alice@example.org/resource").is_err());
        let mut n = [0; 24];
        n[0] = 255;
        assert_eq!(&increment(n)[..3], &[0, 1, 0]);
        let entries = json!([{"login":"bob","password":"wrong"},{"login":"alice@example.org","password":"right"}]);
        assert_eq!(
            matching(&entries, "alice@example.org").unwrap().unwrap()["password"],
            "right"
        );
        assert!(matching(&entries, "carol@example.org").unwrap().is_none());
        assert!(
            matching(
                &json!([{"login":"alice"},{"login":"alice@example.org"}]),
                "alice@example.org"
            )
            .is_err()
        );
    }
}
#[cfg(test)]
mod native_test {
    use super::*;
    #[test]
    #[ignore = "private libsodium proxy and Secret Service fixture"]
    fn protocol_fixture_roundtrip() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let context = gtk::glib::MainContext::default();
        context.block_on(async {
            let jid = "alice@example.invalid".to_owned();
            let fill = run(jid.clone(), Action::Fill, gio::Cancellable::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fill.password.as_str(), "fictional login");
            run(
                jid.clone(),
                Action::Save(Zeroizing::new("replacement login".into())),
                gio::Cancellable::new(),
            )
            .await
            .unwrap();
            let fill = run(jid.clone(), Action::Fill, gio::Cancellable::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fill.password.as_str(), "replacement login");
            disconnect(&jid).await.unwrap();
        });
    }
}

#[cfg(test)]
mod rejection_tests {
    use super::*;
    #[test]
    #[ignore = "private tampered libsodium proxy fixture"]
    fn rejects_unverified_response() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        gtk::glib::MainContext::default().block_on(async {
            let result = run(
                "alice@example.invalid".into(),
                Action::Fill,
                gio::Cancellable::new(),
            )
            .await;
            assert!(matches!(
                result,
                Err("KeePassXC response verification failed.")
            ));
        });
    }
    #[test]
    #[ignore = "private slow libsodium proxy fixture"]
    fn cancels_pending_proxy_without_blocking() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        gtk::glib::MainContext::default().block_on(async {
            let cancel = gio::Cancellable::new();
            let later = cancel.clone();
            let begin = std::time::Instant::now();
            gtk::glib::MainContext::default().spawn_local(async move {
                gtk::glib::timeout_future(Duration::from_millis(50)).await;
                later.cancel();
            });
            let result = run("alice@example.invalid".into(), Action::Fill, cancel).await;
            assert!(matches!(result, Err("KeePassXC request cancelled.")));
            assert!(begin.elapsed() < Duration::from_millis(400));
        });
    }
}
