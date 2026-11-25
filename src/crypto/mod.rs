//! Independent classic, Legacy and PQ OMEMO identities, trust and sessions.
pub mod bundle;
pub mod device;
pub mod engine;
pub mod journal;
pub mod legacy;
pub mod lifecycle;
pub mod native;
pub mod payload;
pub mod pep;
pub mod pq;
pub mod records;
pub mod service;
pub mod vault;
use std::fmt;
pub type Secret = zeroize::Zeroizing<Vec<u8>>;
pub type MasterKey = zeroize::Zeroizing<[u8; 32]>;
/// Whether a room failure shown as text is one member's unusable keys, in either protocol, which the
/// sender may choose to leave that member out for (Send Anyway).
pub(crate) fn skippable_reason(reason: &str) -> bool {
    legacy::skippable_reason(reason) || pq::service::skippable_reason(reason)
}
/// Whose bundle could not be used, for [`Error::InvalidBundle`].
#[derive(Debug)]
pub struct BundleFor {
    pub protocol: &'static str,
    pub peer: String,
    pub device: Option<u32>,
}
impl Error {
    /// An invalid or missing bundle for `peer` (and `device` when known) under `protocol`.
    pub fn invalid_bundle(protocol: &'static str, peer: &str, device: Option<u32>) -> Self {
        Self::InvalidBundle(Some(BundleFor { protocol, peer: peer.into(), device }))
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Native(i32),
    Storage(rusqlite::Error),
    Crypto(openssl::error::ErrorStack),
    TrustRequired,
    OwnDeviceTrustRequired,
    Changed,
    Incomplete,
    Vault,
    Unavailable(&'static str),
    /// A contact refused to share their Legacy OMEMO keys: it carries their bare address.
    ContactAuthorization(String),
    Uncertain,
    /// A device's public bundle is invalid or missing; names who and which protocol when known.
    InvalidBundle(Option<BundleFor>),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContactAuthorization(contact) => {
                return write!(f, "{} {contact}", legacy::DENIED_PREFIX);
            }
            Self::InvalidBundle(Some(who)) => {
                return match who.device {
                    Some(device) => write!(
                        f,
                        "No usable {} keys from {} (device {device}); refresh device review",
                        who.protocol, who.peer
                    ),
                    None => write!(
                        f,
                        "No usable {} keys from {}; refresh device review",
                        who.protocol, who.peer
                    ),
                };
            }
            _ => {}
        }
        f.write_str(match self {
            Self::Invalid(_) => "Invalid OMEMO 2 content",
            Self::Native(_) => "OMEMO 2 ratchet operation failed",
            Self::Storage(_) => "OMEMO 2 state could not be saved",
            Self::Crypto(_) => "OMEMO 2 authentication failed",
            Self::TrustRequired => "Review this device's fingerprint before using encryption",
            Self::OwnDeviceTrustRequired => {
                "Review these own-device fingerprints or enable automatic trust for this conversation. Explicitly blocked devices remain blocked."
            }
            Self::Changed => "The device identity changed; refresh before approving it",
            Self::Incomplete => "Incomplete OMEMO 2 state; existing identity preserved",
            Self::Vault => "The encryption key is missing or the keyring is locked",
            Self::Unavailable(reason) => reason,
            Self::ContactAuthorization(_) => unreachable!("written above"),
            Self::Uncertain => "Encrypted delivery is uncertain; retrying may send a duplicate",
            Self::InvalidBundle(_) => {
                "A device's encryption keys are unavailable. Reconnect and retry."
            }
        })
    }
}
impl std::error::Error for Error {}
impl Error {
    /// Diagnostic labels contain no message content, addresses, keys or server responses.
    pub(crate) fn diagnostic(&self) -> String {
        match self {
            Self::Invalid(reason) => format!("invalid: {reason}"),
            Self::Native(code) => format!("ratchet: {code}"),
            Self::Unavailable(reason) => format!("unavailable: {reason}"),
            Self::Storage(error) => format!("SQLite failure: {:?}", error.sqlite_error_code()),
            Self::Crypto(error) => format!("OpenSSL failure codes: {:?}", error.errors().iter().map(|error| error.code()).collect::<Vec<_>>()),
            Self::TrustRequired => "contact fingerprint is not approved".into(),
            Self::OwnDeviceTrustRequired => "own-device fingerprint is not approved".into(),
            Self::Changed => "local or remote encryption identity changed".into(),
            Self::Incomplete => "incomplete persisted encryption state".into(),
            Self::Vault => "local encryption key unavailable or keyring locked".into(),
            Self::ContactAuthorization(_) => "contact denied encryption-key access".into(),
            Self::Uncertain => "encrypted submission has uncertain delivery".into(),
            Self::InvalidBundle(_) => "device bundle missing or invalid".into(),
        }
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<openssl::error::ErrorStack> for Error {
    fn from(e: openssl::error::ErrorStack) -> Self {
        Self::Crypto(e)
    }
}
pub fn address(peer: &str, device: u32) -> Result<()> {
    if device == 0
        || device > i32::MAX as u32
        || crate::protocol::bare(peer).ok().as_deref() != Some(peer)
    {
        return Err(Error::Invalid("canonical peer/device"));
    }
    Ok(())
}
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    openssl::rand::rand_bytes(&mut bytes)?;
    Ok(bytes)
}
#[cfg(test)]
mod tests;

// Keyrings can become available after login/auto-start. Retry local loading on
// crypto workers; existing profile/pin checks still forbid replacing identities.
pub(super) fn keyring_recovery_timer() -> tokio::time::Interval {
    let delay = std::time::Duration::from_secs(5);
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + delay, delay);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer
}
