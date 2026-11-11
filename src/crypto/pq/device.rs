// SPDX-License-Identifier: AGPL-3.0-only
//! Separate hybrid identity lifecycle and trust, adapted from mobile PqOmemoDevice.
use super::{
    bundle::{self, Bundle, KemKey, PublicBundle},
    native,
    store::{Store, Tx},
    *,
};
use pq_signal::{self as signal, GenericSignedPreKey};
use rand::TryRngCore as _;
use std::{collections::BTreeMap, fs::File, path::Path};
const ROTATE: u64 = 7 * 24 * 60 * 60 * 1000;
const RETAIN: u64 = 90 * 24 * 60 * 60 * 1000;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trust {
    Undecided,
    Trusted,
    Rejected,
}
pub struct Review {
    pub peer: String,
    pub device: u32,
    pub fingerprint: String,
    pub trust: Trust,
}
pub struct Device {
    pub owner: String,
    pub id: u32,
    store: Store,
    _lock: Option<File>,
}

pub fn exists(root: &Path, owner: &str) -> bool {
    root.join("crypto")
        .join(crate::crypto::lifecycle::hex(&openssl::sha::sha256(
            owner.as_bytes(),
        )))
        .join("pq/device.json")
        .exists()
}
fn identity_pin(classic: &[u8; 33], pq: &[u8]) -> Result<Vec<u8>> {
    if pq.len() != signal::PQ_IDENTITY_PUBLIC_KEY_LEN {
        return Err(Error::Invalid("PQ hybrid identity"));
    }
    signal::IdentityKey::try_from(classic.as_slice()).map_err(native::failure)?;
    signal::PqIdentityKey::deserialize(pq).map_err(native::failure)?;
    let mut pin = Vec::with_capacity(2625);
    pin.extend(classic);
    pin.extend(pq);
    Ok(pin)
}
pub fn fingerprint(pin: &[u8]) -> Result<String> {
    if pin.len() != 2625 {
        return Err(Error::Incomplete);
    }
    bundle::fingerprint(&pin[..33], &pin[33..])
}
fn initialize(store: &mut Store) -> Result<()> {
    let allow_initialize = store.allow_initialize;
    let tx = store.begin()?;
    let identity = tx.get("local/identity")?;
    let pq = tx.get("local/pq-identity")?;
    let device = tx.get("local/device")?;
    let registration = tx.get("local/registration")?;
    match (&identity, &pq, &device, &registration) {
        (None, None, None, None) if allow_initialize && tx.keys("")?.is_empty() => {
            let mut rng = rand::rngs::OsRng.unwrap_err();
            let identity = signal::IdentityKeyPair::generate(&mut rng);
            let pq = signal::PqIdentityKeyPair::generate(&mut rng);
            tx.put("local/identity", Some(&identity.serialize()))?;
            tx.put("local/pq-identity", Some(&pq.serialize()))?;
            tx.put("local/device", Some(&tx.device.to_be_bytes()))?;
            let registration = rand::Rng::random_range(&mut rng, 1u32..=16380);
            tx.put("local/registration", Some(&registration.to_be_bytes()))?;
            tx.put("local/next-id", Some(&1u32.to_be_bytes()))?;
        }
        (Some(identity), Some(pq), Some(device), Some(registration)) => {
            signal::IdentityKeyPair::try_from(identity.as_slice()).map_err(native::failure)?;
            signal::PqIdentityKeyPair::deserialize(pq).map_err(native::failure)?;
            let registration = native::number(registration)?;
            if native::number(device)? != tx.device || registration == 0 || registration > 16380 {
                return Err(Error::Incomplete);
            }
            if native::number(&native::required(&tx, "local/next-id")?)? == 0 {
                return Err(Error::Incomplete);
            }
        }
        _ => return Err(Error::Incomplete),
    }
    tx.commit()
}
fn next(tx: &Tx<'_>) -> Result<u32> {
    let value = native::number(&native::required(tx, "local/next-id")?)?;
    if value == 0 || value >= i32::MAX as u32 {
        return Err(Error::Incomplete);
    }
    tx.put("local/next-id", Some(&(value + 1).to_be_bytes()))?;
    Ok(value)
}
fn kem(
    tx: &Tx<'_>,
    identity: &signal::IdentityKeyPair,
    now: u64,
) -> Result<signal::KyberPreKeyRecord> {
    let mut rng = rand::rngs::OsRng.unwrap_err();
    let pair = signal::kem::KeyPair::generate(signal::kem::KeyType::MLKEM1024, &mut rng);
    let signature = identity
        .private_key()
        .calculate_signature(&pair.public_key.serialize(), &mut rng)
        .map_err(|_| Error::Invalid("PQ classical signature"))?;
    Ok(signal::KyberPreKeyRecord::new(
        next(tx)?.into(),
        signal::Timestamp::from_epoch_millis(now),
        &pair,
        &signature,
    ))
}
fn public_kem(record: &signal::KyberPreKeyRecord) -> Result<KemKey> {
    Ok(KemKey {
        id: record.id().map_err(native::failure)?.into(),
        public: record
            .public_key()
            .map_err(native::failure)?
            .serialize()
            .into_vec(),
        signature: record
            .signature()
            .map_err(native::failure)?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Incomplete)?,
    })
}
impl Device {
    pub async fn open(root: &Path, owner: &str, create: bool) -> Result<Self> {
        let (mut store, lock) = Store::open_profile(root, owner, create).await?;
        initialize(&mut store)?;
        let mut device = Self {
            owner: owner.into(),
            id: store.device,
            store,
            _lock: Some(lock),
        };
        let fingerprint = device.fingerprint()?;
        device.store.bind_identity(&fingerprint).await?;
        Ok(device)
    }
    pub fn memory(owner: &str, id: u32) -> Result<Self> {
        let mut store = Store::memory(owner, id)?;
        initialize(&mut store)?;
        Ok(Self {
            owner: owner.into(),
            id,
            store,
            _lock: None,
        })
    }
    pub fn begin(&mut self) -> Result<Tx<'_>> {
        self.store.begin()
    }
    pub fn fingerprint(&mut self) -> Result<String> {
        let tx = self.begin()?;
        let identity = native::identity(&tx)?.identity_key().serialize();
        let pq = native::pq_identity(&tx)?.public_key().serialize();
        fingerprint(&identity_pin(
            identity
                .as_ref()
                .try_into()
                .map_err(|_| Error::Incomplete)?,
            &pq,
        )?)
    }
    pub fn prepare(&mut self) -> Result<PublicBundle> {
        let now = chrono::Utc::now().timestamp_millis();
        if now < 0 {
            return Err(Error::Incomplete);
        }
        self.prepare_at(now as u64)
    }
    pub fn needs_replenish(&mut self) -> Result<bool> {
        let now = chrono::Utc::now().timestamp_millis();
        if now < 0 {
            return Err(Error::Incomplete);
        }
        let now = now as u64;
        let tx = self.begin()?;
        if tx.keys("prekey/")?.len() < 16 {
            return Ok(true);
        }
        let mut kem_count = 0;
        for key in tx.keys("kem/")? {
            if tx.get(&format!("last-resort/{}", &key[4..]))?.is_none() {
                kem_count += 1;
            }
        }
        if kem_count < 16 {
            return Ok(true);
        }
        let Some(signed) = tx.get("local/active-signed")? else {
            return Ok(true);
        };
        let Some(kem) = tx.get("local/active-kem")? else {
            return Ok(true);
        };
        let signed = signal::SignedPreKeyRecord::deserialize(&native::required(
            &tx,
            &format!("signed/{}", native::number(&signed)?),
        )?)
        .map_err(native::failure)?;
        let kem = signal::KyberPreKeyRecord::deserialize(&native::required(
            &tx,
            &format!("kem/{}", native::number(&kem)?),
        )?)
        .map_err(native::failure)?;
        let due =
            |stamp: u64| now.saturating_sub(stamp) >= ROTATE || stamp > now.saturating_add(600000);
        Ok(
            due(signed.timestamp().map_err(native::failure)?.epoch_millis())
                || due(kem.timestamp().map_err(native::failure)?.epoch_millis()),
        )
    }
    fn prepare_at(&mut self, now: u64) -> Result<PublicBundle> {
        let tx = self.begin()?;
        let identity = native::identity(&tx)?;
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let mut signed = tx
            .get("local/active-signed")?
            .map(|id| -> Result<_> {
                let id = native::number(&id)?;
                signal::SignedPreKeyRecord::deserialize(&native::required(
                    &tx,
                    &format!("signed/{id}"),
                )?)
                .map_err(native::failure)
            })
            .transpose()?;
        if signed
            .as_ref()
            .map(|s| s.timestamp().map(|t| t.epoch_millis()))
            .transpose()
            .map_err(native::failure)?
            .is_none_or(|stamp| {
                now.saturating_sub(stamp) >= ROTATE || stamp > now.saturating_add(600000)
            })
        {
            let pair = signal::KeyPair::generate(&mut rng);
            let signature = identity
                .private_key()
                .calculate_signature(&pair.public_key.serialize(), &mut rng)
                .map_err(|_| Error::Invalid("PQ classical signature"))?;
            let record = signal::SignedPreKeyRecord::new(
                next(&tx)?.into(),
                signal::Timestamp::from_epoch_millis(now),
                &pair,
                &signature,
            );
            let id: u32 = record.id().map_err(native::failure)?.into();
            tx.put(
                &format!("signed/{id}"),
                Some(&record.serialize().map_err(native::failure)?),
            )?;
            tx.put("local/active-signed", Some(&id.to_be_bytes()))?;
            signed = Some(record);
        }
        let signed = signed.ok_or(Error::Incomplete)?;
        let signed_id: u32 = signed.id().map_err(native::failure)?.into();
        let mut last = tx
            .get("local/active-kem")?
            .map(|id| -> Result<_> {
                let id = native::number(&id)?;
                signal::KyberPreKeyRecord::deserialize(&native::required(
                    &tx,
                    &format!("kem/{id}"),
                )?)
                .map_err(native::failure)
            })
            .transpose()?;
        if last
            .as_ref()
            .map(|s| s.timestamp().map(|t| t.epoch_millis()))
            .transpose()
            .map_err(native::failure)?
            .is_none_or(|stamp| {
                now.saturating_sub(stamp) >= ROTATE || stamp > now.saturating_add(600000)
            })
        {
            let record = kem(&tx, &identity, now)?;
            let id: u32 = record.id().map_err(native::failure)?.into();
            tx.put(
                &format!("kem/{id}"),
                Some(&record.serialize().map_err(native::failure)?),
            )?;
            tx.put(&format!("last-resort/{id}"), Some(&[1]))?;
            tx.put("local/active-kem", Some(&id.to_be_bytes()))?;
            last = Some(record);
        }
        let last = last.ok_or(Error::Incomplete)?;
        let last_id: u32 = last.id().map_err(native::failure)?.into();
        for name in tx.keys("signed/")? {
            let record = signal::SignedPreKeyRecord::deserialize(&native::required(&tx, &name)?)
                .map_err(native::failure)?;
            if u32::from(record.id().map_err(native::failure)?) != signed_id
                && now.saturating_sub(record.timestamp().map_err(native::failure)?.epoch_millis())
                    > RETAIN
            {
                tx.put(&name, None)?;
            }
        }
        for name in tx.keys("kem/")? {
            let record = signal::KyberPreKeyRecord::deserialize(&native::required(&tx, &name)?)
                .map_err(native::failure)?;
            let id: u32 = record.id().map_err(native::failure)?.into();
            if id != last_id
                && now.saturating_sub(record.timestamp().map_err(native::failure)?.epoch_millis())
                    > RETAIN
            {
                tx.put(&name, None)?;
                tx.put(&format!("last-resort/{id}"), None)?;
                for key in tx.keys(&format!("replay/{id}/"))? {
                    tx.put(&key, None)?;
                }
            }
        }
        let mut prekeys = BTreeMap::new();
        for name in tx.keys("prekey/")? {
            let record = signal::PreKeyRecord::deserialize(&native::required(&tx, &name)?)
                .map_err(native::failure)?;
            prekeys.insert(
                record.id().map_err(native::failure)?.into(),
                record
                    .public_key()
                    .map_err(native::failure)?
                    .serialize()
                    .as_ref()
                    .try_into()
                    .map_err(|_| Error::Incomplete)?,
            );
        }
        while prekeys.len() < 32 {
            let id = next(&tx)?;
            let record = signal::PreKeyRecord::new(id.into(), &signal::KeyPair::generate(&mut rng));
            tx.put(
                &format!("prekey/{id}"),
                Some(&record.serialize().map_err(native::failure)?),
            )?;
            prekeys.insert(
                id,
                record
                    .public_key()
                    .map_err(native::failure)?
                    .serialize()
                    .as_ref()
                    .try_into()
                    .map_err(|_| Error::Incomplete)?,
            );
        }
        let mut kem_prekeys = BTreeMap::new();
        for name in tx.keys("kem/")? {
            let record = signal::KyberPreKeyRecord::deserialize(&native::required(&tx, &name)?)
                .map_err(native::failure)?;
            let key = public_kem(&record)?;
            if tx.get(&format!("last-resort/{}", key.id))?.is_none() {
                kem_prekeys.insert(key.id, key);
            }
        }
        while kem_prekeys.len() < 32 {
            let record = kem(&tx, &identity, now)?;
            let key = public_kem(&record)?;
            tx.put(
                &format!("kem/{}", key.id),
                Some(&record.serialize().map_err(native::failure)?),
            )?;
            kem_prekeys.insert(key.id, key);
        }
        let mut public = PublicBundle {
            identity: identity
                .identity_key()
                .serialize()
                .as_ref()
                .try_into()
                .map_err(|_| Error::Incomplete)?,
            pq_identity: native::pq_identity(&tx)?
                .public_key()
                .serialize()
                .into_vec(),
            pq_signature: Vec::new(),
            classic_signature: [0; 64],
            signed_id,
            signed: signed
                .public_key()
                .map_err(native::failure)?
                .serialize()
                .as_ref()
                .try_into()
                .map_err(|_| Error::Incomplete)?,
            signed_signature: signed
                .signature()
                .map_err(native::failure)?
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)?,
            prekeys,
            last_resort: public_kem(&last)?,
            kem_prekeys,
        };
        let transcript = bundle::transcript(&public)?;
        let (_, pq_signature, classic_signature) = native::sign_auth(&tx, &transcript)?;
        public.pq_signature = pq_signature;
        public.classic_signature = classic_signature;
        // Verify our public projection before making any newly-generated private records durable.
        Bundle::from_parts(public.clone())?;
        tx.commit()?;
        Ok(public)
    }
    pub fn observe(&mut self, peer: &str, id: u32, bundle: &Bundle, blind: bool) -> Result<Trust> {
        self.observe_hybrid(peer, id, &bundle.identity, &bundle.pq_identity, blind)
    }
    pub fn observe_hybrid(
        &mut self,
        peer: &str,
        id: u32,
        classic: &[u8; 33],
        pq: &[u8],
        blind: bool,
    ) -> Result<Trust> {
        let pin = identity_pin(classic, pq)?;
        let key = native::peer_key(peer, id)?;
        let own = peer == self.owner;
        let tx = self.begin()?;
        tx.put(&format!("observed/{key}"), Some(&pin))?;
        let blocked = tx.get(&format!("blocked/{key}"))?.is_some();
        let previous = tx.get(&format!("trust/{key}"))?;
        if blind
            && !own
            && !blocked
            && previous.as_ref().map(|value| value.as_slice()) != Some(pin.as_slice())
        {
            tx.put(&format!("trust/{key}"), Some(&pin))?;
            tx.put(&format!("session/{key}"), None)?;
            tx.put(&format!("identity/{key}"), None)?;
        }
        let trusted = tx
            .get(&format!("trust/{key}"))?
            .is_some_and(|saved| saved.as_slice() == pin);
        tx.commit()?;
        Ok(if blocked {
            Trust::Rejected
        } else if trusted {
            Trust::Trusted
        } else {
            Trust::Undecided
        })
    }
    pub fn decide(&mut self, peer: &str, id: u32, expected: &str, approve: bool) -> Result<()> {
        let key = native::peer_key(peer, id)?;
        let tx = self.begin()?;
        let pin = native::required(&tx, &format!("observed/{key}"))?;
        if fingerprint(&pin)? != expected {
            return Err(Error::Changed);
        }
        let previous = tx.get(&format!("trust/{key}"))?;
        tx.put(&format!("trust/{key}"), approve.then_some(pin.as_slice()))?;
        tx.put(&format!("blocked/{key}"), (!approve).then_some(&[1][..]))?;
        if !approve || previous.as_ref().map(|value| value.as_slice()) != Some(pin.as_slice()) {
            tx.put(&format!("session/{key}"), None)?;
            tx.put(&format!("identity/{key}"), None)?;
        }
        tx.commit()
    }
    pub fn review(&mut self, peer: &str) -> Result<Vec<Review>> {
        crate::crypto::address(peer, 1)?;
        use base64::Engine as _;
        let prefix = format!(
            "observed/{}/",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(peer.as_bytes())
        );
        let tx = self.begin()?;
        let mut reviews = Vec::new();
        for name in tx.keys(&prefix)? {
            let id = name[prefix.len()..]
                .parse::<u32>()
                .map_err(|_| Error::Incomplete)?;
            let key = native::peer_key(peer, id)?;
            let pin = native::required(&tx, &name)?;
            let blocked = tx.get(&format!("blocked/{key}"))?.is_some();
            let trusted = tx
                .get(&format!("trust/{key}"))?
                .is_some_and(|saved| saved.as_slice() == pin.as_slice());
            reviews.push(Review {
                peer: peer.into(),
                device: id,
                fingerprint: fingerprint(&pin)?,
                trust: if blocked {
                    Trust::Rejected
                } else if trusted {
                    Trust::Trusted
                } else {
                    Trust::Undecided
                },
            });
        }
        reviews.sort_by_key(|row| row.device);
        Ok(reviews)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{MasterKey, journal::Context};
    use tokio_xmpp::minidom::Element;
    const A: &str = "alice@example.invalid";
    const B: &str = "bob@example.invalid";
    fn plain(from: &str, to: &str, id: &str, body: &str) -> Element {
        Element::builder("message", "jabber:client")
            .attr("from".try_into().unwrap(), from)
            .attr("to".try_into().unwrap(), to)
            .attr("type".try_into().unwrap(), "chat")
            .attr("id".try_into().unwrap(), id)
            .append(
                Element::builder("body", "jabber:client")
                    .append(body)
                    .build(),
            )
            .build()
    }
    #[tokio::test]
    async fn pq_profile_transactions_restart_and_hybrid_trust() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("monogram-pq-core-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        struct Clean(std::path::PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _clean = Clean(directory.clone());
        let path = directory.join("pq.sqlite3");
        let mut missing = Store::memory(A, 100).unwrap();
        missing.allow_initialize = false;
        assert!(matches!(initialize(&mut missing), Err(Error::Incomplete)));
        assert!(missing.begin().unwrap().keys("").unwrap().is_empty());
        let mut store = Store::open(Some(&path), B, 202, MasterKey::new([7; 32])).unwrap();
        initialize(&mut store).unwrap();
        let mut bob = Device {
            owner: B.into(),
            id: 202,
            store,
            _lock: None,
        };
        let mut alice = Device::memory(A, 101).unwrap();
        let ab = Bundle::from_parts(alice.prepare().unwrap()).unwrap();
        let bb = Bundle::from_parts(bob.prepare().unwrap()).unwrap();
        let original = bob.fingerprint().unwrap();
        assert_eq!(original, bb.fingerprint().unwrap());
        assert_eq!(alice.observe(B, 202, &bb, true).unwrap(), Trust::Trusted);
        assert_eq!(
            alice.observe(A, 303, &bb, true).unwrap(),
            Trust::Undecided,
            "contact blind trust never approves own devices"
        );
        assert_eq!(bob.observe(A, 101, &ab, false).unwrap(), Trust::Undecided);
        bob.decide(A, 101, &ab.fingerprint().unwrap(), true)
            .unwrap();
        {
            let tx = bob.begin().unwrap();
            tx.put("test/rollback", Some(b"not committed")).unwrap();
        }
        assert!(bob.begin().unwrap().get("test/rollback").unwrap().is_none());
        let first = super::super::wire::encrypt(
            &mut alice,
            &plain(A, B, "first", "hybrid hello"),
            &[super::super::wire::Target {
                peer: B.into(),
                device: 202,
                bundle: Some(bb.clone()),
            }],
            false,
        )
        .await
        .unwrap();
        let mut first_outer = plain(A, B, "first", "");
        first_outer.append_child(first);
        let first = first_outer;
        let now = chrono::Utc::now().timestamp_millis();
        let received =
            super::super::wire::receive(&mut bob, &first, A, B, now, Context::default(), false)
                .await
                .unwrap();
        assert_eq!(
            received
                .content
                .as_ref()
                .unwrap()
                .get_child("body", "jabber:client")
                .unwrap()
                .text(),
            "hybrid hello"
        );
        assert_eq!(super::super::wire::pending(&mut bob).unwrap().len(), 1);
        drop(bob);
        let mut store = Store::open(Some(&path), B, 202, MasterKey::new([7; 32])).unwrap();
        initialize(&mut store).unwrap();
        let mut bob = Device {
            owner: B.into(),
            id: 202,
            store,
            _lock: None,
        };
        assert_eq!(bob.fingerprint().unwrap(), original);
        let pending = super::super::wire::pending(&mut bob).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].token, received.token);
        super::super::wire::accepted(&mut bob, &received.token).unwrap();
        let reply = super::super::wire::encrypt(
            &mut bob,
            &plain(B, A, "reply", "hybrid reply"),
            &[super::super::wire::Target {
                peer: A.into(),
                device: 101,
                bundle: Some(ab.clone()),
            }],
            false,
        )
        .await
        .unwrap();
        let mut reply_outer = plain(B, A, "reply", "");
        reply_outer.append_child(reply);
        let reply = reply_outer;
        let received =
            super::super::wire::receive(&mut alice, &reply, B, A, now, Context::default(), false)
                .await
                .unwrap();
        assert_eq!(
            received
                .content
                .unwrap()
                .get_child("body", "jabber:client")
                .unwrap()
                .text(),
            "hybrid reply"
        );
        bob.decide(A, 101, &ab.fingerprint().unwrap(), false)
            .unwrap();
        assert_eq!(
            bob.observe(A, 101, &ab, true).unwrap(),
            Trust::Rejected,
            "blind trust preserves explicit blocks"
        );
        assert!(!native::has_session(&bob.begin().unwrap(), A, 101).unwrap());
        let rotated =
            Bundle::from_parts(bob.prepare_at((now as u64) + ROTATE + 1).unwrap()).unwrap();
        assert_ne!(rotated.signed_id, bb.signed_id);
        let tx = bob.begin().unwrap();
        assert!(tx.keys("signed/").unwrap().len() >= 2);
        assert!(tx.keys("last-resort/").unwrap().len() >= 2);
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
