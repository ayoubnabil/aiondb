//! Executor-agnostic pattern description fed to the planner.
//! [`QueryGraph::validate`] rejects malformed input (dangling endpoints,
//! non-contiguous ids) up front, so the planner is total and panic-free.

/// Pattern node id (index into [`QueryGraph::nodes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

/// Pattern relationship id (index into [`QueryGraph::rels`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelId(pub usize);

/// Predicate shape; drives the default selectivity when no histogram exists,
/// exactly as mature planners behave for un-analyzed columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredicateOp {
    /// `=`: selectivity `1/ndistinct` when known, else a default.
    Equality,
    /// `<`, `>`, `<=`, `>=`.
    Range,
    /// `STARTS WITH`.
    Prefix,
    /// Regex, function — anything weakly selective.
    Other,
}

/// A predicate on one node property.
#[derive(Clone, Debug)]
pub struct PropertyPredicate {
    pub property: String,
    pub op: PredicateOp,
}

impl PropertyPredicate {
    /// Equality predicate on `property`.
    pub fn equality(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            op: PredicateOp::Equality,
        }
    }
    /// Predicate with an explicit shape.
    pub fn new(property: impl Into<String>, op: PredicateOp) -> Self {
        Self {
            property: property.into(),
            op,
        }
    }
}

/// Kind of index usable to seed a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexKind {
    /// Unique / primary key — seek yields ≤ 1 row.
    Unique,
    /// Non-unique equality index.
    NonUnique,
    /// Ordered index usable for a range seek.
    Range,
}

/// An index that can seed a node instead of a label scan.
#[derive(Clone, Debug)]
pub struct IndexSeed {
    pub property: String,
    pub kind: IndexKind,
}

/// A pattern node: optional label, property predicates, optional index seed.
#[derive(Clone, Debug)]
pub struct QueryNode {
    pub id: NodeId,
    /// `None` ⇒ all-nodes scan candidate.
    pub label: Option<String>,
    pub predicates: Vec<PropertyPredicate>,
    pub index: Option<IndexSeed>,
}

impl QueryNode {
    /// Labelled node with no predicates or index.
    pub fn labelled(id: usize, label: impl Into<String>) -> Self {
        Self {
            id: NodeId(id),
            label: Some(label.into()),
            predicates: Vec::new(),
            index: None,
        }
    }
    /// Unlabelled node.
    pub fn anonymous(id: usize) -> Self {
        Self {
            id: NodeId(id),
            label: None,
            predicates: Vec::new(),
            index: None,
        }
    }
    /// Attach a predicate (builder style).
    pub fn with_predicate(mut self, pred: PropertyPredicate) -> Self {
        self.predicates.push(pred);
        self
    }
    /// Attach an index seed (builder style).
    pub fn with_index(mut self, property: impl Into<String>, kind: IndexKind) -> Self {
        self.index = Some(IndexSeed {
            property: property.into(),
            kind,
        });
        self
    }
}

/// Traversal direction from `from` to `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelDirection {
    /// `(from)-[r]->(to)`.
    Outgoing,
    /// `(from)<-[r]-(to)`.
    Incoming,
    /// `(from)-[r]-(to)`, either direction.
    Both,
}

impl RelDirection {
    /// Direction seen when traversal starts from the `to` endpoint instead.
    pub fn reversed(self) -> Self {
        match self {
            Self::Outgoing => Self::Incoming,
            Self::Incoming => Self::Outgoing,
            Self::Both => Self::Both,
        }
    }
}

/// Variable-length bound, e.g. `-[*2..5]->`; an open upper bound is cost-capped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VarLength {
    pub min: u32,
    /// `None` ⇒ unbounded.
    pub max: Option<u32>,
}

/// A pattern relationship between two nodes.
#[derive(Clone, Debug)]
pub struct QueryRel {
    pub id: RelId,
    pub from: NodeId,
    pub to: NodeId,
    /// `None` ⇒ any type.
    pub rel_type: Option<String>,
    pub direction: RelDirection,
    /// Predicates on the relationship itself, e.g. `[r:T {since: 2020}]`.
    pub predicates: Vec<PropertyPredicate>,
    pub var_length: Option<VarLength>,
}

impl QueryRel {
    /// Fixed-length relationship between two nodes.
    pub fn new(
        id: usize,
        from: usize,
        to: usize,
        rel_type: Option<&str>,
        direction: RelDirection,
    ) -> Self {
        Self {
            id: RelId(id),
            from: NodeId(from),
            to: NodeId(to),
            rel_type: rel_type.map(str::to_owned),
            direction,
            predicates: Vec::new(),
            var_length: None,
        }
    }
    /// Attach a predicate on the relationship (builder style).
    pub fn with_predicate(mut self, pred: PropertyPredicate) -> Self {
        self.predicates.push(pred);
        self
    }
    /// Attach a variable-length bound (builder style).
    pub fn with_var_length(mut self, min: u32, max: Option<u32>) -> Self {
        self.var_length = Some(VarLength { min, max });
        self
    }
}

/// Why a [`QueryGraph`] failed validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// A node id does not match its position in `nodes`.
    NodeIdMismatch(usize),
    /// A relationship id does not match its position in `rels`.
    RelIdMismatch(usize),
    /// A relationship references a node id outside `nodes`.
    DanglingEndpoint(RelId),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeIdMismatch(i) => write!(f, "node id mismatch at index {i}"),
            Self::RelIdMismatch(i) => write!(f, "relationship id mismatch at index {i}"),
            Self::DanglingEndpoint(r) => {
                write!(f, "relationship r{} references a missing node", r.0)
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// A graph pattern to be planned.
#[derive(Clone, Debug, Default)]
pub struct QueryGraph {
    pub nodes: Vec<QueryNode>,
    pub rels: Vec<QueryRel>,
}

impl QueryGraph {
    pub fn new() -> Self {
        Self::default()
    }
    /// Add a node, assigning its [`NodeId`].
    pub fn add_node(&mut self, mut node: QueryNode) -> NodeId {
        let id = NodeId(self.nodes.len());
        node.id = id;
        self.nodes.push(node);
        id
    }
    /// Add a relationship, assigning its [`RelId`].
    pub fn add_rel(&mut self, mut rel: QueryRel) -> RelId {
        let id = RelId(self.rels.len());
        rel.id = id;
        self.rels.push(rel);
        id
    }
    /// Reject malformed graphs before planning so the planner stays total.
    pub fn validate(&self) -> Result<(), GraphError> {
        for (i, n) in self.nodes.iter().enumerate() {
            if n.id.0 != i {
                return Err(GraphError::NodeIdMismatch(i));
            }
        }
        let node_count = self.nodes.len();
        for (i, r) in self.rels.iter().enumerate() {
            if r.id.0 != i {
                return Err(GraphError::RelIdMismatch(i));
            }
            if r.from.0 >= node_count || r.to.0 >= node_count {
                return Err(GraphError::DanglingEndpoint(r.id));
            }
        }
        Ok(())
    }
}
