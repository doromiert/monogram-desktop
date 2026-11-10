//! Desktop equivalent of the Android Keystore boundary. No file/plaintext fallback.
use super::*;
use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
const APP: &str = "com.negzero.MonogramDesktop";
const PIN: &str = "OMEMO2 local identity binding v1";
pub async fn existing_devices(account: &str) -> Result<Vec<u32>> {
    address(account, 1)?;
    let service = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|_| Error::Vault)?;
    let found = service
        .search_items(HashMap::from([
            ("application", APP),
            ("purpose", "OMEMO2 record encryption v1"),
            ("account", account),
        ]))
        .await
        .map_err(|_| Error::Vault)?;
    let mut devices = vec![];
    for item in found.unlocked.into_iter().chain(found.locked) {
        let attrs = item.get_attributes().await.map_err(|_| Error::Vault)?;
        let id = super::bundle::positive(attrs.get("device").map(String::as_str))?;
        if devices.contains(&id) {
            return Err(Error::Vault);
        }
        devices.push(id);
    }
    Ok(devices)
}
pub async fn identity_pin(
    account: &str,
    device: u32,
    create: Option<&[u8; 32]>,
) -> Result<Option<[u8; 32]>> {
    address(account, device)?;
    let id = device.to_string();
    let service = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|_| Error::Vault)?;
    let attrs = HashMap::from([
        ("application", APP),
        ("purpose", PIN),
        ("account", account),
        ("device", id.as_str()),
    ]);
    let found = service
        .search_items(attrs.clone())
        .await
        .map_err(|_| Error::Vault)?;
    let mut items = found.unlocked;
    items.extend(found.locked);
    if items.len() > 1 {
        return Err(Error::Vault);
    }
    if let Some(item) = items.pop() {
        item.unlock().await.map_err(|_| Error::Vault)?;
        let secret = Secret::new(item.get_secret().await.map_err(|_| Error::Vault)?);
        let pin: [u8; 32] = secret.as_slice().try_into().map_err(|_| Error::Vault)?;
        if create.is_some_and(|expected| expected != &pin) {
            return Err(Error::Incomplete);
        }
        return Ok(Some(pin));
    }
    let Some(pin) = create else { return Ok(None) };
    let collection = service
        .get_default_collection()
        .await
        .map_err(|_| Error::Vault)?;
    collection.unlock().await.map_err(|_| Error::Vault)?;
    collection
        .create_item(
            &format!("Monogram OMEMO 2 identity · {account} · {device}"),
            attrs,
            pin,
            false,
            "application/octet-stream",
        )
        .await
        .map_err(|_| Error::Vault)?;
    Ok(Some(*pin))
}
pub async fn master_key(account: &str, device: u32, create: bool) -> Result<MasterKey> {
    address(account, device)?;
    let service = SecretService::connect(EncryptionType::Dh)
        .await
        .map_err(|_| Error::Vault)?;
    let device = device.to_string();
    let attributes = HashMap::from([
        ("application", "com.negzero.MonogramDesktop"),
        ("purpose", "OMEMO2 record encryption v1"),
        ("account", account),
        ("device", device.as_str()),
    ]);
    let found = service
        .search_items(attributes.clone())
        .await
        .map_err(|_| Error::Vault)?;
    let mut items = found.unlocked;
    items.extend(found.locked);
    if items.len() > 1 {
        return Err(Error::Vault);
    }
    if let Some(item) = items.pop() {
        item.unlock().await.map_err(|_| Error::Vault)?;
        let secret = Secret::new(item.get_secret().await.map_err(|_| Error::Vault)?);
        return secret
            .as_slice()
            .try_into()
            .map(MasterKey::new)
            .map_err(|_| Error::Vault);
    }
    if !create {
        return Err(Error::Vault);
    }
    let collection = service
        .get_default_collection()
        .await
        .map_err(|_| Error::Vault)?;
    collection.unlock().await.map_err(|_| Error::Vault)?;
    let key = MasterKey::new(random::<32>()?);
    collection
        .create_item(
            &format!("Monogram OMEMO 2 · {account} · {device}"),
            attributes,
            key.as_slice(),
            false,
            "application/octet-stream",
        )
        .await
        .map_err(|_| Error::Vault)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires disposable Secret Service and private D-Bus"]
    fn isolated_secret_service() {
        assert_eq!(
            std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),
            Ok("1"),
            "Never run against the desktop keyring"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let owner = "alice@example.invalid";
            assert!(master_key(owner, 101, false).await.is_err());
            let original = master_key(owner, 101, true).await.unwrap();
            assert_eq!(
                master_key(owner, 101, false).await.unwrap().as_slice(),
                original.as_slice()
            );
            assert_eq!(
                master_key(owner, 101, true).await.unwrap().as_slice(),
                original.as_slice()
            );
            assert_ne!(
                master_key(owner, 102, true).await.unwrap().as_slice(),
                original.as_slice()
            );
            assert_ne!(
                master_key("bob@example.invalid", 101, true)
                    .await
                    .unwrap()
                    .as_slice(),
                original.as_slice()
            );
            let service = SecretService::connect(EncryptionType::Dh).await.unwrap();
            let collection = service.get_default_collection().await.unwrap();
            let attrs = |id| {
                HashMap::from([
                    ("application", "com.negzero.MonogramDesktop"),
                    ("purpose", "OMEMO2 record encryption v1"),
                    ("account", owner),
                    ("device", id),
                ])
            };
            collection
                .create_item(
                    "Malformed fictional key",
                    attrs("404"),
                    &[0; 31],
                    false,
                    "application/octet-stream",
                )
                .await
                .unwrap();
            assert!(master_key(owner, 404, false).await.is_err());
            collection
                .create_item(
                    "Duplicate fictional key",
                    attrs("101"),
                    &[0; 32],
                    false,
                    "application/octet-stream",
                )
                .await
                .unwrap();
            assert!(master_key(owner, 101, false).await.is_err());
            assert!(master_key(owner, 101, true).await.is_err());
        });
    }
}
