//! Decryption, ratchet changes and pending authenticated content commit together.
//! Chat-history persistence and heartbeat delivery acknowledge separate phases.
use super::{bundle::xml, records::*, *};
use serde::{Deserialize, Serialize};
use tokio_xmpp::minidom::Element;
const ENTRIES: usize = 128;
const BYTES: usize = 16 * 1024 * 1024;
const ENTRY: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    epoch: [u8; 16],
    slot: u32,
    digest: [u8; 32],
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    pub archive: Option<(String, String)>,
    pub historical: bool,
    pub receipt_to: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    epoch: [u8; 16],
    digest: [u8; 32],
    identity: [u8; 32],
    owner: String,
    device: u32,
    fingerprint: [u8; 32],
    accepted: bool,
    replenish: bool,
    outer: Option<String>,
    content: Option<String>,
    ack: Option<String>,
    sent_time: i64,
    #[serde(default)]
    context: Option<Context>,
}
pub struct Received {
    pub content: Option<Element>,
    pub outer: Option<Element>,
    pub acknowledgement: Option<Element>,
    pub fingerprint: [u8; 32],
    pub replenish: bool,
    pub token: Option<Token>,
    pub sent_time: i64,
    pub context: Option<Context>,
}
pub struct Journal {
    epoch: [u8; 16],
    owner: String,
    device: u32,
    identity: [u8; 32],
}
fn text(root: &Element, limit: usize) -> Result<String> {
    String::from_utf8(xml(root, limit)?).map_err(|_| Error::Invalid("journal XML"))
}
fn parsed(value: &Option<String>) -> Result<Option<Element>> {
    value
        .as_ref()
        .map(|s| {
            if s.contains("<!") {
                return Err(Error::Incomplete);
            }
            super::bundle::document(s, ENTRY).map_err(|_| Error::Incomplete)
        })
        .transpose()
}
impl Journal {
    pub fn new(store: &mut Store) -> Result<Self> {
        let owner = store.owner.clone();
        let device = store.device;
        store.transaction(|tx| {
            let header = tx.load(INCOMING, "", 0)?;
            let epoch = if let Some(header) = header {
                Self::header(&header)?
            } else {
                if !tx.ids(INCOMING)?.is_empty() {
                    return Err(Error::Incomplete);
                }
                let epoch = random::<16>()?;
                let mut header = Vec::from(*b"MIH1");
                header.extend(epoch);
                tx.save(INCOMING, "", 0, Some(&header))?;
                epoch
            };
            let identity = tx.load(IDENTITY, "", 1)?.ok_or(Error::Incomplete)?;
            Ok(Self {
                epoch,
                owner,
                device,
                identity: identity
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Incomplete)?,
            })
        })
    }
    fn header(bytes: &[u8]) -> Result<[u8; 16]> {
        if bytes.len() != 20 || &bytes[..4] != b"MIH1" {
            return Err(Error::Incomplete);
        }
        Ok(bytes[4..].try_into().unwrap())
    }
    fn check(&self, tx: &Tx<'_>) -> Result<()> {
        let header = tx.load(INCOMING, "", 0)?.ok_or(Error::Incomplete)?;
        if Self::header(&header)? != self.epoch
            || tx
                .load(IDENTITY, "", 1)?
                .is_none_or(|k| k.as_slice() != self.identity)
        {
            return Err(Error::Incomplete);
        }
        Ok(())
    }
    fn entries(&self, tx: &Tx<'_>) -> Result<Vec<(u32, Saved, usize)>> {
        self.check(tx)?;
        let mut entries = vec![];
        let mut total = 0;
        for slot in tx.ids(INCOMING)? {
            if slot == 0 {
                continue;
            }
            if slot as usize > ENTRIES || entries.len() >= ENTRIES {
                return Err(Error::Incomplete);
            }
            let bytes = tx.load(INCOMING, "", slot)?.ok_or(Error::Incomplete)?;
            if bytes.len() > ENTRY {
                return Err(Error::Incomplete);
            }
            total += bytes.len();
            if total > BYTES {
                return Err(Error::Incomplete);
            }
            let saved: Saved = serde_json::from_slice(&bytes).map_err(|_| Error::Incomplete)?;
            if saved.version != 1
                || saved.epoch != self.epoch
                || saved.owner != self.owner
                || saved.device != self.device
                || saved.identity != self.identity
                || (saved.accepted && (saved.content.is_some() || saved.outer.is_some()))
            {
                return Err(Error::Incomplete);
            }
            entries.push((slot, saved, bytes.len()));
        }
        Ok(entries)
    }
    fn received(&self, slot: u32, saved: &Saved) -> Result<Received> {
        Ok(Received {
            content: parsed(&saved.content)?,
            outer: parsed(&saved.outer)?,
            acknowledgement: parsed(&saved.ack)?,
            fingerprint: saved.fingerprint,
            replenish: saved.replenish,
            token: Some(Token {
                epoch: self.epoch,
                slot,
                digest: saved.digest,
            }),
            sent_time: saved.sent_time,
            context: saved.context.clone(),
        })
    }
    pub fn find(&self, tx: &Tx<'_>, digest: &[u8; 32]) -> Result<Option<Received>> {
        for (slot, saved, _) in self.entries(tx)? {
            if &saved.digest == digest {
                return self.received(slot, &saved).map(Some);
            }
        }
        Ok(None)
    }
    pub fn append(
        &self,
        tx: &Tx<'_>,
        digest: [u8; 32],
        outer: &Element,
        content: Option<&Element>,
        ack: Option<&Element>,
        fingerprint: [u8; 32],
        replenish: bool,
        sent_time: i64,
        context: Option<Context>,
    ) -> Result<Received> {
        let entries = self.entries(tx)?;
        if entries.len() >= ENTRIES {
            return Err(Error::Invalid("incoming journal full"));
        }
        let total: usize = entries.iter().map(|(_, _, size)| size).sum();
        let slot = (1..=ENTRIES as u32)
            .find(|id| !entries.iter().any(|(slot, _, _)| slot == id))
            .ok_or(Error::Incomplete)?;
        let saved = Saved {
            version: 1,
            epoch: self.epoch,
            digest,
            identity: self.identity,
            owner: self.owner.clone(),
            device: self.device,
            fingerprint,
            accepted: content.is_none(),
            replenish,
            outer: if content.is_some() {
                Some(text(outer, 1024 * 1024)?)
            } else {
                None
            },
            content: content.map(|c| text(c, MAX_CONTENT)).transpose()?,
            ack: ack.map(|a| text(a, 65536)).transpose()?,
            sent_time,
            context,
        };
        let bytes = Secret::new(serde_json::to_vec(&saved).map_err(|_| Error::Incomplete)?);
        if bytes.len() > ENTRY || total + bytes.len() > BYTES {
            return Err(Error::Invalid("incoming journal capacity"));
        }
        tx.save(INCOMING, "", slot, Some(&bytes))?;
        self.received(slot, &saved)
    }
    pub fn pending(&self, store: &mut Store) -> Result<Vec<Received>> {
        store.transaction(|tx| {
            self.entries(tx)?
                .iter()
                .map(|(slot, saved, _)| self.received(*slot, saved))
                .collect()
        })
    }
    pub fn accept(&self, store: &mut Store, token: &Token) -> Result<bool> {
        if token.epoch != self.epoch {
            return Ok(false);
        }
        store.transaction(|tx| {
            for (slot, mut saved, _) in self.entries(tx)? {
                if slot == token.slot && saved.digest == token.digest {
                    if saved.accepted {
                        return Ok(true);
                    }
                    saved.accepted = true;
                    saved.outer = None;
                    saved.content = None;
                    if saved.ack.is_some() {
                        let bytes =
                            Secret::new(serde_json::to_vec(&saved).map_err(|_| Error::Incomplete)?);
                        tx.save(INCOMING, "", slot, Some(&bytes))?;
                    } else {
                        tx.save(INCOMING, "", slot, None)?;
                    }
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
    pub fn acknowledge(&self, store: &mut Store, token: &Token) -> Result<()> {
        if token.epoch != self.epoch {
            return Ok(());
        }
        store.transaction(|tx| {
            for (slot, saved, _) in self.entries(tx)? {
                if slot == token.slot && saved.digest == token.digest && saved.accepted {
                    tx.save(INCOMING, "", slot, None)?;
                    break;
                }
            }
            Ok(())
        })
    }
}
const MAX_CONTENT: usize = 512 * 1024;
