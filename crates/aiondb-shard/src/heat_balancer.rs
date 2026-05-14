//! Replica heat balancer.
//!
//! Given current per-node QPS and the set of leaseholder ranges
//! each node holds, recommends lease transfers from hot to cold
//! nodes. The objective is to equalise per-node QPS without
//! exceeding the network/transfer budget.
//!
//! The algorithm is greedy : iterate from the hottest node to the
//! coldest, transfer the leases that contribute the most heat until
//! the node sits at or below the cluster mean.

use std::collections::BTreeMap;

use crate::range_descriptor::RangeId;

#[derive(Clone, Copy, Debug)]
pub struct RangeHeat {
    pub range: RangeId,
    pub qps: f64,
}

#[derive(Clone, Debug)]
pub struct NodeReport {
    pub node_id: u64,
    pub ranges: Vec<RangeHeat>,
}

impl NodeReport {
    pub fn total_qps(&self) -> f64 {
        self.ranges.iter().map(|r| r.qps).sum()
    }
}

#[derive(Clone, Debug)]
pub struct LeaseTransfer {
    pub range: RangeId,
    pub from_node: u64,
    pub to_node: u64,
    pub qps_transferred: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct HeatBalancerConfig {
    pub max_transfers: usize,
    pub imbalance_threshold: f64,
}

impl Default for HeatBalancerConfig {
    fn default() -> Self {
        Self {
            max_transfers: 16,
            imbalance_threshold: 0.20,
        }
    }
}

pub fn plan_balancing(reports: Vec<NodeReport>, cfg: HeatBalancerConfig) -> Vec<LeaseTransfer> {
    if reports.len() < 2 {
        return Vec::new();
    }
    let total: f64 = reports.iter().map(|n| n.total_qps()).sum();
    let mean = total / reports.len() as f64;
    if mean <= 0.0 {
        return Vec::new();
    }
    let threshold = mean * (1.0 + cfg.imbalance_threshold);
    let mut work: BTreeMap<u64, NodeReport> = reports.into_iter().map(|n| (n.node_id, n)).collect();
    let mut plan = Vec::new();
    let mut transfers_done = 0usize;

    loop {
        if transfers_done >= cfg.max_transfers {
            break;
        }
        let (hottest_id, coldest_id) = {
            let mut entries: Vec<(u64, f64)> =
                work.values().map(|n| (n.node_id, n.total_qps())).collect();
            entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let (hot_id, hot_qps) = entries.first().copied().unwrap();
            let (cold_id, cold_qps) = entries.last().copied().unwrap();
            if hot_qps <= threshold {
                break;
            }
            if hot_qps - cold_qps <= 0.0 {
                break;
            }
            (hot_id, cold_id)
        };
        let to_move = {
            let hot = work.get(&hottest_id).unwrap();
            hot.ranges
                .iter()
                .max_by(|a, b| a.qps.partial_cmp(&b.qps).unwrap())
                .copied()
        };
        let Some(range_heat) = to_move else {
            break;
        };
        if range_heat.qps <= 0.0 {
            break;
        }
        // Update bookkeeping.
        {
            let hot = work.get_mut(&hottest_id).unwrap();
            hot.ranges.retain(|r| r.range != range_heat.range);
        }
        {
            let cold = work.get_mut(&coldest_id).unwrap();
            cold.ranges.push(range_heat);
        }
        plan.push(LeaseTransfer {
            range: range_heat.range,
            from_node: hottest_id,
            to_node: coldest_id,
            qps_transferred: range_heat.qps,
        });
        transfers_done += 1;
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn no_plan_with_one_node() {
        let plan = plan_balancing(
            vec![NodeReport {
                node_id: 1,
                ranges: vec![RangeHeat {
                    range: r(1),
                    qps: 100.0,
                }],
            }],
            HeatBalancerConfig::default(),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn balanced_cluster_produces_no_plan() {
        let plan = plan_balancing(
            vec![
                NodeReport {
                    node_id: 1,
                    ranges: vec![RangeHeat {
                        range: r(1),
                        qps: 50.0,
                    }],
                },
                NodeReport {
                    node_id: 2,
                    ranges: vec![RangeHeat {
                        range: r(2),
                        qps: 50.0,
                    }],
                },
            ],
            HeatBalancerConfig::default(),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn hot_node_offloads_to_cold_node() {
        let plan = plan_balancing(
            vec![
                NodeReport {
                    node_id: 1,
                    ranges: vec![
                        RangeHeat {
                            range: r(1),
                            qps: 100.0,
                        },
                        RangeHeat {
                            range: r(2),
                            qps: 100.0,
                        },
                    ],
                },
                NodeReport {
                    node_id: 2,
                    ranges: vec![],
                },
            ],
            HeatBalancerConfig::default(),
        );
        assert!(!plan.is_empty());
        assert_eq!(plan[0].from_node, 1);
        assert_eq!(plan[0].to_node, 2);
    }

    #[test]
    fn max_transfers_is_respected() {
        let mut hot_ranges = Vec::new();
        for i in 0..50u64 {
            hot_ranges.push(RangeHeat {
                range: r(i),
                qps: 10.0,
            });
        }
        let plan = plan_balancing(
            vec![
                NodeReport {
                    node_id: 1,
                    ranges: hot_ranges,
                },
                NodeReport {
                    node_id: 2,
                    ranges: vec![],
                },
            ],
            HeatBalancerConfig {
                max_transfers: 4,
                imbalance_threshold: 0.0,
            },
        );
        assert!(plan.len() <= 4);
    }

    #[test]
    fn imbalance_threshold_filters_minor_skew() {
        let plan = plan_balancing(
            vec![
                NodeReport {
                    node_id: 1,
                    ranges: vec![RangeHeat {
                        range: r(1),
                        qps: 105.0,
                    }],
                },
                NodeReport {
                    node_id: 2,
                    ranges: vec![RangeHeat {
                        range: r(2),
                        qps: 95.0,
                    }],
                },
            ],
            HeatBalancerConfig {
                max_transfers: 16,
                imbalance_threshold: 0.5,
            },
        );
        assert!(plan.is_empty());
    }
}
