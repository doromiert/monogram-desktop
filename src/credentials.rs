//! XMPP login secrets are stored only in the session's Secret Service.
use gtk::gio;
use secret_service::{EncryptionType, Item, SecretService};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const APP: &str = "com.negzero.MonogramDesktop";
const PURPOSE: &str = "XMPP login password v1";
const CONTENT_TYPE: &str = "text/plain; charset=utf-8";
static OPERATIONS: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Missing,
    Locked,
    Unavailable,
    Invalid,
    Ambiguous,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "No password is saved for this account.",
            Self::Locked => "The system keyring is locked.",
            Self::Unavailable => "The system keyring could not complete the request.",
            Self::Invalid => "The XMPP account or its password is invalid.",
            Self::Ambiguous => "More than one keyring entry matches this account.",
        })
    }
}

impl std::error::Error for Error {}

impl From<secret_service::Error> for Error {
    fn from(error: secret_service::Error) -> Self {
        match error {
            secret_service::Error::Locked => Self::Locked,
            _ => Self::Unavailable,
        }
    }
}

type Password = Zeroizing<String>;

enum Operation {
    Load,
    Store(Password),
    Remove,
}

pub async fn load(account: &str) -> Result<Password, Error> {
    run(account, Operation::Load).await?.ok_or(Error::Missing)
}

pub async fn store(account: &str, password: Password) -> Result<(), Error> {
    if password.is_empty() {
        return Err(Error::Invalid);
    }
    run(account, Operation::Store(password)).await.map(|_| ())
}

/// No entry is already the desired result. A locked entry is left untouched.
pub async fn remove(account: &str) -> Result<(), Error> {
    run(account, Operation::Remove).await.map(|_| ())
}

async fn run(account: &str, operation: Operation) -> Result<Option<Password>, Error> {
    if crate::protocol::bare(account).ok().as_deref() != Some(account) {
        return Err(Error::Invalid);
    }
    let account = account.to_owned();
    let guard = OPERATIONS.clone().lock_owned().await;
    gio::spawn_blocking(move || {
        // Keep writes ordered even when the GTK request that launched one has gone away.
        let _guard = guard;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Unavailable)?
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), execute(&account, operation))
                    .await
                    .map_err(|_| Error::Unavailable)?
            })
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APP),
        ("purpose", PURPOSE),
        ("account", account),
    ])
}

async fn matching_item<'a>(
    service: &'a SecretService<'_>,
    attrs: &HashMap<&str, &str>,
) -> Result<Option<Item<'a>>, Error> {
    let found = service.search_items(attrs.clone()).await?;
    let mut matching = None;
    for item in found.unlocked.into_iter().chain(found.locked) {
        let item_attrs = item.get_attributes().await?;
        // SearchItems matches subsets. Never update or delete a different schema's item.
        if item_attrs.len() != attrs.len()
            || !attrs
                .iter()
                .all(|(key, value)| item_attrs.get(*key).map(String::as_str) == Some(*value))
        {
            continue;
        }
        if matching.replace(item).is_some() {
            return Err(Error::Ambiguous);
        }
    }
    if let Some(item) = matching.as_ref() {
        item.ensure_unlocked().await?;
    }
    Ok(matching)
}

async fn execute(account: &str, operation: Operation) -> Result<Option<Password>, Error> {
    let service = SecretService::connect(EncryptionType::Dh).await?;
    let attrs = attributes(account);
    let item = matching_item(&service, &attrs).await?;
    match operation {
        Operation::Load => {
            let Some(item) = item else {
                return Ok(None);
            };
            let bytes = Zeroizing::new(item.get_secret().await?);
            let password = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
            if password.is_empty() {
                return Err(Error::Invalid);
            }
            return Ok(Some(Zeroizing::new(password.to_owned())));
        }
        Operation::Store(password) => {
            if let Some(item) = item {
                item.set_secret(password.as_bytes(), CONTENT_TYPE).await?;
            } else {
                let collection = service.get_default_collection().await?;
                collection.ensure_unlocked().await?;
                collection
                    .create_item(
                        &format!("Monogram XMPP login · {account}"),
                        attrs,
                        password.as_bytes(),
                        false,
                        CONTENT_TYPE,
                    )
                    .await?;
            }
        }
        Operation::Remove => {
            if let Some(item) = item {
                item.delete().await?;
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the disposable Secret Service fixture"]
    fn isolated_password_roundtrip_scope_and_lock() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let ctx = gtk::glib::MainContext::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let alice = "login-alice@example.invalid";
        let bob = "login-bob@example.invalid";
        let mut foreign = attributes(alice);
        foreign.insert("other-schema", "fixture");
        rt.block_on(async {
            let service = SecretService::connect(EncryptionType::Dh).await.unwrap();
            service
                .get_default_collection()
                .await
                .unwrap()
                .create_item(
                    "Another fictional schema",
                    foreign.clone(),
                    b"unchanged",
                    false,
                    CONTENT_TYPE,
                )
                .await
                .unwrap();
        });
        ctx.block_on(async {
            assert_eq!(load(alice).await, Err(Error::Missing));
            store(alice, Zeroizing::new("fixture password ☃".into()))
                .await
                .unwrap();
            store(bob, Zeroizing::new("another login".into()))
                .await
                .unwrap();
            assert_eq!(load(alice).await.unwrap().as_str(), "fixture password ☃");
            store(alice, Zeroizing::new("replacement".into()))
                .await
                .unwrap();
            assert_eq!(load(alice).await.unwrap().as_str(), "replacement");
            remove(alice).await.unwrap();
            assert_eq!(load(alice).await, Err(Error::Missing));
            assert_eq!(load(bob).await.unwrap().as_str(), "another login");
        });
        rt.block_on(async {
            let service = SecretService::connect(EncryptionType::Dh).await.unwrap();
            let item = service
                .search_items(foreign)
                .await
                .unwrap()
                .unlocked
                .pop()
                .unwrap();
            assert_eq!(item.get_secret().await.unwrap(), b"unchanged");
            service
                .get_default_collection()
                .await
                .unwrap()
                .lock()
                .await
                .unwrap();
        });
        ctx.block_on(async {
            assert_eq!(load(bob).await, Err(Error::Locked));
            assert_eq!(remove(bob).await, Err(Error::Locked));
        });
    }
}
