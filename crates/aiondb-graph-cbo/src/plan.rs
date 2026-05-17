//! The optimizer's output: an annotated physical traversal plan.
//!
//! Every node carries estimated rows and cumulative cost, so the result is
//! directly explainable and callers can compare alternatives. The op set is
//! minimal — seed, expand (optionally "into" a bound node), cartesian product
//! — which suffices for any linear traversal order a MATCH requires.
//!
//! Child nodes are `Rc` rather than `Box`: the dynamic-programming search
//! clones partial plans constantly, and structural sharing makes each clone
//! O(1) instead of a deep tree copy. `Rc` is sufficient because planning is
//! single-threaded and the resulting plan is read-only.

use std::rc::Rc;

use crate::query_graph::{NodeId, RelDirection, RelId, VarLength};

/// A physical traversal operator.
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicalOp {
    /// Scan every node (no label available).
    AllNodesScan { node: NodeId },
    /// Scan every node carrying `label`.
    NodeByLabelScan { node: NodeId, label: String },
    /// Seek `node` directly through an index.
    NodeIndexSeek {
        node: NodeId,
        property: String,
        unique: bool,
        range: bool,
    },
    /// Expand `rel` from a bound endpoint to its other endpoint.
    Expand {
        input: Rc<ExpansionPlan>,
        rel: RelId,
        /// Already-bound endpoint the expand starts from.
        from: NodeId,
        /// Endpoint reached by the expand.
        to: NodeId,
        /// Direction as traversed (oriented for the chosen start).
        direction: RelDirection,
        /// `true` when `to` was already bound (cycle close / join filter).
        into: bool,
        var_length: Option<VarLength>,
    },
    /// Two independently-expanded sub-patterns joined on a shared node — a
    /// bushy plan. Strictly better than any left-deep expand order when both
    /// sides are individually selective (Neo4j's `NodeHashJoin`).
    HashJoin {
        /// Build side (smaller, hashed).
        left: Rc<ExpansionPlan>,
        /// Probe side.
        right: Rc<ExpansionPlan>,
        /// Node both sides bind; the equi-join key.
        on: NodeId,
    },
    /// Independent components combined by a cartesian product.
    CartesianProduct {
        left: Rc<ExpansionPlan>,
        right: Rc<ExpansionPlan>,
    },
}

/// A physical operator annotated with its cost-model estimates.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpansionPlan {
    /// Operator at this plan node.
    pub op: PhysicalOp,
    /// Estimated rows emitted.
    pub rows: f64,
    /// Estimated cumulative cost of this node and its inputs.
    pub cost: f64,
}

impl ExpansionPlan {
    pub(crate) fn leaf(op: PhysicalOp, rows: f64, cost: f64) -> Self {
        Self { op, rows, cost }
    }

    /// Render the plan as an indented tree (root first) for `EXPLAIN`/debug.
    pub fn explain(&self) -> String {
        let mut out = String::new();
        self.explain_into(&mut out, 0);
        out
    }

    fn explain_into(&self, out: &mut String, depth: usize) {
        for _ in 0..depth {
            out.push_str("  ");
        }
        let line = match &self.op {
            PhysicalOp::AllNodesScan { node } => format!("AllNodesScan(n{})", node.0),
            PhysicalOp::NodeByLabelScan { node, label } => {
                format!("NodeByLabelScan(n{}:{})", node.0, label)
            }
            PhysicalOp::NodeIndexSeek {
                node,
                property,
                unique,
                range,
            } => format!(
                "NodeIndexSeek(n{}.{}{}{})",
                node.0,
                property,
                if *unique { " unique" } else { "" },
                if *range { " range" } else { "" },
            ),
            PhysicalOp::Expand {
                rel,
                from,
                to,
                into,
                ..
            } => format!(
                "{}(n{}-[r{}]-n{})",
                if *into { "ExpandInto" } else { "Expand" },
                from.0,
                rel.0,
                to.0,
            ),
            PhysicalOp::HashJoin { on, .. } => format!("HashJoin(on n{})", on.0),
            PhysicalOp::CartesianProduct { .. } => "CartesianProduct".to_owned(),
        };
        out.push_str(&format!(
            "{} ~{:.0} rows, cost {:.1}\n",
            line, self.rows, self.cost
        ));
        match &self.op {
            PhysicalOp::Expand { input, .. } => input.explain_into(out, depth + 1),
            PhysicalOp::HashJoin { left, right, .. }
            | PhysicalOp::CartesianProduct { left, right } => {
                left.explain_into(out, depth + 1);
                right.explain_into(out, depth + 1);
            }
            _ => {}
        }
    }
}
