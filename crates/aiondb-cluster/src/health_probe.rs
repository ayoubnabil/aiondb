//! Cluster health probe.
//!
//! Exposes `/readyz` and `/healthz` semantics as pure functions. The
//! HTTP wiring lives in `aiondb-server`; this module owns the logic
//! so it stays unit-testable.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason_code: u32 },
    Down { reason_code: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessStatus {
    Ready,
    NotReady { reason_code: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct ProbeInputs {
    pub raft_leader_present: bool,
    pub gossip_alive_count: usize,
    pub gossip_total_count: usize,
    pub last_apply_lag_entries: u64,
    pub draining: bool,
}

pub fn evaluate_healthz(p: &ProbeInputs) -> HealthStatus {
    if p.draining {
        return HealthStatus::Down { reason_code: 1001 };
    }
    if !p.raft_leader_present {
        return HealthStatus::Down { reason_code: 1002 };
    }
    if p.gossip_total_count > 0 && p.gossip_alive_count * 2 < p.gossip_total_count {
        return HealthStatus::Degraded { reason_code: 1003 };
    }
    if p.last_apply_lag_entries > 1_000_000 {
        return HealthStatus::Degraded { reason_code: 1004 };
    }
    HealthStatus::Healthy
}

pub fn evaluate_readyz(p: &ProbeInputs) -> ReadinessStatus {
    if p.draining {
        return ReadinessStatus::NotReady { reason_code: 2001 };
    }
    if !p.raft_leader_present {
        return ReadinessStatus::NotReady { reason_code: 2002 };
    }
    if p.last_apply_lag_entries > 100_000 {
        return ReadinessStatus::NotReady { reason_code: 2003 };
    }
    ReadinessStatus::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> ProbeInputs {
        ProbeInputs {
            raft_leader_present: true,
            gossip_alive_count: 3,
            gossip_total_count: 3,
            last_apply_lag_entries: 0,
            draining: false,
        }
    }

    #[test]
    fn healthy_passes_both_probes() {
        let p = healthy();
        assert_eq!(evaluate_healthz(&p), HealthStatus::Healthy);
        assert_eq!(evaluate_readyz(&p), ReadinessStatus::Ready);
    }

    #[test]
    fn no_leader_fails_both() {
        let mut p = healthy();
        p.raft_leader_present = false;
        assert!(matches!(evaluate_healthz(&p), HealthStatus::Down { .. }));
        assert!(matches!(
            evaluate_readyz(&p),
            ReadinessStatus::NotReady { .. }
        ));
    }

    #[test]
    fn draining_fails_both() {
        let mut p = healthy();
        p.draining = true;
        assert!(matches!(evaluate_healthz(&p), HealthStatus::Down { .. }));
        assert!(matches!(
            evaluate_readyz(&p),
            ReadinessStatus::NotReady { .. }
        ));
    }

    #[test]
    fn high_apply_lag_degrades_healthz() {
        let mut p = healthy();
        p.last_apply_lag_entries = 5_000_000;
        assert!(matches!(
            evaluate_healthz(&p),
            HealthStatus::Degraded { .. }
        ));
    }

    #[test]
    fn medium_apply_lag_breaks_readyz_only() {
        let mut p = healthy();
        p.last_apply_lag_entries = 500_000;
        assert!(matches!(
            evaluate_readyz(&p),
            ReadinessStatus::NotReady { .. }
        ));
        assert_eq!(evaluate_healthz(&p), HealthStatus::Healthy);
    }
}
