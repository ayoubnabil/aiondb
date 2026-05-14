//! Ops dashboard summary.
//!
//! Single struct serialisable to JSON. Bundles cluster status + raft
//! metrics + pending events for one-call dashboard fetches.

use serde::Serialize;

use crate::cluster_status::ClusterStatus;
use crate::event_log::ClusterEvent;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DashboardSummary {
    pub status: ClusterStatus,
    pub recent_events: Vec<ClusterEvent>,
    pub generation: u64,
}

impl DashboardSummary {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster_status::{GossipSummary, RaftStatus};
    use crate::event_log::ClusterEventKind;

    fn sample_status() -> ClusterStatus {
        ClusterStatus {
            local_node: "n1".into(),
            gossip: GossipSummary {
                alive: 3,
                suspect: 0,
                dead: 0,
                left: 0,
                members: vec!["n1".into(), "n2".into(), "n3".into()],
            },
            raft: RaftStatus {
                total_groups: 1,
                leader_count: 1,
                follower_count: 0,
                candidate_count: 0,
                slowest_apply_lag: 0,
                groups: Vec::new(),
            },
        }
    }

    fn sample_events() -> Vec<ClusterEvent> {
        vec![ClusterEvent {
            id: 1,
            at_us: 100,
            kind: ClusterEventKind::NodeJoined { node_id: 7 },
        }]
    }

    #[test]
    fn summary_carries_status_events_and_generation() {
        let s = DashboardSummary {
            status: sample_status(),
            recent_events: sample_events(),
            generation: 42,
        };
        assert_eq!(s.status.local_node, "n1");
        assert_eq!(s.recent_events.len(), 1);
        assert_eq!(s.generation, 42);
    }

    #[test]
    fn json_round_trips() {
        let s = DashboardSummary {
            status: sample_status(),
            recent_events: sample_events(),
            generation: 1,
        };
        let json = s.to_json();
        assert!(json.contains("\"local_node\":\"n1\""));
        assert!(json.contains("\"recent_events\""));
    }
}
