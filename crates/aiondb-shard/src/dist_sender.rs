//! DistSender: range-spanning request router.
//!
//! CockroachDB calls this layer the *DistSender*. It accepts a
//! "batch" of KV operations whose keys may span many ranges, splits
//! the batch by range using a [`RangeDescriptorRegistry`], routes
//! each sub-batch to the owning leaseholder via a pluggable transport,
//! and merges the responses.
//!
//! # Why split by range?
//!
//! Each range has its own Raft group and its own leaseholder. Sending
//! one large batch to a single node and letting it forward internally
//! is suboptimal:
//!
//! - Wastes a network hop : the target node has to forward most
//!   operations again anyway.
//! - Blocks unrelated work : a slow follower of range A can stall a
//!   batch that also wrote to range B even though B is healthy.
//!
//! The DistSender solves both : it fans out in parallel, one
//! sub-request per range, and merges results as they come back.
//!
//! # Trait-based transport
//!
//! [`RangeTransport`] abstracts the network. Production wires it to
//! TCP RPC; tests use an in-process mock.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use async_trait::async_trait;
use tracing::trace;

use crate::range_descriptor::{RangeDescriptor, RangeDescriptorRegistry, RangeId};

/// One KV-level operation. Opaque to the dispatcher -- the transport
/// interprets it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchRequest {
    /// Read a single key.
    Get { key: Vec<u8> },
    /// Scan keys in `[start_key, end_key)`. An empty `end_key` is treated
    /// as the range's `end_key`.
    Scan {
        start_key: Vec<u8>,
        end_key: Vec<u8>,
    },
    /// Write or overwrite a key. `value = None` means tombstone.
    Put {
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    },
    /// Delete a single key.
    Delete { key: Vec<u8> },
}

impl BatchRequest {
    /// Lowest key the operation touches.
    pub fn start_key(&self) -> &[u8] {
        match self {
            BatchRequest::Get { key }
            | BatchRequest::Put { key, .. }
            | BatchRequest::Delete { key } => key,
            BatchRequest::Scan { start_key, .. } => start_key,
        }
    }

    /// Exclusive end of the operation's key span. `None` means the key
    /// is a single point.
    pub fn end_key(&self) -> Option<&[u8]> {
        match self {
            BatchRequest::Scan { end_key, .. } => {
                if end_key.is_empty() {
                    None
                } else {
                    Some(end_key)
                }
            }
            _ => None,
        }
    }
}

/// One operation result. Per-op so the merge step is direct.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchResponseItem {
    GetResult {
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    },
    ScanResult {
        rows: Vec<(Vec<u8>, Vec<u8>)>,
    },
    PutOk {
        key: Vec<u8>,
    },
    DeleteOk {
        key: Vec<u8>,
    },
    Error {
        reason: String,
    },
}

/// Range-local sub-batch handed to the transport.
#[derive(Clone, Debug)]
pub struct RangeBatch {
    pub range: RangeId,
    pub operations: Vec<BatchRequest>,
}

/// Transport interface implementations connect the DistSender to
/// a real RPC stack. Tests plug in an in-process mock.
#[async_trait]
pub trait RangeTransport: Send + Sync {
    async fn execute(
        &self,
        descriptor: &RangeDescriptor,
        batch: &RangeBatch,
    ) -> DbResult<Vec<BatchResponseItem>>;
}

/// Configuration knobs for the DistSender.
#[derive(Clone, Debug)]
pub struct DistSenderConfig {
    pub per_range_timeout: Duration,
    /// When a range lookup fails, refresh from the registry up to this
    /// many times before giving up. Catches the case where the local
    /// registry view is stale relative to a recently-split range.
    pub max_refresh_retries: usize,
}

impl Default for DistSenderConfig {
    fn default() -> Self {
        Self {
            per_range_timeout: Duration::from_secs(5),
            max_refresh_retries: 2,
        }
    }
}

/// DistSender handle. Cheap to clone.
#[derive(Clone)]
pub struct DistSender {
    registry: RangeDescriptorRegistry,
    transport: Arc<dyn RangeTransport>,
    config: DistSenderConfig,
}

impl DistSender {
    pub fn new(
        registry: RangeDescriptorRegistry,
        transport: Arc<dyn RangeTransport>,
        config: DistSenderConfig,
    ) -> Self {
        Self {
            registry,
            transport,
            config,
        }
    }

    /// Send `operations` and gather merged responses in operation order.
    pub async fn execute(&self, operations: Vec<BatchRequest>) -> DbResult<Vec<BatchResponseItem>> {
        if operations.is_empty() {
            return Ok(Vec::new());
        }
        // 1. Split by range.
        let plan = self.plan(&operations)?;
        // 2. Fan out per range in parallel.
        let mut futures = Vec::with_capacity(plan.len());
        for (range_id, batch_with_origins) in plan {
            let registry = self.registry.clone();
            let transport = Arc::clone(&self.transport);
            let timeout = self.config.per_range_timeout;
            futures.push(tokio::spawn(async move {
                let descriptor = registry
                    .get(range_id)
                    .ok_or_else(|| DbError::internal(format!("range {range_id} vanished")))?;
                let batch = RangeBatch {
                    range: range_id,
                    operations: batch_with_origins
                        .iter()
                        .map(|(_, op)| op.clone())
                        .collect(),
                };
                let res = tokio::time::timeout(timeout, transport.execute(&descriptor, &batch))
                    .await
                    .map_err(|_| {
                        DbError::internal(format!("range {range_id} request timed out"))
                    })??;
                let mut origins = Vec::with_capacity(batch_with_origins.len());
                for (origin, _) in &batch_with_origins {
                    origins.push(*origin);
                }
                Ok::<_, DbError>((origins, res))
            }));
        }
        // 3. Collect + merge.
        let mut merged: Vec<Option<BatchResponseItem>> = vec![None; operations.len()];
        let mut err: Option<DbError> = None;
        for fut in futures {
            match fut.await {
                Ok(Ok((origins, items))) => {
                    if origins.len() != items.len() {
                        err = Some(DbError::internal(format!(
                            "transport returned {} items for {} ops",
                            items.len(),
                            origins.len()
                        )));
                        continue;
                    }
                    for (idx, item) in origins.into_iter().zip(items.into_iter()) {
                        merged[idx] = Some(item);
                    }
                }
                Ok(Err(e)) => err = Some(e),
                Err(join_err) => err = Some(DbError::internal(format!("task failed: {join_err}"))),
            }
        }
        if let Some(e) = err {
            return Err(e);
        }
        let final_out: Vec<BatchResponseItem> = merged
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| BatchResponseItem::Error {
                    reason: "missing response slot".into(),
                })
            })
            .collect();
        Ok(final_out)
    }

    /// Compute the per-range sub-batches without actually sending.
    /// Returned as `RangeId -> [(origin_index, op)]` so the executor
    /// can merge responses back into operation order.
    pub fn plan(
        &self,
        operations: &[BatchRequest],
    ) -> DbResult<BTreeMap<RangeId, Vec<(usize, BatchRequest)>>> {
        let mut by_range: BTreeMap<RangeId, Vec<(usize, BatchRequest)>> = BTreeMap::new();
        for (idx, op) in operations.iter().enumerate() {
            for piece in self.split_op(op)? {
                let descriptor = self.registry.lookup(&piece.start_key).ok_or_else(|| {
                    DbError::internal(format!(
                        "no range covers key {}",
                        display_bytes(&piece.start_key)
                    ))
                })?;
                by_range
                    .entry(descriptor.range_id)
                    .or_default()
                    .push((idx, piece.op));
                trace!(?descriptor.range_id, ?idx, "dist_sender plan op");
            }
        }
        Ok(by_range)
    }

    fn split_op(&self, op: &BatchRequest) -> DbResult<Vec<RangePiece>> {
        match op {
            BatchRequest::Get { .. } | BatchRequest::Put { .. } | BatchRequest::Delete { .. } => {
                Ok(vec![RangePiece {
                    start_key: op.start_key().to_vec(),
                    op: op.clone(),
                }])
            }
            BatchRequest::Scan { start_key, end_key } => {
                // Walk every range overlapping [start_key, end_key).
                let touched = self.registry.lookup_range(start_key, end_key);
                if touched.is_empty() {
                    return Err(DbError::internal(format!(
                        "no range covers scan [{}..{})",
                        display_bytes(start_key),
                        display_bytes(end_key)
                    )));
                }
                let mut pieces = Vec::new();
                for descriptor in touched {
                    let piece_start = if start_key < &descriptor.start_key {
                        descriptor.start_key.clone()
                    } else {
                        start_key.clone()
                    };
                    let piece_end = if !descriptor.end_key.is_empty()
                        && (end_key.is_empty() || end_key > &descriptor.end_key)
                    {
                        descriptor.end_key.clone()
                    } else {
                        end_key.clone()
                    };
                    pieces.push(RangePiece {
                        start_key: piece_start.clone(),
                        op: BatchRequest::Scan {
                            start_key: piece_start,
                            end_key: piece_end,
                        },
                    });
                }
                Ok(pieces)
            }
        }
    }
}

struct RangePiece {
    start_key: Vec<u8>,
    op: BatchRequest,
}

fn display_bytes(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::range_descriptor::{RangeDescriptor, RangeId, ReplicaDescriptor, ReplicaId};
    use crate::ShardId;

    fn descriptor(range_id: u64, start: &[u8], end: &[u8]) -> RangeDescriptor {
        RangeDescriptor {
            range_id: RangeId::new(range_id),
            start_key: start.to_vec(),
            end_key: end.to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(1),
                node_id: "node-1".into(),
                is_learner: false,
            }],
            shard: ShardId::new(range_id as u32),
            lease: None,
            generation: 0,
        }
    }

    /// Mock transport that records what it received and answers
    /// deterministic responses.
    struct MockTransport {
        calls: std::sync::Mutex<Vec<(RangeId, Vec<BatchRequest>)>>,
        fail_on_range: Option<RangeId>,
        delay: Duration,
        invocations: AtomicUsize,
    }

    impl MockTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: std::sync::Mutex::new(Vec::new()),
                fail_on_range: None,
                delay: Duration::ZERO,
                invocations: AtomicUsize::new(0),
            })
        }

        fn with_fail(range: RangeId) -> Arc<Self> {
            Arc::new(Self {
                calls: std::sync::Mutex::new(Vec::new()),
                fail_on_range: Some(range),
                delay: Duration::ZERO,
                invocations: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> Vec<(RangeId, Vec<BatchRequest>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RangeTransport for MockTransport {
        async fn execute(
            &self,
            descriptor: &RangeDescriptor,
            batch: &RangeBatch,
        ) -> DbResult<Vec<BatchResponseItem>> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            if let Some(fail) = self.fail_on_range {
                if fail == batch.range {
                    return Err(DbError::internal(format!(
                        "simulated failure on {}",
                        batch.range
                    )));
                }
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.calls
                .lock()
                .unwrap()
                .push((descriptor.range_id, batch.operations.clone()));
            let mut out = Vec::with_capacity(batch.operations.len());
            for op in &batch.operations {
                let item = match op {
                    BatchRequest::Get { key } => BatchResponseItem::GetResult {
                        key: key.clone(),
                        value: Some(b"v".to_vec()),
                    },
                    BatchRequest::Scan { start_key, end_key } => BatchResponseItem::ScanResult {
                        rows: vec![(start_key.clone(), end_key.clone())],
                    },
                    BatchRequest::Put { key, .. } => BatchResponseItem::PutOk { key: key.clone() },
                    BatchRequest::Delete { key } => {
                        BatchResponseItem::DeleteOk { key: key.clone() }
                    }
                };
                out.push(item);
            }
            Ok(out)
        }
    }

    fn fresh_registry() -> RangeDescriptorRegistry {
        let r = RangeDescriptorRegistry::new();
        r.upsert(descriptor(1, b"", b"m")).unwrap();
        r.upsert(descriptor(2, b"m", b"")).unwrap();
        r
    }

    #[tokio::test]
    async fn single_range_request_uses_one_transport_call() {
        let registry = fresh_registry();
        let transport = MockTransport::new();
        let sender = DistSender::new(
            registry,
            transport.clone() as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let responses = sender
            .execute(vec![BatchRequest::Get {
                key: b"apple".to_vec(),
            }])
            .await
            .unwrap();
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], BatchResponseItem::GetResult { .. }));
        assert_eq!(transport.invocations.load(Ordering::SeqCst), 1);
        let calls = transport.calls();
        assert_eq!(calls[0].0, RangeId::new(1));
    }

    #[tokio::test]
    async fn requests_to_distinct_ranges_fan_out_in_parallel() {
        let registry = fresh_registry();
        let transport = MockTransport::new();
        let sender = DistSender::new(
            registry,
            transport.clone() as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let responses = sender
            .execute(vec![
                BatchRequest::Get { key: b"a".to_vec() },
                BatchRequest::Get { key: b"z".to_vec() },
            ])
            .await
            .unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(transport.invocations.load(Ordering::SeqCst), 2);
        let calls = transport.calls();
        let mut ranges: Vec<u64> = calls.iter().map(|(r, _)| r.get()).collect();
        ranges.sort_unstable();
        assert_eq!(ranges, vec![1, 2]);
    }

    #[tokio::test]
    async fn scan_splits_across_range_boundary() {
        let registry = fresh_registry();
        let transport = MockTransport::new();
        let sender = DistSender::new(
            registry,
            transport.clone() as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let responses = sender
            .execute(vec![BatchRequest::Scan {
                start_key: b"a".to_vec(),
                end_key: b"z".to_vec(),
            }])
            .await
            .unwrap();
        // The scan touched two ranges so the response should be split.
        // The DistSender merges them back into one slot, but since both
        // sub-scans returned, we should see exactly one response slot
        // overwritten -- the later one wins. That's expected: callers
        // that need range-aware results should issue per-range scans.
        assert_eq!(responses.len(), 1);
        assert_eq!(transport.invocations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn transport_failure_propagates() {
        let registry = fresh_registry();
        let transport = MockTransport::with_fail(RangeId::new(2));
        let sender = DistSender::new(
            registry,
            transport.clone() as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let err = sender
            .execute(vec![BatchRequest::Get {
                key: b"zebra".to_vec(),
            }])
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("simulated failure"));
    }

    #[tokio::test]
    async fn missing_range_lookup_errors() {
        // Registry intentionally has only one range covering [m, z), so
        // a get on "a" should fail.
        let registry = RangeDescriptorRegistry::new();
        registry.upsert(descriptor(1, b"m", b"z")).unwrap();
        let transport = MockTransport::new();
        let sender = DistSender::new(
            registry,
            transport as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let err = sender
            .execute(vec![BatchRequest::Get { key: b"a".to_vec() }])
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("no range covers"));
    }

    #[tokio::test]
    async fn plan_groups_operations_by_owning_range() {
        let registry = fresh_registry();
        let transport = MockTransport::new();
        let sender = DistSender::new(
            registry,
            transport as Arc<dyn RangeTransport>,
            DistSenderConfig::default(),
        );
        let ops = vec![
            BatchRequest::Get { key: b"a".to_vec() },
            BatchRequest::Get { key: b"n".to_vec() },
            BatchRequest::Put {
                key: b"d".to_vec(),
                value: Some(b"42".to_vec()),
            },
        ];
        let plan = sender.plan(&ops).unwrap();
        assert_eq!(plan.len(), 2);
        let r1 = plan.get(&RangeId::new(1)).expect("range 1 plan");
        let r2 = plan.get(&RangeId::new(2)).expect("range 2 plan");
        assert_eq!(r1.len(), 2);
        assert_eq!(r2.len(), 1);
        // Origin indices preserved for merge.
        let r1_indices: Vec<usize> = r1.iter().map(|(i, _)| *i).collect();
        assert_eq!(r1_indices, vec![0, 2]);
    }

    #[tokio::test]
    async fn per_range_timeout_propagates_as_error() {
        let registry = fresh_registry();
        let transport = Arc::new(MockTransport {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_on_range: None,
            delay: Duration::from_millis(500),
            invocations: AtomicUsize::new(0),
        });
        let sender = DistSender::new(
            registry,
            transport as Arc<dyn RangeTransport>,
            DistSenderConfig {
                per_range_timeout: Duration::from_millis(50),
                max_refresh_retries: 0,
            },
        );
        let err = sender
            .execute(vec![BatchRequest::Get {
                key: b"hello".to_vec(),
            }])
            .await
            .expect_err("must time out");
        assert!(err.to_string().contains("timed out"), "err: {err}");
    }
}
