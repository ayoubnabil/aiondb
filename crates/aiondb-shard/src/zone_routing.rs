//! Zone-aware read routing.
//!
//! Combines :
//!
//! - Per-shard preferred zones (e.g. "lease this shard in `eu` if
//!   possible").
//! - Per-replica zone tags (region, az, rack).
//! - Per-request client zone (where the connection originated).
//!
//! Produces a ranked list of candidate replicas for a read. Used by
//! the DistSender to bias follower reads toward the nearest region.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::ReplicaId;

#[derive(Clone, Debug, Default)]
pub struct ZoneRouter {
    /// `replica_id -> zone tag`. Populated from the catalog at boot.
    replica_zones: Arc<std::sync::Mutex<BTreeMap<ReplicaId, String>>>,
}

impl ZoneRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_zone(&self, replica: ReplicaId, zone: impl Into<String>) {
        self.replica_zones
            .lock()
            .unwrap()
            .insert(replica, zone.into());
    }

    pub fn zone_of(&self, replica: ReplicaId) -> Option<String> {
        self.replica_zones.lock().unwrap().get(&replica).cloned()
    }

    /// Rank replicas by zone affinity with the client. Replicas in
    /// the client's zone come first, then anything else in the same
    /// region, then the rest. Ties broken by replica id for stable
    /// output.
    pub fn rank_for_client(&self, client_zone: &str, replicas: &[ReplicaId]) -> Vec<ReplicaId> {
        let guard = self.replica_zones.lock().unwrap();
        let mut scored: Vec<(u8, ReplicaId)> = replicas
            .iter()
            .map(|r| {
                let zone = guard.get(r).map(String::as_str).unwrap_or("");
                let score = score(client_zone, zone);
                (score, *r)
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, r)| r).collect()
    }
}

/// 0 = exact match, 1 = same region prefix, 2 = unknown/other.
fn score(client: &str, candidate: &str) -> u8 {
    if client == candidate {
        return 0;
    }
    let client_region = client.split('-').next().unwrap_or("");
    let cand_region = candidate.split('-').next().unwrap_or("");
    if !client_region.is_empty() && client_region == cand_region {
        return 1;
    }
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(n: u64) -> ReplicaId {
        ReplicaId::new(n)
    }

    #[test]
    fn exact_zone_match_ranks_first() {
        let r = ZoneRouter::new();
        r.set_zone(rep(1), "eu-west-1");
        r.set_zone(rep(2), "us-east-1");
        let order = r.rank_for_client("eu-west-1", &[rep(1), rep(2)]);
        assert_eq!(order, vec![rep(1), rep(2)]);
    }

    #[test]
    fn same_region_beats_unrelated() {
        let r = ZoneRouter::new();
        r.set_zone(rep(1), "eu-east-2"); // same region eu
        r.set_zone(rep(2), "us-west-1"); // different region
        r.set_zone(rep(3), "eu-west-1"); // same region
        let order = r.rank_for_client("eu-west-1", &[rep(1), rep(2), rep(3)]);
        assert_eq!(order, vec![rep(3), rep(1), rep(2)]);
    }

    #[test]
    fn unknown_zone_lands_at_the_back() {
        let r = ZoneRouter::new();
        r.set_zone(rep(1), "eu-west-1");
        // rep(2) has no zone tag.
        let order = r.rank_for_client("eu-west-1", &[rep(1), rep(2)]);
        assert_eq!(order[0], rep(1));
        assert_eq!(order[1], rep(2));
    }

    #[test]
    fn empty_replica_set_returns_empty_ranking() {
        let r = ZoneRouter::new();
        let order = r.rank_for_client("eu-west-1", &[]);
        assert!(order.is_empty());
    }

    #[test]
    fn deterministic_when_scores_tie() {
        let r = ZoneRouter::new();
        r.set_zone(rep(2), "ap-south-1");
        r.set_zone(rep(1), "ap-south-1");
        let order = r.rank_for_client("ap-south-1", &[rep(2), rep(1)]);
        // Both score 0; smaller replica id wins the tie-break.
        assert_eq!(order, vec![rep(1), rep(2)]);
    }
}
