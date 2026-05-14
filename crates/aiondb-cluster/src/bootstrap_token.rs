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
        let Some(t) = g.get_mut(value) else {
            return Err(RedemptionError::Unknown);
        };
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
}
