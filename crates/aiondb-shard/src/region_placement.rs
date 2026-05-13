//! Region-aware replica placement.
//!
//! Picks N replicas from a pool of candidate nodes such that :
//!
//! - At least `region_diversity` distinct regions are covered when
//!   the cluster has the diversity available.
//! - The chosen set minimises **maximum RTT** to a designated client
//!   region (useful for "regional read locality" placements).
//! - Required attribute constraints (env=prod, ...) are honoured.
//!
//! This is a deterministic, greedy planner. It does NOT replace
//! Raft membership changes : it produces the *recommendation*
//! plan that an admin job consumes via [`crate::range_relocator`].

use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::PlacementAttributeConstraint;

/// One candidate node with its region + attribute tags.
#[derive(Clone, Debug)]
pub struct CandidateNode {
    pub node_id: String,
    pub region: String,
    pub attributes: BTreeMap<String, String>,
}

/// RTT matrix : `(src_region, dst_region) -> RTT`. Missing entries
/// default to `Duration::MAX`.
#[derive(Clone, Debug, Default)]
pub struct RegionRttMatrix {
    rtts: BTreeMap<(String, String), Duration>,
}

impl RegionRttMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, src: impl Into<String>, dst: impl Into<String>, rtt: Duration) {
        self.rtts.insert((src.into(), dst.into()), rtt);
    }

    pub fn get(&self, src: &str, dst: &str) -> Duration {
        if src == dst {
            return Duration::ZERO;
        }
        self.rtts
            .get(&(src.to_owned(), dst.to_owned()))
            .copied()
            .unwrap_or(Duration::MAX)
    }
}

#[derive(Clone, Debug)]
pub struct RegionPlacementRequest {
    pub replica_count: usize,
    pub client_region: String,
    pub required_attributes: Vec<PlacementAttributeConstraint>,
    pub region_diversity: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionPlacementPlan {
    pub picked_nodes: Vec<String>,
    pub regions_covered: Vec<String>,
    pub max_rtt: Duration,
}

pub fn plan_placement(
    candidates: &[CandidateNode],
    matrix: &RegionRttMatrix,
    req: &RegionPlacementRequest,
) -> RegionPlacementPlan {
    if req.replica_count == 0 || candidates.is_empty() {
        return RegionPlacementPlan {
            picked_nodes: Vec::new(),
            regions_covered: Vec::new(),
            max_rtt: Duration::ZERO,
        };
    }

    // Filter by required attributes.
    let eligible: Vec<&CandidateNode> = candidates
        .iter()
        .filter(|c| {
            req.required_attributes.iter().all(|constraint| {
                c.attributes
                    .get(&constraint.key)
                    .map(|v| v == &constraint.value)
                    .unwrap_or(false)
            })
        })
        .collect();
    if eligible.is_empty() {
        return RegionPlacementPlan {
            picked_nodes: Vec::new(),
            regions_covered: Vec::new(),
            max_rtt: Duration::MAX,
        };
    }

    // Score each candidate : lower RTT to client region = better.
    let mut scored: Vec<(Duration, &CandidateNode)> = eligible
        .iter()
        .map(|c| (matrix.get(&c.region, &req.client_region), *c))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.node_id.cmp(&b.1.node_id)));

    // Greedy picks : prefer candidate that EITHER reduces max RTT or
    // adds a new region to the covered set, until N replicas chosen.
    let mut picked: Vec<&CandidateNode> = Vec::with_capacity(req.replica_count);
    let mut covered_regions: Vec<String> = Vec::new();
    let mut max_rtt = Duration::ZERO;
    for (rtt, candidate) in &scored {
        if picked.len() == req.replica_count {
            break;
        }
        let new_region = !covered_regions.contains(&candidate.region);
        let need_diversity = covered_regions.len() < req.region_diversity;
        if need_diversity && !new_region {
            continue;
        }
        picked.push(candidate);
        if new_region {
            covered_regions.push(candidate.region.clone());
        }
        if *rtt > max_rtt {
            max_rtt = *rtt;
        }
    }
    // If we still don't have enough replicas (cluster lacks diversity),
    // fill the remaining slots from the next-best candidates ignoring
    // diversity.
    if picked.len() < req.replica_count {
        for (rtt, candidate) in &scored {
            if picked.len() == req.replica_count {
                break;
            }
            if picked.iter().any(|p| p.node_id == candidate.node_id) {
                continue;
            }
            picked.push(candidate);
            if !covered_regions.contains(&candidate.region) {
                covered_regions.push(candidate.region.clone());
            }
            if *rtt > max_rtt {
                max_rtt = *rtt;
            }
        }
    }
    RegionPlacementPlan {
        picked_nodes: picked.iter().map(|c| c.node_id.clone()).collect(),
        regions_covered: covered_regions,
        max_rtt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, region: &str, attrs: &[(&str, &str)]) -> CandidateNode {
        let mut a = BTreeMap::new();
        for (k, v) in attrs {
            a.insert((*k).to_owned(), (*v).to_owned());
        }
        CandidateNode {
            node_id: id.to_owned(),
            region: region.to_owned(),
            attributes: a,
        }
    }

    fn matrix() -> RegionRttMatrix {
        let mut m = RegionRttMatrix::new();
        m.set("eu", "us", Duration::from_millis(80));
        m.set("us", "eu", Duration::from_millis(80));
        m.set("eu", "ap", Duration::from_millis(180));
        m.set("ap", "eu", Duration::from_millis(180));
        m.set("us", "ap", Duration::from_millis(150));
        m.set("ap", "us", Duration::from_millis(150));
        m
    }

    #[test]
    fn diversity_constraint_is_honoured() {
        let nodes = [
            candidate("a", "eu", &[]),
            candidate("b", "eu", &[]),
            candidate("c", "us", &[]),
            candidate("d", "ap", &[]),
        ];
        let req = RegionPlacementRequest {
            replica_count: 3,
            client_region: "eu".into(),
            required_attributes: Vec::new(),
            region_diversity: 3,
        };
        let plan = plan_placement(&nodes, &matrix(), &req);
        let regions: std::collections::BTreeSet<_> = plan.regions_covered.iter().collect();
        assert_eq!(regions.len(), 3);
    }

    #[test]
    fn required_attributes_filter_pool() {
        let nodes = [
            candidate("a", "eu", &[("env", "prod")]),
            candidate("b", "us", &[("env", "staging")]),
            candidate("c", "ap", &[("env", "prod")]),
        ];
        let req = RegionPlacementRequest {
            replica_count: 2,
            client_region: "eu".into(),
            required_attributes: vec![PlacementAttributeConstraint {
                key: "env".into(),
                value: "prod".into(),
            }],
            region_diversity: 2,
        };
        let plan = plan_placement(&nodes, &matrix(), &req);
        assert!(plan.picked_nodes.contains(&"a".to_owned()));
        assert!(plan.picked_nodes.contains(&"c".to_owned()));
        assert!(!plan.picked_nodes.contains(&"b".to_owned()));
    }

    #[test]
    fn falls_back_when_diversity_unavailable() {
        let nodes = [
            candidate("a", "eu", &[]),
            candidate("b", "eu", &[]),
            candidate("c", "eu", &[]),
        ];
        let req = RegionPlacementRequest {
            replica_count: 3,
            client_region: "eu".into(),
            required_attributes: Vec::new(),
            region_diversity: 3,
        };
        let plan = plan_placement(&nodes, &matrix(), &req);
        assert_eq!(plan.picked_nodes.len(), 3);
    }

    #[test]
    fn empty_pool_returns_empty_plan() {
        let req = RegionPlacementRequest {
            replica_count: 3,
            client_region: "eu".into(),
            required_attributes: Vec::new(),
            region_diversity: 1,
        };
        let plan = plan_placement(&[], &matrix(), &req);
        assert!(plan.picked_nodes.is_empty());
    }

    #[test]
    fn client_region_picks_minimise_max_rtt() {
        let nodes = [
            candidate("eu1", "eu", &[]),
            candidate("us1", "us", &[]),
            candidate("ap1", "ap", &[]),
        ];
        let req = RegionPlacementRequest {
            replica_count: 2,
            client_region: "eu".into(),
            required_attributes: Vec::new(),
            region_diversity: 1, // diversity not required
        };
        let plan = plan_placement(&nodes, &matrix(), &req);
        // Best two for eu : eu1 (0ms) + us1 (80ms). ap1 (180ms) would
        // raise max RTT.
        assert!(plan.picked_nodes.contains(&"eu1".to_owned()));
        assert!(plan.picked_nodes.contains(&"us1".to_owned()));
        assert_eq!(plan.max_rtt, Duration::from_millis(80));
    }
}
