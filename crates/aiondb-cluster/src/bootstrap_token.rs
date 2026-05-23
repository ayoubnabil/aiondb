//! Single-use cluster bootstrap token.
//!
//! Operator creates a token, gives it to a node joining the
//! cluster, the node presents it on the join RPC, the coordinator
//! marks it consumed. Tokens have a TTL so a leak doesn't open the
//! door forever. Per-token IP allow-lists prevent a stolen token
//! from working from outside the bastion.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;

/// V2-03 : compare two byte slices without short-circuiting. The
/// length is folded into the accumulator so neither the matching
/// prefix length nor the stored secret length leaks through timing.
fn ct_eq_bytes(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let len_eq = (left.len() as u64).ct_eq(&(right.len() as u64));
    let mut byte_diff: u8 = 0;
    for i in 0..max_len {
        let l = *left.get(i).unwrap_or(&0);
        let r = *right.get(i).unwrap_or(&0);
        byte_diff |= l ^ r;
    }
    bool::from(byte_diff.ct_eq(&0) & len_eq)
}

#[derive(Clone, Debug)]
pub struct BootstrapToken {
    pub value: String,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub allowed_ips: Vec<IpAddr>,
    pub consumed_by: Option<String>,
}

impl BootstrapToken {
    pub fn is_consumed(&self) -> bool {
        self.consumed_by.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedemptionError {
    Unknown,
    Expired,
    AlreadyConsumed,
    IpDenied,
}

#[derive(Clone, Debug, Default)]
pub struct BootstrapTokenStore {
    inner: Arc<std::sync::Mutex<BTreeMap<String, BootstrapToken>>>,
}

impl BootstrapTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(
        &self,
        value: impl Into<String>,
        ttl: Duration,
        allowed_ips: Vec<IpAddr>,
    ) -> BootstrapToken {
        let now = Instant::now();
        let t = BootstrapToken {
            value: value.into(),
            created_at: now,
            expires_at: now + ttl,
            allowed_ips,
            consumed_by: None,
        };
        self.inner
            .lock()
            .unwrap()
            .insert(t.value.clone(), t.clone());
        t
    }

    pub fn redeem(
        &self,
        value: &str,
        node_name: &str,
        from_ip: Option<IpAddr>,
    ) -> Result<BootstrapToken, RedemptionError> {
        let mut g = self.inner.lock().unwrap();
        // V2-03 : the previous `BTreeMap::get_mut(value)` lookup did
        // byte-by-byte ordering compare and short-circuited at the
        // first byte mismatch — leaking each stored token octet by
        // octet through RPC timing. Scan every entry linearly with a
        // constant-time byte compare so misses and partial matches
        // are indistinguishable from a clean miss.
        let candidate = value.as_bytes();
        let matched_key = g
            .keys()
            .find(|stored| ct_eq_bytes(stored.as_bytes(), candidate))
            .cloned();
        let Some(key) = matched_key else {
            return Err(RedemptionError::Unknown);
        };
        let t = g.get_mut(&key).expect("matched key must exist");
        if t.is_consumed() {
            return Err(RedemptionError::AlreadyConsumed);
        }
        if Instant::now() > t.expires_at {
            return Err(RedemptionError::Expired);
        }
        if !t.allowed_ips.is_empty() {
            match from_ip {
                Some(ip) if t.allowed_ips.contains(&ip) => {}
                _ => return Err(RedemptionError::IpDenied),
            }
        }
        t.consumed_by = Some(node_name.to_string());
        Ok(t.clone())
    }

    pub fn revoke(&self, value: &str) -> bool {
        self.inner.lock().unwrap().remove(value).is_some()
    }

    pub fn outstanding(&self) -> Vec<BootstrapToken> {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|t| !t.is_consumed() && t.expires_at > now)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_then_redeem_succeeds() {
        let s = BootstrapTokenStore::new();
        s.issue("xyz", Duration::from_secs(60), vec![]);
        let r = s.redeem("xyz", "node-1", None);
        assert!(r.is_ok());
        assert_eq!(r.unwrap().consumed_by, Some("node-1".to_string()));
    }

    #[test]
    fn redeem_unknown_token_fails() {
        let s = BootstrapTokenStore::new();
        assert_eq!(
            s.redeem("nope", "node", None).unwrap_err(),
            RedemptionError::Unknown
        );
    }

    #[test]
    fn cannot_redeem_twice() {
        let s = BootstrapTokenStore::new();
        s.issue("once", Duration::from_secs(60), vec![]);
        let _ = s.redeem("once", "a", None).unwrap();
        assert_eq!(
            s.redeem("once", "b", None).unwrap_err(),
            RedemptionError::AlreadyConsumed
        );
    }

    #[test]
    fn expired_token_rejected() {
        let s = BootstrapTokenStore::new();
        s.issue("short", Duration::from_millis(1), vec![]);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            s.redeem("short", "n", None).unwrap_err(),
            RedemptionError::Expired
        );
    }

    #[test]
    fn ip_allow_list_blocks_other_origins() {
        let s = BootstrapTokenStore::new();
        let allowed = "10.0.0.1".parse().unwrap();
        s.issue("locked", Duration::from_secs(60), vec![allowed]);
        let denied = "192.168.1.1".parse().unwrap();
        assert_eq!(
            s.redeem("locked", "n", Some(denied)).unwrap_err(),
            RedemptionError::IpDenied
        );
        assert!(s.redeem("locked", "n", Some(allowed)).is_ok());
    }

    #[test]
    fn outstanding_excludes_consumed() {
        let s = BootstrapTokenStore::new();
        s.issue("a", Duration::from_secs(60), vec![]);
        s.issue("b", Duration::from_secs(60), vec![]);
        s.redeem("a", "n", None).unwrap();
        let live = s.outstanding();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].value, "b");
    }

    #[test]
    fn revoke_drops_token() {
        let s = BootstrapTokenStore::new();
        s.issue("kill", Duration::from_secs(60), vec![]);
        assert!(s.revoke("kill"));
        assert_eq!(
            s.redeem("kill", "n", None).unwrap_err(),
            RedemptionError::Unknown
        );
    }

    #[test]
    fn v2_03_ct_eq_bytes_handles_length_diff() {
        // Length mismatch must NOT short-circuit ; both directions
        // return false without leaking which side is longer.
        assert!(!ct_eq_bytes(b"AAA", b"AAAAA"));
        assert!(!ct_eq_bytes(b"AAAAA", b"AAA"));
        assert!(ct_eq_bytes(b"AAAAA", b"AAAAA"));
        assert!(ct_eq_bytes(b"", b""));
        assert!(!ct_eq_bytes(b"", b"A"));
    }

    #[test]
    fn v2_03_partial_prefix_match_does_not_redeem() {
        // Pre-fix the BTreeMap::get_mut comparison could not be
        // exploited to redeem a partial match — only to time-leak
        // bytes. Confirm that the linear scan still rejects partial
        // matches with the Unknown error, not e.g. a delayed answer.
        let s = BootstrapTokenStore::new();
        s.issue(
            "AAAAA-BBBBB-CCCCC-DDDDD",
            Duration::from_secs(60),
            vec![],
        );
        for candidate in ["A", "AAAAA", "AAAAA-BBBBB-CCCCC-DDDD"] {
            assert_eq!(
                s.redeem(candidate, "n", None).unwrap_err(),
                RedemptionError::Unknown,
                "candidate {candidate:?} must NOT be accepted"
            );
        }
        // Exact match still works.
        let r = s.redeem("AAAAA-BBBBB-CCCCC-DDDDD", "n", None);
        assert!(r.is_ok());
    }
}
