// SPDX-License-Identifier: AGPL-3.0-only
//! Thin transactional adapter to the exact Monocles libsignal fork used by mobile.
use super::{store::Tx, *};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use pq_signal::{
    self as signal, GenericSignedPreKey, IdentityKeyStore, KyberPreKeyStore, PreKeyStore,
    SessionStore, SignedPreKeyStore,
};
use rand::TryRngCore as _;
use std::time::SystemTime;
type SignalResult<T> = std::result::Result<T, signal::SignalProtocolError>;

pub fn failure(error: signal::SignalProtocolError) -> Error {
    use signal::SignalProtocolError as S;
    match error {
        S::UntrustedIdentity(_) => Error::TrustRequired,
        S::DuplicatedMessage(_, _) => Error::Invalid("PQ duplicate message"),
        S::InvalidPreKeyId | S::InvalidSignedPreKeyId | S::InvalidKyberPreKeyId => {
            Error::Unavailable(
                "This OMEMO 2 message refers to a PQ key that is no longer available",
            )
        }
        S::ApplicationCallbackError(_, _) => {
            Error::Unavailable("PQ encrypted state is unavailable; existing keys were preserved")
        }
        S::SessionNotFound(_) => Error::Unavailable("The OMEMO 2 PQ device session is unavailable"),
        _ => Error::Invalid("PQ ratchet or authentication"),
    }
}
#[derive(Debug)]
struct Callback;
impl std::fmt::Display for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PQ encrypted storage callback failed")
    }
}
impl std::error::Error for Callback {}
fn callback(_: Error) -> signal::SignalProtocolError {
    signal::SignalProtocolError::ApplicationCallbackError("PQStore", Box::new(Callback))
}
pub fn remote(peer: &str, id: u32) -> Result<signal::ProtocolAddress> {
    crate::crypto::address(peer, id)?;
    Ok(signal::ProtocolAddress::new(
        peer.into(),
        id.try_into().map_err(|_| Error::Invalid("PQ device id"))?,
    ))
}
pub fn peer_key(peer: &str, id: u32) -> Result<String> {
    remote(peer, id)?;
    Ok(format!("{}/{id}", URL_SAFE_NO_PAD.encode(peer.as_bytes())))
}
fn key(address: &signal::ProtocolAddress) -> String {
    format!(
        "{}/{}",
        URL_SAFE_NO_PAD.encode(address.name().as_bytes()),
        u32::from(address.device_id())
    )
}
pub fn required(tx: &Tx<'_>, key: &str) -> Result<Secret> {
    tx.get(key)?.ok_or(Error::Incomplete)
}
pub fn identity(tx: &Tx<'_>) -> Result<signal::IdentityKeyPair> {
    signal::IdentityKeyPair::try_from(required(tx, "local/identity")?.as_slice()).map_err(failure)
}
pub fn pq_identity(tx: &Tx<'_>) -> Result<signal::PqIdentityKeyPair> {
    signal::PqIdentityKeyPair::deserialize(&required(tx, "local/pq-identity")?).map_err(failure)
}
pub fn number(bytes: &[u8]) -> Result<u32> {
    bytes
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| Error::Incomplete)
}
pub fn has_session(tx: &Tx<'_>, peer: &str, id: u32) -> Result<bool> {
    Ok(tx
        .get(&format!("session/{}", peer_key(peer, id)?))?
        .is_some())
}
pub fn trust_record(tx: &Tx<'_>, peer: &str, id: u32) -> Result<Option<Secret>> {
    let key = peer_key(peer, id)?;
    if tx.get(&format!("blocked/{key}"))?.is_some() {
        return Ok(None);
    }
    let trusted = tx.get(&format!("trust/{key}"))?;
    if let Some(trusted) = &trusted {
        if trusted.len() != 2625 {
            return Err(Error::Incomplete);
        }
        if tx
            .get(&format!("observed/{key}"))?
            .as_ref()
            .map(|value| value.as_slice())
            != Some(trusted.as_slice())
        {
            return Ok(None);
        }
    }
    Ok(trusted)
}
pub fn authorize(tx: &Tx<'_>, peer: &str, id: u32, classic: &[u8; 33], pq: &[u8]) -> Result<()> {
    if pq.len() != signal::PQ_IDENTITY_PUBLIC_KEY_LEN {
        return Err(Error::Invalid("PQ hybrid identity"));
    }
    let trusted = trust_record(tx, peer, id)?.ok_or(Error::TrustRequired)?;
    if !openssl::memcmp::eq(&trusted[..33], classic) || !openssl::memcmp::eq(&trusted[33..], pq) {
        return Err(Error::TrustRequired);
    }
    Ok(())
}
pub fn sign_auth(tx: &Tx<'_>, transcript: &[u8]) -> Result<(Vec<u8>, Vec<u8>, [u8; 64])> {
    let mut rng = rand::rngs::OsRng.unwrap_err();
    let pq = pq_identity(tx)?;
    let signature = pq
        .sign(transcript, &mut rng)
        .map_err(failure)?
        .serialize()
        .into_vec();
    let classic = identity(tx)?
        .private_key()
        .calculate_signature(transcript, &mut rng)
        .map_err(|_| Error::Invalid("PQ classical signature"))?;
    Ok((
        pq.public_key().serialize().into_vec(),
        signature,
        classic.as_ref().try_into().map_err(|_| Error::Incomplete)?,
    ))
}

#[derive(Clone, Copy)]
pub struct SignalStore<'a, 'db> {
    tx: &'a Tx<'db>,
}
impl<'a, 'db> SignalStore<'a, 'db> {
    pub fn new(tx: &'a Tx<'db>) -> Result<Self> {
        identity(tx)?;
        number(&required(tx, "local/registration")?)?;
        Ok(Self { tx })
    }
}
#[async_trait(?Send)]
impl IdentityKeyStore for SignalStore<'_, '_> {
    async fn get_identity_key_pair(&self) -> SignalResult<signal::IdentityKeyPair> {
        identity(self.tx).map_err(callback)
    }
    async fn get_local_registration_id(&self) -> SignalResult<u32> {
        number(&required(self.tx, "local/registration").map_err(callback)?).map_err(callback)
    }
    async fn save_identity(
        &mut self,
        address: &signal::ProtocolAddress,
        identity: &signal::IdentityKey,
    ) -> SignalResult<signal::IdentityChange> {
        if !self
            .is_trusted_identity(address, identity, signal::Direction::Receiving)
            .await?
        {
            return Err(signal::SignalProtocolError::UntrustedIdentity(
                address.clone(),
            ));
        }
        let key = format!("identity/{}", key(address));
        let old = self.tx.get(&key).map_err(callback)?;
        let new = identity.serialize();
        self.tx.put(&key, Some(&new)).map_err(callback)?;
        Ok(if old.is_some_and(|old| old.as_slice() != new.as_ref()) {
            signal::IdentityChange::ReplacedExisting
        } else {
            signal::IdentityChange::NewOrUnchanged
        })
    }
    async fn is_trusted_identity(
        &self,
        address: &signal::ProtocolAddress,
        identity: &signal::IdentityKey,
        _: signal::Direction,
    ) -> SignalResult<bool> {
        let trusted =
            trust_record(self.tx, address.name(), address.device_id().into()).map_err(callback)?;
        Ok(trusted.is_some_and(|saved| openssl::memcmp::eq(&saved[..33], &identity.serialize())))
    }
    async fn get_identity(
        &self,
        address: &signal::ProtocolAddress,
    ) -> SignalResult<Option<signal::IdentityKey>> {
        self.tx
            .get(&format!("identity/{}", key(address)))
            .map_err(callback)?
            .map(|bytes| signal::IdentityKey::try_from(bytes.as_slice()))
            .transpose()
    }
}
#[async_trait(?Send)]
impl PreKeyStore for SignalStore<'_, '_> {
    async fn get_pre_key(&self, id: signal::PreKeyId) -> SignalResult<signal::PreKeyRecord> {
        let bytes = self
            .tx
            .get(&format!("prekey/{}", u32::from(id)))
            .map_err(callback)?
            .ok_or(signal::SignalProtocolError::InvalidPreKeyId)?;
        signal::PreKeyRecord::deserialize(&bytes)
    }
    async fn save_pre_key(
        &mut self,
        id: signal::PreKeyId,
        record: &signal::PreKeyRecord,
    ) -> SignalResult<()> {
        self.tx
            .put(
                &format!("prekey/{}", u32::from(id)),
                Some(&record.serialize()?),
            )
            .map_err(callback)
    }
    async fn remove_pre_key(&mut self, id: signal::PreKeyId) -> SignalResult<()> {
        self.tx
            .put(&format!("prekey/{}", u32::from(id)), None)
            .map_err(callback)
    }
}
#[async_trait(?Send)]
impl SignedPreKeyStore for SignalStore<'_, '_> {
    async fn get_signed_pre_key(
        &self,
        id: signal::SignedPreKeyId,
    ) -> SignalResult<signal::SignedPreKeyRecord> {
        let bytes = self
            .tx
            .get(&format!("signed/{}", u32::from(id)))
            .map_err(callback)?
            .ok_or(signal::SignalProtocolError::InvalidSignedPreKeyId)?;
        signal::SignedPreKeyRecord::deserialize(&bytes)
    }
    async fn save_signed_pre_key(
        &mut self,
        id: signal::SignedPreKeyId,
        record: &signal::SignedPreKeyRecord,
    ) -> SignalResult<()> {
        self.tx
            .put(
                &format!("signed/{}", u32::from(id)),
                Some(&record.serialize()?),
            )
            .map_err(callback)
    }
}
#[async_trait(?Send)]
impl SessionStore for SignalStore<'_, '_> {
    async fn load_session(
        &self,
        address: &signal::ProtocolAddress,
    ) -> SignalResult<Option<signal::SessionRecord>> {
        self.tx
            .get(&format!("session/{}", key(address)))
            .map_err(callback)?
            .map(|bytes| signal::SessionRecord::deserialize(&bytes))
            .transpose()
    }
    async fn store_session(
        &mut self,
        address: &signal::ProtocolAddress,
        record: &signal::SessionRecord,
    ) -> SignalResult<()> {
        self.tx
            .put(
                &format!("session/{}", key(address)),
                Some(&record.serialize()?),
            )
            .map_err(callback)
    }
}
#[async_trait(?Send)]
impl KyberPreKeyStore for SignalStore<'_, '_> {
    async fn get_kyber_pre_key(
        &self,
        id: signal::KyberPreKeyId,
    ) -> SignalResult<signal::KyberPreKeyRecord> {
        let bytes = self
            .tx
            .get(&format!("kem/{}", u32::from(id)))
            .map_err(callback)?
            .ok_or(signal::SignalProtocolError::InvalidKyberPreKeyId)?;
        signal::KyberPreKeyRecord::deserialize(&bytes)
    }
    async fn save_kyber_pre_key(
        &mut self,
        id: signal::KyberPreKeyId,
        record: &signal::KyberPreKeyRecord,
    ) -> SignalResult<()> {
        self.tx
            .put(
                &format!("kem/{}", u32::from(id)),
                Some(&record.serialize()?),
            )
            .map_err(callback)
    }
    async fn mark_kyber_pre_key_used(
        &mut self,
        id: signal::KyberPreKeyId,
        signed: signal::SignedPreKeyId,
        base: &signal::PublicKey,
    ) -> SignalResult<()> {
        let id = u32::from(id);
        if self
            .tx
            .get(&format!("last-resort/{id}"))
            .map_err(callback)?
            .is_none()
        {
            return self.tx.put(&format!("kem/{id}"), None).map_err(callback);
        }
        let replay = format!(
            "replay/{id}/{}/{}",
            u32::from(signed),
            URL_SAFE_NO_PAD.encode(base.serialize())
        );
        if self.tx.get(&replay).map_err(callback)?.is_some() {
            return Err(signal::SignalProtocolError::InvalidMessage(
                signal::CiphertextMessageType::PreKey,
                "reused base key".into(),
            ));
        }
        self.tx.put(&replay, Some(&[1])).map_err(callback)
    }
}
pub async fn process_bundle(
    tx: &Tx<'_>,
    peer: &str,
    id: u32,
    bundle: &signal::PreKeyBundle,
) -> Result<()> {
    let store = SignalStore::new(tx)?;
    let mut rng = rand::rngs::OsRng.unwrap_err();
    signal::process_prekey_bundle(
        &remote(peer, id)?,
        &remote(tx.owner, tx.device)?,
        &mut store.clone(),
        &mut store.clone(),
        bundle,
        SystemTime::now(),
        &mut rng,
    )
    .await
    .map_err(failure)
}
pub async fn encrypt(tx: &Tx<'_>, peer: &str, id: u32, plain: &[u8]) -> Result<(bool, Vec<u8>)> {
    if plain.len() > 65536 {
        return Err(Error::Invalid("PQ wrapped material size"));
    }
    let store = SignalStore::new(tx)?;
    let mut rng = rand::rngs::OsRng.unwrap_err();
    let result = signal::message_encrypt(
        plain,
        &remote(peer, id)?,
        &remote(tx.owner, tx.device)?,
        &mut store.clone(),
        &mut store.clone(),
        SystemTime::now(),
        &mut rng,
    )
    .await
    .map_err(failure)?;
    Ok((
        matches!(result, signal::CiphertextMessage::PreKeySignalMessage(_)),
        result.serialize().to_vec(),
    ))
}
pub async fn decrypt(
    tx: &Tx<'_>,
    peer: &str,
    id: u32,
    cipher: &[u8],
    prekey: bool,
) -> Result<Secret> {
    if cipher.is_empty() || cipher.len() > 65536 {
        return Err(Error::Invalid("PQ wrapped material size"));
    }
    let message = if prekey {
        signal::CiphertextMessage::PreKeySignalMessage(
            signal::PreKeySignalMessage::try_from(cipher).map_err(failure)?,
        )
    } else {
        signal::CiphertextMessage::SignalMessage(
            signal::SignalMessage::try_from(cipher).map_err(failure)?,
        )
    };
    let store = SignalStore::new(tx)?;
    let mut rng = rand::rngs::OsRng.unwrap_err();
    signal::message_decrypt(
        &message,
        &remote(peer, id)?,
        &remote(tx.owner, tx.device)?,
        &mut store.clone(),
        &mut store.clone(),
        &mut store.clone(),
        &store.clone(),
        &mut store.clone(),
        &mut rng,
    )
    .await
    .map(Secret::new)
    .map_err(failure)
}
