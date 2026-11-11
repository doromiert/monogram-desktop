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
const ROOM_PURPOSE: &str = "XMPP room password v1";
const INVITATION_PURPOSE: &str = "XMPP room invitation password v1";
const SERVICE_PURPOSE: &str = "Monogram optional provider credential v1";
const CONTENT_TYPE: &str = "text/plain; charset=utf-8";
static OPERATIONS: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Missing,
    Locked,
    Unavailable,
    Invalid,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "No password is saved for this account.",
            Self::Locked => "The system keyring is locked.",
            Self::Unavailable => "The system keyring could not complete the request.",
            Self::Invalid => "The XMPP account or its password is invalid.",
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

pub async fn load_service(account: &str, service: &str) -> Result<Option<Password>, Error> {
    provider(account, service, Operation::Load).await
}
pub async fn save_service(account: &str, service: &str, secret: Password) -> Result<(), Error> {
    if secret.is_empty() || secret.len() > 4096 {
        return Err(Error::Invalid);
    }
    provider(account, service, Operation::Store(secret))
        .await
        .map(|_| ())
}
pub async fn remove_service(account: &str, service: &str) -> Result<(), Error> {
    provider(account, service, Operation::Remove)
        .await
        .map(|_| ())
}
fn service_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
async fn provider(
    account: &str,
    provider: &str,
    operation: Operation,
) -> Result<Option<Password>, Error> {
    if crate::protocol::bare(account).ok().as_deref() != Some(account) || !service_name(provider) {
        return Err(Error::Invalid);
    }
    let account = account.to_owned();
    let provider = provider.to_owned();
    let guard = OPERATIONS.clone().lock_owned().await;
    let work = move || {
        let _guard = guard;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Unavailable)?
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), async {
                    let service = SecretService::connect(EncryptionType::Dh).await?;
                    let mut attrs = attributes(&account);
                    attrs.insert("purpose", SERVICE_PURPOSE);
                    attrs.insert("service", &provider);
                    let items = matching_items(&service, &attrs).await?;
                    match operation {
                        Operation::Load => {
                            if let Some(item) = items.first() {
                                item.ensure_unlocked().await?;
                                let bytes = Zeroizing::new(item.get_secret().await?);
                                let value =
                                    std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
                                if value.is_empty() || value.len() > 4096 {
                                    return Err(Error::Invalid);
                                }
                                return Ok(Some(Zeroizing::new(value.to_owned())));
                            }
                        }
                        Operation::Store(secret) => {
                            if let Some(item) = items.first() {
                                item.ensure_unlocked().await?;
                                item.set_secret(secret.as_bytes(), CONTENT_TYPE).await?;
                            } else {
                                let collection = service.get_default_collection().await?;
                                collection.ensure_unlocked().await?;
                                collection
                                    .create_item(
                                        &format!("Monogram {provider} · {account}"),
                                        attrs,
                                        secret.as_bytes(),
                                        false,
                                        CONTENT_TYPE,
                                    )
                                    .await?;
                            }
                            for duplicate in items.iter().skip(1) {
                                duplicate.ensure_unlocked().await?;
                                duplicate.delete().await?;
                            }
                        }
                        Operation::Remove => {
                            for item in items {
                                item.ensure_unlocked().await?;
                                item.delete().await?;
                            }
                        }
                    }
                    Ok(None)
                })
                .await
                .map_err(|_| Error::Unavailable)?
            })
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|_| Error::Unavailable)?
    } else {
        gio::spawn_blocking(work)
            .await
            .map_err(|_| Error::Unavailable)?
    }
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

pub async fn load_room(account: &str, room: &str) -> Result<Password, Error> {
    run_scoped(account, Some(room), Operation::Load)
        .await?
        .ok_or(Error::Missing)
}

pub async fn store_room(account: &str, room: &str, password: Password) -> Result<(), Error> {
    if password.encode_utf16().count() > 4096 {
        return Err(Error::Invalid);
    }
    run_scoped(account, Some(room), Operation::Store(password))
        .await
        .map(|_| ())
}

pub async fn load_invitation(
    account: &str,
    room: &str,
    secret_id: &str,
) -> Result<Password, Error> {
    run_scope(account, Some(room), Some(secret_id), Operation::Load)
        .await?
        .ok_or(Error::Missing)
}

pub async fn store_invitation(
    account: &str,
    room: &str,
    secret_id: &str,
    password: Password,
) -> Result<(), Error> {
    if password.is_empty() || password.encode_utf16().count() > 4096 {
        return Err(Error::Invalid);
    }
    run_scope(
        account,
        Some(room),
        Some(secret_id),
        Operation::Store(password),
    )
    .await
    .map(|_| ())
}

pub async fn remove_invitation(account: &str, room: &str, secret_id: &str) -> Result<(), Error> {
    run_scope(account, Some(room), Some(secret_id), Operation::Remove)
        .await
        .map(|_| ())
}

async fn run(account: &str, operation: Operation) -> Result<Option<Password>, Error> {
    run_scoped(account, None, operation).await
}

async fn run_scoped(
    account: &str,
    room: Option<&str>,
    operation: Operation,
) -> Result<Option<Password>, Error> {
    run_scope(account, room, None, operation).await
}

async fn run_scope(
    account: &str,
    room: Option<&str>,
    secret_id: Option<&str>,
    operation: Operation,
) -> Result<Option<Password>, Error> {
    if crate::protocol::bare(account).ok().as_deref() != Some(account) {
        return Err(Error::Invalid);
    }
    if room.is_some_and(|room| crate::protocol::bare(room).ok().as_deref() != Some(room)) {
        return Err(Error::Invalid);
    }
    if secret_id.is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        || secret_id.is_some() && room.is_none()
    {
        return Err(Error::Invalid);
    }
    let account = account.to_owned();
    let room = room.map(str::to_owned);
    let secret_id = secret_id.map(str::to_owned);
    let worker_runtime = room.is_some() && tokio::runtime::Handle::try_current().is_ok();
    let guard = OPERATIONS.clone().lock_owned().await;
    let work = move || {
        // Keep writes ordered even when the GTK request that launched one has gone away.
        let _guard = guard;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Unavailable)?
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    execute_scoped(&account, room.as_deref(), secret_id.as_deref(), operation),
                )
                .await
                .map_err(|_| Error::Unavailable)?
            })
    };
    if worker_runtime {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|_| Error::Unavailable)?
    } else {
        gio::spawn_blocking(work)
            .await
            .map_err(|_| Error::Unavailable)?
    }
}

fn attributes(account: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", APP),
        ("purpose", PURPOSE),
        ("account", account),
    ])
}

async fn matching_items<'a>(
    service: &'a SecretService<'_>,
    attrs: &HashMap<&str, &str>,
) -> Result<Vec<Item<'a>>, Error> {
    let found = service.search_items(attrs.clone()).await?;
    let mut matching = Vec::new();
    for item in found.unlocked.into_iter().chain(found.locked) {
        let item_attrs = item.get_attributes().await?;
        // Secret Service may add its generic schema to otherwise exact attributes.
        let platform_schema = item_attrs
            .get("xdg:schema")
            .is_some_and(|schema| schema == "org.freedesktop.Secret.Generic");
        if item_attrs.len() != attrs.len() + usize::from(platform_schema)
            || !attrs
                .iter()
                .all(|(key, value)| item_attrs.get(*key).map(String::as_str) == Some(*value))
        {
            continue;
        }
        matching.push((item.get_modified().await?, item));
    }
    // Earlier versions could create duplicates when the platform added xdg:schema.
    matching.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(matching.into_iter().map(|(_, item)| item).collect())
}

async fn execute_scoped(
    account: &str,
    room: Option<&str>,
    secret_id: Option<&str>,
    operation: Operation,
) -> Result<Option<Password>, Error> {
    let service = SecretService::connect(EncryptionType::Dh).await?;
    let mut attrs = attributes(account);
    if let Some(room) = room {
        attrs.insert("purpose", ROOM_PURPOSE);
        attrs.insert("room", room);
    }
    if let Some(secret_id) = secret_id {
        attrs.insert("purpose", INVITATION_PURPOSE);
        attrs.insert("invitation", secret_id);
    }
    let mut items = matching_items(&service, &attrs).await?;
    match operation {
        Operation::Load => {
            let Some(item) = items.first() else {
                return Ok(None);
            };
            item.ensure_unlocked().await?;
            let bytes = Zeroizing::new(item.get_secret().await?);
            let password = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
            if password.is_empty() && room.is_none() {
                return Err(Error::Invalid);
            }
            return Ok(Some(Zeroizing::new(password.to_owned())));
        }
        Operation::Store(password) => {
            if let Some(item) = items.first() {
                item.ensure_unlocked().await?;
                item.set_secret(password.as_bytes(), CONTENT_TYPE).await?;
            } else {
                let collection = service.get_default_collection().await?;
                collection.ensure_unlocked().await?;
                collection
                    .create_item(
                        &room
                            .map(|room| {
                                if secret_id.is_some() {
                                    format!("Monogram invitation · {account} · {room}")
                                } else {
                                    format!("Monogram room · {account} · {room}")
                                }
                            })
                            .unwrap_or_else(|| format!("Monogram XMPP login · {account}")),
                        attrs,
                        password.as_bytes(),
                        false,
                        CONTENT_TYPE,
                    )
                    .await?;
            }
        }
        Operation::Remove => {
            if room.is_none() {
                let mut query = attributes(account);
                query.insert("purpose", SERVICE_PURPOSE);
                let found = service.search_items(query).await?;
                for item in found.unlocked.into_iter().chain(found.locked) {
                    let values = item.get_attributes().await?;
                    let Some(provider) = values.get("service").filter(|value| service_name(value))
                    else {
                        continue;
                    };
                    let mut exact = attributes(account);
                    exact.insert("purpose", SERVICE_PURPOSE);
                    exact.insert("service", provider);
                    let schema = values
                        .get("xdg:schema")
                        .is_some_and(|v| v == "org.freedesktop.Secret.Generic");
                    if values.len() == exact.len() + usize::from(schema)
                        && exact
                            .iter()
                            .all(|(k, v)| values.get(*k).map(String::as_str) == Some(*v))
                    {
                        item.ensure_unlocked().await?;
                        item.delete().await?;
                    }
                }
                for purpose in [ROOM_PURPOSE, INVITATION_PURPOSE] {
                    let mut query = attributes(account);
                    query.insert("purpose", purpose);
                    let found = service.search_items(query).await?;
                    for item in found.unlocked.into_iter().chain(found.locked) {
                        let values = item.get_attributes().await?;
                        let Some(room) = values.get("room") else {
                            continue;
                        };
                        if crate::protocol::bare(room).ok().as_deref() != Some(room.as_str()) {
                            continue;
                        }
                        let mut exact = attributes(account);
                        exact.insert("purpose", purpose);
                        exact.insert("room", room);
                        if purpose == INVITATION_PURPOSE {
                            let Some(id) = values.get("invitation") else {
                                continue;
                            };
                            if uuid::Uuid::parse_str(id).is_err() {
                                continue;
                            }
                            exact.insert("invitation", id);
                        }
                        let schema = values
                            .get("xdg:schema")
                            .is_some_and(|value| value == "org.freedesktop.Secret.Generic");
                        if values.len() == exact.len() + usize::from(schema)
                            && exact.iter().all(|(key, value)| {
                                values.get(*key).map(String::as_str) == Some(*value)
                            })
                        {
                            items.push(item);
                        }
                    }
                }
            }
            for item in &items {
                item.ensure_unlocked().await?;
            }
            for item in items.drain(..) {
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
    fn isolated_invitation_passwords_are_separate_and_removed_with_account() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1")
        );
        let ctx = gtk::glib::MainContext::new();
        ctx.block_on(async {
            let owner = "invitation-owner@example.invalid";
            let other = "invitation-other@example.invalid";
            let room = "room@conference.example.invalid";
            let first = uuid::Uuid::new_v4().to_string();
            let second = uuid::Uuid::new_v4().to_string();
            store_room(owner, room, Zeroizing::new("joined-room-password".into()))
                .await
                .unwrap();
            store_invitation(
                owner,
                room,
                &first,
                Zeroizing::new("pending-invitation".into()),
            )
            .await
            .unwrap();
            store_invitation(
                owner,
                room,
                &second,
                Zeroizing::new("new-invitation".into()),
            )
            .await
            .unwrap();
            store_invitation(other, room, &first, Zeroizing::new("other-account".into()))
                .await
                .unwrap();
            assert_eq!(
                load_room(owner, room).await.unwrap().as_str(),
                "joined-room-password"
            );
            assert_eq!(
                load_invitation(owner, room, &first).await.unwrap().as_str(),
                "pending-invitation"
            );
            remove_invitation(owner, room, &first).await.unwrap();
            assert_eq!(
                load_invitation(owner, room, &first).await,
                Err(Error::Missing)
            );
            assert_eq!(
                load_invitation(owner, room, &second)
                    .await
                    .unwrap()
                    .as_str(),
                "new-invitation"
            );
            remove(owner).await.unwrap();
            assert_eq!(load_room(owner, room).await, Err(Error::Missing));
            assert_eq!(
                load_invitation(owner, room, &second).await,
                Err(Error::Missing)
            );
            assert_eq!(
                load_invitation(other, room, &first).await.unwrap().as_str(),
                "other-account"
            );
            assert_eq!(
                load_invitation(owner, room, "not-an-id").await,
                Err(Error::Invalid)
            );
            remove(other).await.unwrap();
        });
    }

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
