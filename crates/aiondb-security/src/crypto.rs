//! Shared SHA-256 / HMAC-SHA-256 helpers.
//!
//! Both the SCRAM verifier and the catalog auth policy hash internal
//! material with the same primitive. Keeping a single helper here prevents
//! either side from drifting (different error messages, accidental keyed
//! vs. unkeyed hashing, etc.).

use aiondb_core::{DbError, DbResult};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA-256(key, data). Returns an internal error if the underlying
/// MAC initialisation fails (it cannot, in practice, with HMAC-SHA-256).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> DbResult<[u8; 32]> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| DbError::internal("failed to initialise HMAC-SHA-256"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().into())
}

/// SHA-256(data).
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}
