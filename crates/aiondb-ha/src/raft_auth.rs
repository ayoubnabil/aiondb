//! HMAC-SHA256 authentication for Raft TCP messages.
//!
//! Every frame is signed with a shared secret. Servers reject frames
//! whose tag does not verify. Production deployments should layer
//! TLS underneath this; HMAC alone protects against accidental
//! misrouting and gives a tamper-evident channel inside trusted
//! networks.

use std::fmt;
use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const MIN_RAFT_SHARED_SECRET_BYTES: usize = 32;

#[derive(Clone)]
pub struct RaftSharedSecret {
    secret: Arc<Vec<u8>>,
}

impl RaftSharedSecret {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: Arc::new(secret.into()),
        }
    }

    pub fn len(&self) -> usize {
        self.secret.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secret.is_empty()
    }

    pub fn is_strong_enough(&self) -> bool {
        self.len() >= MIN_RAFT_SHARED_SECRET_BYTES
    }

    /// Compute a 32-byte HMAC-SHA256 tag over `payload`.
    pub fn sign(&self, payload: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC takes any key length");
        mac.update(payload);
        let bytes = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    /// Verify a previously-computed tag. Constant-time comparison.
    pub fn verify(&self, payload: &[u8], tag: &[u8]) -> bool {
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC takes any key length");
        mac.update(payload);
        mac.verify_slice(tag).is_ok()
    }
}

impl fmt::Debug for RaftSharedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RaftSharedSecret")
            .field("len", &self.secret.len())
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Helper : encode an authenticated frame as
/// `[ payload_len (u32 BE) ][ payload ][ tag (32 B) ]`.
pub fn encode_authenticated(secret: &RaftSharedSecret, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len() + 32);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
    out.extend_from_slice(&secret.sign(payload));
    out
}

/// Helper : decode-and-verify. Returns the payload slice or `None`
/// when verification fails or the frame is malformed.
pub fn decode_authenticated<'a>(secret: &RaftSharedSecret, frame: &'a [u8]) -> Option<&'a [u8]> {
    if frame.len() < 4 + 32 {
        return None;
    }
    let len = u32::from_be_bytes(frame[..4].try_into().ok()?) as usize;
    if frame.len() < 4 + len + 32 {
        return None;
    }
    let payload = &frame[4..4 + len];
    let tag = &frame[4 + len..4 + len + 32];
    if secret.verify(payload, tag) {
        Some(payload)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_returns_true() {
        let s = RaftSharedSecret::new(b"top-secret".to_vec());
        let tag = s.sign(b"hello");
        assert!(s.verify(b"hello", &tag));
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let s = RaftSharedSecret::new(b"top-secret".to_vec());
        let tag = s.sign(b"hello");
        assert!(!s.verify(b"hellp", &tag));
    }

    #[test]
    fn verify_rejects_tampered_tag() {
        let s = RaftSharedSecret::new(b"top-secret".to_vec());
        let mut tag = s.sign(b"hello");
        tag[0] ^= 0xFF;
        assert!(!s.verify(b"hello", &tag));
    }

    #[test]
    fn different_secrets_produce_different_tags() {
        let a = RaftSharedSecret::new(b"key-A".to_vec());
        let b = RaftSharedSecret::new(b"key-B".to_vec());
        assert_ne!(a.sign(b"payload"), b.sign(b"payload"));
        assert!(!a.verify(b"payload", &b.sign(b"payload")));
    }

    #[test]
    fn encode_decode_roundtrips() {
        let s = RaftSharedSecret::new(b"k".to_vec());
        let frame = encode_authenticated(&s, b"important-payload");
        let payload = decode_authenticated(&s, &frame).unwrap();
        assert_eq!(payload, b"important-payload");
    }

    #[test]
    fn decode_rejects_truncated_frame() {
        let s = RaftSharedSecret::new(b"k".to_vec());
        let mut frame = encode_authenticated(&s, b"payload");
        frame.truncate(frame.len() - 5);
        assert!(decode_authenticated(&s, &frame).is_none());
    }

    #[test]
    fn decode_rejects_wrong_secret() {
        let a = RaftSharedSecret::new(b"k1".to_vec());
        let b = RaftSharedSecret::new(b"k2".to_vec());
        let frame = encode_authenticated(&a, b"hi");
        assert!(decode_authenticated(&b, &frame).is_none());
    }
}
