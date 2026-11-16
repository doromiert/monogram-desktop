//! QR-authorized, pinned TLS peer sync. No account server or plaintext fallback.
#[cfg(feature = "android-jni")]
mod jni;
mod network;
pub mod profile;
mod storage;
mod wire;
pub use network::{Invite, Mesh};
pub use storage::{MAX_RECORD, Meta, Peer, State, Store};
pub type Result<T> = std::result::Result<T, String>;
pub(crate) fn failure(_: impl std::fmt::Debug) -> String {
    "Device sync could not complete this operation.".into()
}
pub(crate) fn random<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut n = [0; N];
    ring::rand::SystemRandom::new()
        .fill(&mut n)
        .expect("OS entropy");
    n
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(crate) fn constant(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
#[cfg(test)]
mod constant_tests {
    use super::constant;
    #[test]
    fn constant_compares_equal_unequal_and_different_length_strings() {
        assert!(constant("", ""));
        assert!(constant("pairing-token", "pairing-token"));
        assert!(!constant("pairing-token", "pairing-tokeN"));
        assert!(!constant("pairing-token", "pairing-token-longer"));
        assert!(!constant("pairing-token", ""));
    }
}
