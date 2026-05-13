//! Replica proximity filter.
//!
//! Ranks read candidates by topology proximity (same zone > same
//! region > cross-region). The follower-reads path uses this to
//! avoid the WAN hop when a same-region replica can serve the read.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeLocality {
    pub region: String,
    pub zone: String,
}

impl NodeLocality {
    pub fn new(region: impl Into<String>, zone: impl Into<String>) -> Self {
        Self {
            region: region.into(),
            zone: zone.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ProximityTier {
    SameZone = 0,
    SameRegion = 1,
    CrossRegion = 2,
}

pub fn classify(local: &NodeLocality, peer: &NodeLocality) -> ProximityTier {
    if local.zone == peer.zone && local.region == peer.region {
        ProximityTier::SameZone
    } else if local.region == peer.region {
        ProximityTier::SameRegion
    } else {
        ProximityTier::CrossRegion
    }
}

#[derive(Clone, Debug)]
pub struct ReplicaCandidate<T> {
    pub id: T,
    pub locality: NodeLocality,
}

pub fn rank<T: Clone + Ord>(
    local: &NodeLocality,
    candidates: &[ReplicaCandidate<T>],
) -> Vec<ReplicaCandidate<T>> {
    let mut by_tier: BTreeMap<ProximityTier, Vec<ReplicaCandidate<T>>> = BTreeMap::new();
    for c in candidates {
        let t = classify(local, &c.locality);
        by_tier.entry(t).or_default().push(c.clone());
    }
    let mut out = Vec::with_capacity(candidates.len());
    for (_, mut bucket) in by_tier {
        bucket.sort_by(|a, b| a.id.cmp(&b.id));
        out.extend(bucket);
    }
    out
}

pub fn closest_in_tier<T: Clone>(
    local: &NodeLocality,
    candidates: &[ReplicaCandidate<T>],
    max_tier: ProximityTier,
) -> Option<ReplicaCandidate<T>> {
    candidates
        .iter()
        .filter(|c| classify(local, &c.locality) <= max_tier)
        .min_by_key(|c| classify(local, &c.locality))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(r: &str, z: &str) -> NodeLocality {
        NodeLocality::new(r, z)
    }

    fn c(id: u64, region: &str, zone: &str) -> ReplicaCandidate<u64> {
        ReplicaCandidate {
            id,
            locality: loc(region, zone),
        }
    }

    #[test]
    fn same_zone_is_top() {
        let local = loc("us", "us-a");
        let r = rank(
            &local,
            &[c(1, "us", "us-b"), c(2, "us", "us-a"), c(3, "eu", "eu-a")],
        );
        assert_eq!(r[0].id, 2);
    }

    #[test]
    fn cross_region_is_bottom() {
        let local = loc("us", "us-a");
        let r = rank(&local, &[c(1, "eu", "eu-a"), c(2, "us", "us-b")]);
        assert_eq!(r.last().unwrap().id, 1);
    }

    #[test]
    fn classify_same_zone() {
        assert_eq!(
            classify(&loc("us", "a"), &loc("us", "a")),
            ProximityTier::SameZone
        );
    }

    #[test]
    fn classify_same_region() {
        assert_eq!(
            classify(&loc("us", "a"), &loc("us", "b")),
            ProximityTier::SameRegion
        );
    }

    #[test]
    fn classify_cross_region() {
        assert_eq!(
            classify(&loc("us", "a"), &loc("eu", "a")),
            ProximityTier::CrossRegion
        );
    }

    #[test]
    fn closest_in_tier_respects_max() {
        let local = loc("us", "a");
        let cands = vec![c(1, "eu", "x"), c(2, "us", "b")];
        let r = closest_in_tier(&local, &cands, ProximityTier::SameRegion);
        assert_eq!(r.unwrap().id, 2);
    }

    #[test]
    fn empty_ranking_returns_empty() {
        let local = loc("us", "a");
        let r: Vec<ReplicaCandidate<u64>> = rank(&local, &[]);
        assert!(r.is_empty());
    }
}
