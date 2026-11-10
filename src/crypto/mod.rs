//! OMEMO 2-only state; never shares identities, trust or sessions with legacy/PQ.
pub mod bundle;
pub mod device;
pub mod engine;
pub mod journal;
pub mod native;
pub mod payload;
pub mod pep;
pub mod records;
pub mod vault;
use std::fmt;
pub type Secret = zeroize::Zeroizing<Vec<u8>>;
pub type MasterKey = zeroize::Zeroizing<[u8; 32]>;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Native(i32),
    Storage(rusqlite::Error),
    Crypto(openssl::error::ErrorStack),
    TrustRequired,
    Changed,
    Incomplete,
    Vault,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid(_) => "Invalid OMEMO 2 content",
            Self::Native(_) => "OMEMO 2 ratchet operation failed",
            Self::Storage(_) => "OMEMO 2 state could not be saved",
            Self::Crypto(_) => "OMEMO 2 authentication failed",
            Self::TrustRequired => "Review this device's fingerprint before using encryption",
            Self::Changed => "The device identity changed; refresh before approving it",
            Self::Incomplete => "Incomplete OMEMO 2 state; existing identity preserved",
            Self::Vault => "The encryption key is missing or the keyring is locked",
        })
    }
}
impl std::error::Error for Error {}
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
