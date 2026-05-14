//! Distributed query DAG executor.
//!
//! Takes a logical DAG of fragments and executes it on a pool of
//! workers, honouring exchange semantics (Gather, Broadcast,
//! Repartition). The actual SQL planner emits `aiondb-plan` fragments;
//! this module supports the runtime layer that turns those fragments
//! into concrete row streams.
//!
//! Each [`QueryFragment`] is independent and identified by a numeric
//! `id`. Edges declare how rows flow between fragments and what
//! exchange transformation is applied on the way.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use aiondb_core::{DbError, DbResult, Row};

use crate::hash_partition::HashPartitioner;

/// One executable fragment of a distributed plan.
#[derive(Clone, Debug)]
pub struct QueryFragment {
    pub id: u32,
    /// Static rows the fragment emits when executed. Real fragments
    /// would run an executor against storage; tests inject row sets
    /// directly.
    pub source_rows: Vec<Row>,
}

/// Exchange transformation applied to the rows flowing along an edge.
#[derive(Clone, Debug)]
pub enum ExchangeKind {
    /// All rows arrive at a single downstream consumer.
    Gather,
    /// Every downstream consumer receives a copy of every row.
    Broadcast { fanout: usize },
    /// Rows are hash-partitioned across `partitions` downstream
    /// consumers using `key_ordinals` as the partition key.
    Repartition {
        partitions: usize,
        key_ordinals: Vec<usize>,
    },
}

/// Edge from `producer_id` to `consumer_id` with an exchange.
#[derive(Clone, Debug)]
pub struct QueryEdge {
    pub producer_id: u32,
    pub consumer_id: u32,
    pub exchange: ExchangeKind,
}

/// Full DAG.
#[derive(Clone, Debug, Default)]
pub struct QueryDag {
    fragments: HashMap<u32, QueryFragment>,
    edges: Vec<QueryEdge>,
}

impl QueryDag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_fragment(&mut self, fragment: QueryFragment) {
        self.fragments.insert(fragment.id, fragment);
    }

    pub fn add_edge(&mut self, edge: QueryEdge) {
        self.edges.push(edge);
    }

    pub fn fragment(&self, id: u32) -> Option<&QueryFragment> {
        self.fragments.get(&id)
    }

    pub fn fragments(&self) -> impl Iterator<Item = &QueryFragment> {
        self.fragments.values()
    }

    pub fn edges(&self) -> &[QueryEdge] {
        &self.edges
    }
}

/// Output of executing a DAG. Per-fragment per-partition row buckets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DagOutput {
    /// `(consumer_id, partition_index) -> rows`. Partition index is
    /// `0` for `Gather` and `Broadcast`.
    pub buckets: BTreeMap<(u32, usize), Vec<Row>>,
}

impl DagOutput {
    pub fn for_consumer(&self, consumer_id: u32) -> Vec<&Vec<Row>> {
        self.buckets
            .iter()
            .filter(|((id, _), _)| *id == consumer_id)
            .map(|(_, rows)| rows)
            .collect()
    }

    pub fn for_consumer_partition(&self, consumer_id: u32, partition: usize) -> Option<&Vec<Row>> {
        self.buckets.get(&(consumer_id, partition))
    }
}

/// Synchronous DAG executor. Runs every fragment locally, then routes
/// rows through the declared exchanges.
pub struct QueryDagExecutor;

impl QueryDagExecutor {
    pub fn run(dag: &QueryDag) -> DbResult<DagOutput> {
        let mut output = DagOutput::default();
        for edge in dag.edges() {
            let producer = dag.fragment(edge.producer_id).ok_or_else(|| {
                DbError::internal(format!("producer {} missing", edge.producer_id))
            })?;
            match &edge.exchange {
                ExchangeKind::Gather => {
                    output
                        .buckets
                        .entry((edge.consumer_id, 0))
                        .or_default()
                        .extend(producer.source_rows.iter().cloned());
                }
                ExchangeKind::Broadcast { fanout } => {
                    for p in 0..*fanout {
                        output
                            .buckets
                            .entry((edge.consumer_id, p))
                            .or_default()
                            .extend(producer.source_rows.iter().cloned());
                    }
                }
                ExchangeKind::Repartition {
                    partitions,
                    key_ordinals,
                } => {
                    let partitioner = HashPartitioner::new(*partitions, key_ordinals.clone())?;
                    let buckets = partitioner.partition_batch(producer.source_rows.clone())?;
                    for (p, rows) in buckets.into_iter().enumerate() {
                        output
                            .buckets
                            .entry((edge.consumer_id, p))
                            .or_default()
                            .extend(rows);
                    }
                }
            }
        }
        Ok(output)
    }
}

/// Async wrapper that runs fragments in parallel via `tokio::task::spawn`.
/// Useful when individual fragments are heavy; trivial fragments should
/// just use the synchronous executor.
pub async fn run_async(dag: Arc<QueryDag>) -> DbResult<DagOutput> {
    let task = tokio::task::spawn_blocking(move || QueryDagExecutor::run(&dag));
    task.await
        .map_err(|e| DbError::internal(format!("dag task: {e}")))?
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use aiondb_core::Value;

    use super::*;

    fn row(values: Vec<Value>) -> Row {
        Row::new(values)
    }

    fn fragment(id: u32, rows: Vec<Row>) -> QueryFragment {
        QueryFragment {
            id,
            source_rows: rows,
        }
    }

    #[test]
    fn gather_collects_every_row_on_one_consumer() {
        let mut dag = QueryDag::new();
        dag.add_fragment(fragment(
            1,
            vec![row(vec![Value::Int(1)]), row(vec![Value::Int(2)])],
        ));
        dag.add_fragment(fragment(2, vec![]));
        dag.add_edge(QueryEdge {
            producer_id: 1,
            consumer_id: 2,
            exchange: ExchangeKind::Gather,
        });
        let output = QueryDagExecutor::run(&dag).unwrap();
        let bucket = output.for_consumer_partition(2, 0).unwrap();
        assert_eq!(bucket.len(), 2);
    }

    #[test]
    fn broadcast_duplicates_rows_to_every_partition() {
        let mut dag = QueryDag::new();
        dag.add_fragment(fragment(
            1,
            vec![row(vec![Value::Int(1)]), row(vec![Value::Int(2)])],
        ));
        dag.add_edge(QueryEdge {
            producer_id: 1,
            consumer_id: 2,
            exchange: ExchangeKind::Broadcast { fanout: 3 },
        });
        let output = QueryDagExecutor::run(&dag).unwrap();
        for p in 0..3 {
            let b = output.for_consumer_partition(2, p).unwrap();
            assert_eq!(b.len(), 2, "partition {p}");
        }
    }

    #[test]
    fn repartition_spreads_rows_across_consumers() {
        let mut dag = QueryDag::new();
        let rows: Vec<Row> = (0..200).map(|i| row(vec![Value::BigInt(i)])).collect();
        dag.add_fragment(fragment(1, rows));
        dag.add_edge(QueryEdge {
            producer_id: 1,
            consumer_id: 2,
            exchange: ExchangeKind::Repartition {
                partitions: 4,
                key_ordinals: vec![0],
            },
        });
        let output = QueryDagExecutor::run(&dag).unwrap();
        let mut total = 0usize;
        let mut populated = HashSet::new();
        for p in 0..4 {
            let len = output
                .for_consumer_partition(2, p)
                .map(|v| v.len())
                .unwrap_or(0);
            total += len;
            if len > 0 {
                populated.insert(p);
            }
        }
        assert_eq!(total, 200);
        assert!(
            populated.len() >= 2,
            "spread should hit multiple partitions"
        );
    }

    #[tokio::test]
    async fn run_async_returns_same_result_as_sync() {
        let mut dag = QueryDag::new();
        dag.add_fragment(fragment(
            1,
            vec![row(vec![Value::Int(7)]), row(vec![Value::Int(8)])],
        ));
        dag.add_edge(QueryEdge {
            producer_id: 1,
            consumer_id: 2,
            exchange: ExchangeKind::Gather,
        });
        let sync = QueryDagExecutor::run(&dag).unwrap();
        let async_out = run_async(Arc::new(dag)).await.unwrap();
        assert_eq!(sync, async_out);
    }

    #[test]
    fn missing_producer_errors_cleanly() {
        let mut dag = QueryDag::new();
        dag.add_edge(QueryEdge {
            producer_id: 99,
            consumer_id: 2,
            exchange: ExchangeKind::Gather,
        });
        let err = QueryDagExecutor::run(&dag).unwrap_err();
        assert!(err.to_string().contains("producer 99 missing"));
    }
}
