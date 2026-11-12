//! Mobile-compatible key rotation, explicit exact-fingerprint trust and sessions.
use super::{bundle::Bundle, native, records::*, *};
use std::collections::BTreeMap;
const ROTATION: u64 = 14 * 24 * 60 * 60 * 1000;
#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    Undecided,
    Trusted,
    Rejected,
}
pub fn prepare(store: &mut Store, now: u64) -> Result<Bundle> {
    prepare_for(store, now, false)
}
pub(super) fn prepare_for(store: &mut Store, now: u64, legacy: bool) -> Result<Bundle> {
    if now > i64::MAX as u64 {
        return Err(Error::Invalid("key timestamp"));
    }
    store.transaction(|tx| {
        let identity = tx.load(IDENTITY, "", 0)?;
        let meta = tx.load(META, "", 0)?;
        let (identity, mut next_pre, mut next_signed, mut current) =
            if let Some(identity) = identity {
                let meta = meta.ok_or(Error::Incomplete)?;
                if meta.len() != 24 || &meta[..4] != 1i32.to_be_bytes().as_slice() {
                    return Err(Error::Incomplete);
                }
                let pre = u64::from_be_bytes(meta[4..12].try_into().unwrap());
                let signed = u64::from_be_bytes(meta[12..20].try_into().unwrap());
                let current = u32::from_be_bytes(meta[20..].try_into().unwrap());
                if pre == 0 || signed == 0 || current == 0 {
                    return Err(Error::Incomplete);
                }
                (identity, pre, signed, current)
            } else {
                if tx.initialized()?
                    || meta.is_some()
                    || tx.load(IDENTITY, "", 1)?.is_some()
                    || !tx.ids(PRE_KEY)?.is_empty()
                    || !tx.ids(SIGNED_PRE_KEY)?.is_empty()
                    || !tx.ids(PRE_PUBLIC)?.is_empty()
                    || !tx.ids(SIGNED_PUBLIC)?.is_empty()
                {
                    return Err(Error::Incomplete);
                }
                let (record, public) = native::identity_for(legacy)?;
                tx.save(IDENTITY, "", 0, Some(&record))?;
                tx.save(IDENTITY, "", 1, Some(&public))?;
                (record, 1, 1, 0)
            };
        let mut signed = if current == 0 {
            None
        } else {
            tx.load(SIGNED_PUBLIC, "", current)?
        };
        if current != 0
            && (signed.as_ref().is_none_or(|s| s.len() != 104)
                || tx.load(SIGNED_PRE_KEY, "", current)?.is_none())
        {
            return Err(Error::Incomplete);
        }
        if signed.as_ref().is_none_or(|s| {
            now.saturating_sub(u64::from_be_bytes(s[96..104].try_into().unwrap())) >= ROTATION
        }) {
            if next_signed > i32::MAX as u64 {
                return Err(Error::Incomplete);
            }
            current = next_signed as u32;
            next_signed += 1;
            let (record, public, signature) =
                native::signed_key_for(legacy, &identity, current, now)?;
            tx.save(SIGNED_PRE_KEY, "", current, Some(&record))?;
            let mut info = Secret::new(Vec::with_capacity(104));
            info.extend(public);
            info.extend(signature);
            info.extend(now.to_be_bytes());
            tx.save(SIGNED_PUBLIC, "", current, Some(&info))?;
            signed = Some(info);
        }
        for old in tx.ids(SIGNED_PRE_KEY)? {
            if old != current {
                let info = tx.load(SIGNED_PUBLIC, "", old)?.ok_or(Error::Incomplete)?;
                if info.len() != 104 {
                    return Err(Error::Incomplete);
                }
                if now.saturating_sub(u64::from_be_bytes(info[96..104].try_into().unwrap()))
                    >= ROTATION * 2
                {
                    tx.save(SIGNED_PRE_KEY, "", old, None)?;
                    tx.save(SIGNED_PUBLIC, "", old, None)?;
                }
            }
        }
        let mut prekeys = BTreeMap::new();
        for id in tx.ids(PRE_KEY)? {
            let public = tx.load(PRE_PUBLIC, "", id)?.ok_or(Error::Incomplete)?;
            prekeys.insert(
                id,
                public
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Incomplete)?,
            );
        }
        for id in tx.ids(PRE_PUBLIC)? {
            if !prekeys.contains_key(&id) {
                tx.save(PRE_PUBLIC, "", id, None)?
            }
        }
        if prekeys.len() > 1000 {
            return Err(Error::Incomplete);
        }
        while prekeys.len() < 100 {
            if next_pre == 0xffffff {
                next_pre += 1
            }
            if next_pre > i32::MAX as u64 {
                return Err(Error::Incomplete);
            }
            let id = next_pre as u32;
            next_pre += 1;
            let (record, public) = native::pre_key(id)?;
            tx.save(PRE_KEY, "", id, Some(&record))?;
            tx.save(PRE_PUBLIC, "", id, Some(&public))?;
            prekeys.insert(id, public);
        }
        let public = tx.load(IDENTITY, "", 1)?.ok_or(Error::Incomplete)?;
        let signed = signed.ok_or(Error::Incomplete)?;
        let result = Bundle {
            identity: public
                .as_slice()
                .try_into()
                .map_err(|_| Error::Incomplete)?,
            signed_id: current,
            signed: signed[..32].try_into().unwrap(),
            signature: signed[32..96].try_into().unwrap(),
            prekeys,
        };
        if legacy {
            native::verify_legacy(&result)?;
        } else {
            result.validate()?;
        }
        let mut meta = Secret::new(Vec::with_capacity(24));
        meta.extend(1u32.to_be_bytes());
        meta.extend(next_pre.to_be_bytes());
        meta.extend(next_signed.to_be_bytes());
        meta.extend(current.to_be_bytes());
        tx.save(META, "", 0, Some(&meta))?;
        tx.mark_initialized()?;
        Ok(result)
    })
}
pub fn observe(
    store: &mut Store,
    peer: &str,
    device: u32,
    bundle: &Bundle,
    blind_trust: bool,
) -> Result<()> {
    address(peer, device)?;
    bundle.validate()?;
    let accept = blind_trust;
    store.transaction(|tx| {
        tx.save(SEEN, peer, device, Some(&bundle.identity))?;
        if accept
            && tx.load(REJECTED, peer, device)?.is_none()
            && trust(tx, peer, device, &bundle.identity)? != Trust::Trusted
        {
            tx.save(TRUSTED, peer, device, Some(&bundle.identity))?;
        }
        Ok(())
    })
}
pub fn trust(tx: &Tx<'_>, peer: &str, device: u32, key: &[u8; 32]) -> Result<Trust> {
    address(peer, device)?;
    let refused = tx
        .load(REJECTED, peer, device)?
        .unwrap_or_else(|| Secret::new(vec![]));
    if refused.len() % 32 != 0 || refused.len() > 4096 * 32 {
        return Err(Error::Incomplete);
    }
    if refused.chunks_exact(32).any(|k| k == key) {
        return Ok(Trust::Rejected);
    }
    Ok(
        if tx
            .load(TRUSTED, peer, device)?
            .is_some_and(|v| v.as_slice() == key)
        {
            Trust::Trusted
        } else {
            Trust::Undecided
        },
    )
}
pub fn decide(
    store: &mut Store,
    identities: &BTreeMap<String, BTreeMap<u32, [u8; 32]>>,
    approve: bool,
) -> Result<()> {
    let count: usize = identities.values().map(|keys| keys.len()).sum();
    if count == 0 || count > 1000 {
        return Err(Error::Invalid("trust batch"));
    }
    store.transaction(|tx| {
        for (peer, keys) in identities {
            for (device, key) in keys {
                address(peer, *device)?;
                let mut refused = tx
                    .load(REJECTED, peer, *device)?
                    .unwrap_or_else(|| Secret::new(vec![]));
                if refused.len() % 32 != 0 || refused.len() > 4096 * 32 {
                    return Err(Error::Incomplete);
                }
                if approve {
                    if tx
                        .load(SEEN, peer, *device)?
                        .is_none_or(|v| v.as_slice() != key)
                    {
                        return Err(Error::Changed);
                    }
                    let retained: Vec<u8> = refused
                        .chunks_exact(32)
                        .filter(|k| *k != key)
                        .flatten()
                        .copied()
                        .collect();
                    tx.save(
                        REJECTED,
                        peer,
                        *device,
                        if retained.is_empty() {
                            None
                        } else {
                            Some(&retained)
                        },
                    )?;
                    tx.save(TRUSTED, peer, *device, Some(key))?;
                } else {
                    if !refused.chunks_exact(32).any(|k| k == key) {
                        if refused.len() == 4096 * 32 {
                            return Err(Error::Incomplete);
                        }
                        refused.extend(key);
                        tx.save(REJECTED, peer, *device, Some(&refused))?;
                    }
                    if tx
                        .load(TRUSTED, peer, *device)?
                        .is_some_and(|v| v.as_slice() == key)
                    {
                        tx.save(TRUSTED, peer, *device, None)?;
                    }
                }
            }
        }
        Ok(())
    })
}
pub fn session(tx: &Tx<'_>, local: u32, peer: &str, device: u32, bundle: &Bundle) -> Result<()> {
    if trust(tx, peer, device, &bundle.identity)? != Trust::Trusted {
        return Err(Error::TrustRequired);
    }
    if tx.load(SESSION, peer, device)?.is_some() {
        if tx
            .load(SESSION_IDENTITY, peer, device)?
            .is_some_and(|key| key.as_slice() == bundle.identity)
        {
            return Ok(());
        }
        // A newly approved fingerprint needs its own ratchet.
        tx.save(SESSION, peer, device, None)?;
        tx.save(SESSION_IDENTITY, peer, device, None)?;
    }
    bundle.validate()?;
    let keys: Vec<_> = bundle.prekeys.iter().collect();
    let bound = (u32::MAX as u64 + 1) / keys.len() as u64 * keys.len() as u64;
    let index = loop {
        let value = u32::from_ne_bytes(random::<4>()?) as u64;
        if value < bound {
            break (value % keys.len() as u64) as usize;
        }
    };
    native::build(
        tx,
        local,
        peer,
        device,
        *keys[index].0,
        keys[index].1,
        bundle,
    )
}
