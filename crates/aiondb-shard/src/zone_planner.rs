//! Multi-region / multi-zone replica placement enforcement.
//!
//! [`ShardingConfig`] already declares **what** an operator wants in
//! terms of replica placement (required attributes, spread attributes,
//! lease preferences). This module enforces **how** that intent is
//! translated into concrete node selections.
//!
//! The planner is intentionally read-only: it validates and ranks
//! placements but never mutates the cluster. Callers feed it the
//! current cluster topology (the node-id → attribute map declared in
//! [`ShardingConfig::node_attributes`]) plus a candidate placement,
//! and the planner answers:
//!
//! 1. Does every replica satisfy the required attributes?
//! 2. Are the replicas sufficiently spread across declared failure
//!    domains (region / zone / rack ...)?
//! 3. Which node is the most appropriate leaseholder according to the
//!    declared lease preferences?
//!
//! A naive [`ShardLeasePlanner::propose`] is also exposed to bootstrap
//! placements when no prior assignment exists: greedily picks one node
//! per spread attribute value to maximise diversity, then tops up with
//! whichever nodes minimise constraint violations.
//!
//! [`ShardingConfig`]: crate::config::ShardingConfig

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::config::{PlacementAttributeConstraint, ShardingConfig};

/// Attributes carried by a single node (e.g. `region=eu-west`,
/// `zone=eu-west-2a`, `rack=r17`). Borrowed map type so callers can
/// supply borrowed slices when validating.
pub type NodeAttributes = BTreeMap<String, String>;

/// Opaque node identifier as it appears in
/// `ShardingConfig::node_attributes`.
pub type NodeId = String;

/// A single thing wrong with a proposed placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlacementViolation {
    /// A candidate node was not present in the cluster attribute map.
    UnknownNode { node: NodeId },
    /// A required attribute was missing or had the wrong value on
    /// `node`. `expected` is the constraint value, `actual` may be
    /// `None` when the key was absent entirely.
    RequiredAttributeMismatch {
        node: NodeId,
        key: String,
        expected: String,
        actual: Option<String>,
    },
    /// Two or more replicas share the same value for a spread attribute
    /// when the cluster could have offered more diversity.
    InsufficientSpread {
        attribute: String,
        duplicate_value: String,
        nodes: Vec<NodeId>,
    },
    /// The proposed placement contains duplicate node ids -- a replica
    /// would land on the same node twice.
    DuplicateNode { node: NodeId },
    /// The proposed placement is empty.
    EmptyPlacement,
}

/// Verdict returned by [`ShardLeasePlanner::validate`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementVerdict {
    /// All violations found. Empty when the placement is valid.
    pub violations: Vec<PlacementViolation>,
}

impl PlacementVerdict {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Planner that wraps a [`ShardingConfig`] and answers placement
/// questions against the cluster's current topology snapshot.
#[derive(Clone, Debug)]
pub struct ShardLeasePlanner<'a> {
    config: &'a ShardingConfig,
}

impl<'a> ShardLeasePlanner<'a> {
    pub fn new(config: &'a ShardingConfig) -> Self {
        Self { config }
    }

    /// Lookup a node's attributes from the planner's config snapshot.
    pub fn node_attributes(&self, node: &str) -> Option<&BTreeMap<String, String>> {
        self.config.node_attributes.get(node)
    }

    /// Validate a candidate placement against required attributes,
    /// spread attributes and duplicate-node checks.
    pub fn validate(&self, placement: &[NodeId]) -> PlacementVerdict {
        let mut violations = Vec::new();

        if placement.is_empty() {
            violations.push(PlacementViolation::EmptyPlacement);
            return PlacementVerdict { violations };
        }

        // Duplicate-node detection.
        let mut seen = BTreeSet::new();
        for node in placement {
            if !seen.insert(node.clone()) {
                violations.push(PlacementViolation::DuplicateNode { node: node.clone() });
            }
        }

        // Required attribute checks per node.
        for node in placement {
            let attrs = match self.node_attributes(node) {
                Some(a) => a,
                None => {
                    violations.push(PlacementViolation::UnknownNode { node: node.clone() });
                    continue;
                }
            };
            for req in &self.config.placement_required_attributes {
                match attrs.get(&req.key) {
                    Some(v) if *v == req.value => {}
                    other => violations.push(PlacementViolation::RequiredAttributeMismatch {
                        node: node.clone(),
                        key: req.key.clone(),
                        expected: req.value.clone(),
                        actual: other.cloned(),
                    }),
                }
            }
        }

        // Spread checks: for each spread attribute, every value
        // should appear at most once.
        for spread_key in &self.config.placement_spread_attributes {
            let mut bucket: HashMap<String, Vec<NodeId>> = HashMap::new();
            for node in placement {
                let attrs = match self.node_attributes(node) {
                    Some(a) => a,
                    None => continue,
                };
                if let Some(value) = attrs.get(spread_key) {
                    bucket.entry(value.clone()).or_default().push(node.clone());
                }
            }
            for (value, nodes) in bucket {
                if nodes.len() > 1 {
                    let cluster_distinct_values = self.distinct_values_in_cluster(spread_key);
                    // Only flag insufficient spread when the cluster
                    // had more diverse values available. If every node
                    // already lives in the same `value`, the placement
                    // is best-effort and not a violation.
                    if cluster_distinct_values > 1 {
                        violations.push(PlacementViolation::InsufficientSpread {
                            attribute: spread_key.clone(),
                            duplicate_value: value,
                            nodes,
                        });
                    }
                }
            }
        }

        PlacementVerdict { violations }
    }

    /// Rank a placement's nodes by how well they match
    /// `lease_preference_attributes`. Returns nodes sorted by score
    /// descending; ties are broken alphabetically for stable output.
    pub fn rank_leaseholders(&self, placement: &[NodeId]) -> Vec<(NodeId, i32)> {
        let mut ranked: Vec<(NodeId, i32)> = placement
            .iter()
            .filter_map(|node| {
                self.node_attributes(node).map(|attrs| {
                    (
                        node.clone(),
                        score(attrs, &self.config.lease_preference_attributes),
                    )
                })
            })
            .collect();
        ranked.sort_by(|(an, ascore), (bn, bscore)| bscore.cmp(ascore).then_with(|| an.cmp(bn)));
        ranked
    }

    /// Propose an initial placement of `replicas` nodes that maximises
    /// diversity across declared spread attributes while preferring
    /// nodes that satisfy the required attributes.
    ///
    /// Greedy: pick the highest-scoring eligible node first, then
    /// pick the next-highest-scoring node whose spread attributes
    /// don't already overlap with picked nodes, and so on. When the
    /// pool runs out, fills remaining slots with whatever nodes are
    /// left, even if that means accepting duplicates on spread
    /// attributes.
    ///
    /// `exclude` filters out nodes that must not be picked (already
    /// holding a replica, draining, decommissioning ...).
    pub fn propose(&self, replicas: usize, exclude: &[NodeId]) -> Vec<NodeId> {
        if replicas == 0 {
            return Vec::new();
        }
        let exclude: BTreeSet<&str> = exclude.iter().map(String::as_str).collect();

        // Build eligible candidates -- nodes satisfying every required
        // attribute. If none match, fall back to the full set so the
        // caller still gets *some* placement (validate() will report
        // the violations).
        let strictly_eligible: Vec<&str> = self
            .config
            .node_attributes
            .iter()
            .filter(|(node, _)| !exclude.contains(node.as_str()))
            .filter(|(_, attrs)| {
                satisfies_required(attrs, &self.config.placement_required_attributes)
            })
            .map(|(node, _)| node.as_str())
            .collect();
        let pool: Vec<&str> = if strictly_eligible.is_empty() {
            self.config
                .node_attributes
                .keys()
                .map(String::as_str)
                .filter(|n| !exclude.contains(n))
                .collect()
        } else {
            strictly_eligible
        };

        // Score candidates by lease preference; greedy picks
        // highest-scoring node that doesn't duplicate a spread value.
        let mut scored: Vec<(&str, i32)> = pool
            .iter()
            .map(|node| {
                let attrs = self.node_attributes(node).expect("node in pool");
                (
                    *node,
                    score(attrs, &self.config.lease_preference_attributes),
                )
            })
            .collect();
        scored.sort_by(|(an, ascore), (bn, bscore)| bscore.cmp(ascore).then_with(|| an.cmp(bn)));

        let mut picked: Vec<NodeId> = Vec::with_capacity(replicas);
        let mut used_spread: HashMap<String, BTreeSet<String>> = HashMap::new();

        // First pass: prefer nodes that introduce new spread values.
        for (node, _) in &scored {
            if picked.len() == replicas {
                break;
            }
            let attrs = self.node_attributes(node).expect("node in pool");
            let mut overlaps = false;
            for spread_key in &self.config.placement_spread_attributes {
                if let Some(v) = attrs.get(spread_key) {
                    if used_spread
                        .get(spread_key)
                        .map(|seen| seen.contains(v))
                        .unwrap_or(false)
                    {
                        overlaps = true;
                        break;
                    }
                }
            }
            if overlaps {
                continue;
            }
            for spread_key in &self.config.placement_spread_attributes {
                if let Some(v) = attrs.get(spread_key) {
                    used_spread
                        .entry(spread_key.clone())
                        .or_default()
                        .insert(v.clone());
                }
            }
            picked.push((*node).to_owned());
        }

        // Second pass: fill remaining slots from leftover candidates,
        // accepting spread duplicates.
        if picked.len() < replicas {
            let already: BTreeSet<String> = picked.iter().cloned().collect();
            for (node, _) in &scored {
                if picked.len() == replicas {
                    break;
                }
                if already.contains(*node) {
                    continue;
                }
                picked.push((*node).to_owned());
            }
        }

        picked
    }

    fn distinct_values_in_cluster(&self, attribute: &str) -> usize {
        let mut values = BTreeSet::new();
        for attrs in self.config.node_attributes.values() {
            if let Some(v) = attrs.get(attribute) {
                values.insert(v.clone());
            }
        }
        values.len()
    }
}

fn satisfies_required(attrs: &NodeAttributes, required: &[PlacementAttributeConstraint]) -> bool {
    required
        .iter()
        .all(|req| attrs.get(&req.key) == Some(&req.value))
}

fn score(attrs: &NodeAttributes, prefs: &[PlacementAttributeConstraint]) -> i32 {
    // Earlier preferences weigh more than later ones. Cap weight so
    // ordering stays stable for long preference lists.
    let mut s = 0i32;
    for (rank, pref) in prefs.iter().enumerate() {
        if attrs.get(&pref.key) == Some(&pref.value) {
            let weight = (10 - rank as i32).max(1);
            s += weight;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_config(
        nodes: &[(&str, &[(&str, &str)])],
        required: &[(&str, &str)],
        spread: &[&str],
        lease: &[(&str, &str)],
    ) -> ShardingConfig {
        let mut cfg = ShardingConfig::default();
        for (node, attrs) in nodes {
            let mut map = BTreeMap::new();
            for (k, v) in *attrs {
                map.insert((*k).to_owned(), (*v).to_owned());
            }
            cfg.node_attributes.insert((*node).to_owned(), map);
        }
        cfg.placement_required_attributes = required
            .iter()
            .map(|(k, v)| PlacementAttributeConstraint {
                key: (*k).to_owned(),
                value: (*v).to_owned(),
            })
            .collect();
        cfg.placement_spread_attributes = spread.iter().map(|s| (*s).to_owned()).collect();
        cfg.lease_preference_attributes = lease
            .iter()
            .map(|(k, v)| PlacementAttributeConstraint {
                key: (*k).to_owned(),
                value: (*v).to_owned(),
            })
            .collect();
        cfg
    }

    #[test]
    fn empty_placement_is_rejected() {
        let cfg = ShardingConfig::default();
        let planner = ShardLeasePlanner::new(&cfg);
        let verdict = planner.validate(&[]);
        assert!(matches!(
            verdict.violations.as_slice(),
            [PlacementViolation::EmptyPlacement]
        ));
    }

    #[test]
    fn duplicate_node_is_flagged() {
        let cfg = mk_config(
            &[("a", &[("region", "eu")]), ("b", &[("region", "us")])],
            &[],
            &[],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "a".into()]);
        assert!(v
            .violations
            .iter()
            .any(|x| matches!(x, PlacementViolation::DuplicateNode { node } if node == "a")));
    }

    #[test]
    fn unknown_node_is_flagged() {
        let cfg = mk_config(&[("a", &[("region", "eu")])], &[], &[], &[]);
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "ghost".into()]);
        assert!(v
            .violations
            .iter()
            .any(|x| matches!(x, PlacementViolation::UnknownNode { node } if node == "ghost")));
    }

    #[test]
    fn required_attribute_mismatch_reports_actual() {
        let cfg = mk_config(
            &[("a", &[("env", "prod")]), ("b", &[("env", "staging")])],
            &[("env", "prod")],
            &[],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "b".into()]);
        assert!(v.violations.iter().any(|x| matches!(
            x,
            PlacementViolation::RequiredAttributeMismatch { node, key, expected, actual }
                if node == "b" && key == "env" && expected == "prod" && actual.as_deref() == Some("staging")
        )));
    }

    #[test]
    fn insufficient_spread_when_cluster_has_diversity() {
        let cfg = mk_config(
            &[
                ("a", &[("region", "eu")]),
                ("b", &[("region", "eu")]),
                ("c", &[("region", "us")]),
            ],
            &[],
            &["region"],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "b".into(), "c".into()]);
        assert!(v.violations.iter().any(|x| matches!(
            x,
            PlacementViolation::InsufficientSpread { attribute, duplicate_value, .. }
                if attribute == "region" && duplicate_value == "eu"
        )));
    }

    #[test]
    fn spread_not_flagged_when_cluster_lacks_diversity() {
        let cfg = mk_config(
            &[("a", &[("region", "eu")]), ("b", &[("region", "eu")])],
            &[],
            &["region"],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "b".into()]);
        assert!(v.is_ok(), "best-effort spread, no violation: {v:?}");
    }

    #[test]
    fn rank_leaseholders_orders_by_pref_score() {
        let cfg = mk_config(
            &[
                ("a", &[("region", "eu"), ("zone", "eu-1")]),
                ("b", &[("region", "us"), ("zone", "us-1")]),
                ("c", &[("region", "eu"), ("zone", "eu-2")]),
            ],
            &[],
            &[],
            &[("region", "eu"), ("zone", "eu-1")],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let ranked = p.rank_leaseholders(&["a".into(), "b".into(), "c".into()]);
        // a: region+zone match = 10+9 = 19
        // c: region only = 10
        // b: nothing = 0
        assert_eq!(ranked[0].0, "a");
        assert_eq!(ranked[0].1, 19);
        assert_eq!(ranked[1].0, "c");
        assert_eq!(ranked[1].1, 10);
        assert_eq!(ranked[2].0, "b");
        assert_eq!(ranked[2].1, 0);
    }

    #[test]
    fn propose_picks_diverse_nodes_first() {
        let cfg = mk_config(
            &[
                ("a", &[("region", "eu")]),
                ("b", &[("region", "eu")]),
                ("c", &[("region", "us")]),
                ("d", &[("region", "ap")]),
            ],
            &[],
            &["region"],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let picked = p.propose(3, &[]);
        let regions: BTreeSet<String> = picked
            .iter()
            .filter_map(|n| p.node_attributes(n).and_then(|a| a.get("region").cloned()))
            .collect();
        assert_eq!(
            regions.len(),
            3,
            "all three regions covered, got {picked:?}"
        );
    }

    #[test]
    fn propose_respects_required_attributes() {
        let cfg = mk_config(
            &[
                ("a", &[("env", "prod"), ("region", "eu")]),
                ("b", &[("env", "staging"), ("region", "us")]),
                ("c", &[("env", "prod"), ("region", "us")]),
            ],
            &[("env", "prod")],
            &["region"],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let picked = p.propose(2, &[]);
        assert_eq!(picked.len(), 2);
        for node in &picked {
            assert_eq!(p.node_attributes(node).unwrap().get("env").unwrap(), "prod");
        }
    }

    #[test]
    fn propose_excludes_nodes() {
        let cfg = mk_config(&[("a", &[]), ("b", &[]), ("c", &[])], &[], &[], &[]);
        let p = ShardLeasePlanner::new(&cfg);
        let picked = p.propose(2, &["a".into()]);
        assert_eq!(picked.len(), 2);
        assert!(!picked.contains(&"a".into()));
    }

    #[test]
    fn propose_returns_empty_when_replicas_is_zero() {
        let cfg = mk_config(&[("a", &[])], &[], &[], &[]);
        let p = ShardLeasePlanner::new(&cfg);
        assert!(p.propose(0, &[]).is_empty());
    }

    #[test]
    fn propose_falls_back_when_no_strictly_eligible_nodes() {
        // No node satisfies "env=prod" -- propose still returns
        // something (operator visibility will surface the violation
        // via validate()).
        let cfg = mk_config(
            &[("a", &[("env", "dev")]), ("b", &[("env", "staging")])],
            &[("env", "prod")],
            &[],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let picked = p.propose(2, &[]);
        assert_eq!(picked.len(), 2);
        assert!(!p.validate(&picked).is_ok());
    }

    #[test]
    fn validate_passes_for_diverse_placement() {
        let cfg = mk_config(
            &[
                ("a", &[("env", "prod"), ("region", "eu")]),
                ("b", &[("env", "prod"), ("region", "us")]),
                ("c", &[("env", "prod"), ("region", "ap")]),
            ],
            &[("env", "prod")],
            &["region"],
            &[],
        );
        let p = ShardLeasePlanner::new(&cfg);
        let v = p.validate(&["a".into(), "b".into(), "c".into()]);
        assert!(v.is_ok(), "expected clean verdict, got {v:?}");
    }
}
