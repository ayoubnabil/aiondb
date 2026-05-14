//! KV → CDC adapter.
//!
//! Bridges the [`aiondb_ha::kv_engine::KvApplyObserver`] callback into
//! a [`crate::changefeed::ChangefeedBus`]. Once installed, every KV
//! write applied by the local state machine produces a corresponding
//! [`crate::changefeed::ChangefeedEvent`] that downstream sinks
//! (webhook, browser SSE, Kafka shim) can subscribe to.
//!
//! The adapter is intentionally lossy on purpose : if subscribers
//! are slow, broadcast::send drops events instead of blocking apply.
//! Sinks needing exactly-once semantics persist a resume cursor and
//! re-subscribe.

use std::sync::Arc;

use aiondb_core::{RelationId, Row, TupleId, Value};
use aiondb_ha::kv_engine::KvApplyObserver;
use aiondb_ha::multi_raft::MultiRaftGroupId;

use crate::changefeed::{ChangefeedBus, ChangefeedEvent};

/// Maps `(group, key) -> RelationId, TupleId`. Replace the default
/// mapping (group as relation, hash of key as tuple id) by injecting a
/// custom mapper for richer SQL surface.
pub trait CdcKeyMapper: Send + Sync {
    fn map(&self, group: MultiRaftGroupId, key: &[u8]) -> (RelationId, TupleId);
}

/// Default mapper : relation = group id, tuple id = djb2 hash of key.
pub struct GroupKeyMapper;

impl CdcKeyMapper for GroupKeyMapper {
    fn map(&self, group: MultiRaftGroupId, key: &[u8]) -> (RelationId, TupleId) {
        let mut h: u64 = 5381;
        for byte in key {
            h = h.wrapping_mul(33).wrapping_add(u64::from(*byte));
        }
        (RelationId::new(group.get()), TupleId::new(h))
    }
}

/// Adapter. Cheap to clone.
pub struct KvCdcAdapter {
    bus: ChangefeedBus,
    mapper: Arc<dyn CdcKeyMapper>,
}

impl KvCdcAdapter {
    pub fn new(bus: ChangefeedBus, mapper: Arc<dyn CdcKeyMapper>) -> Self {
        Self { bus, mapper }
    }

    pub fn with_default_mapper(bus: ChangefeedBus) -> Self {
        Self::new(bus, Arc::new(GroupKeyMapper))
    }
}

impl KvApplyObserver for KvCdcAdapter {
    fn on_write(
        &self,
        group: MultiRaftGroupId,
        key: &[u8],
        value: Option<&[u8]>,
        applied_index: u64,
    ) {
        let (relation, tuple_id) = self.mapper.map(group, key);
        let event = match value {
            Some(v) => ChangefeedEvent::Insert {
                commit_ts: applied_index,
                table: relation,
                tuple_id,
                row: Row::new(vec![
                    Value::Text(String::from_utf8_lossy(key).into_owned()),
                    Value::Text(String::from_utf8_lossy(v).into_owned()),
                ]),
            },
            None => ChangefeedEvent::Delete {
                commit_ts: applied_index,
                table: relation,
                tuple_id,
            },
        };
        let _ = self.bus.emit_autocommit(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use aiondb_ha::kv_engine::KvEngine;
    use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    use aiondb_ha::protocol::NodeId;
    use tokio::time;

    use super::*;
    use crate::changefeed::{ChangefeedBus, ChangefeedConfig, ChangefeedEvent, ChangefeedFilter};

    fn fresh() -> (
        tempfile::TempDir,
        Arc<MultiRaftRegistry>,
        KvEngine,
        ChangefeedBus,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        let g = MultiRaftGroupId::new(1);
        reg.create_group(g, 1).unwrap();
        reg.become_leader(g, &[]).unwrap();
        let engine = KvEngine::new(Arc::clone(&reg));
        let bus = ChangefeedBus::new(ChangefeedConfig::default());
        (tmp, reg, engine, bus)
    }

    #[tokio::test]
    async fn put_produces_insert_event() {
        let (_t, _r, engine, bus) = fresh();
        let mut subscriber = bus.subscribe(ChangefeedFilter::all_tables());
        engine.set_observer(Arc::new(KvCdcAdapter::with_default_mapper(bus.clone())));

        engine
            .put(MultiRaftGroupId::new(1), b"k1".to_vec(), b"v1".to_vec())
            .unwrap();

        let received = time::timeout(Duration::from_millis(100), subscriber.recv())
            .await
            .unwrap()
            .unwrap();
        match received {
            ChangefeedEvent::Insert { table, row, .. } => {
                assert_eq!(table.get(), 1);
                assert_eq!(row.values.len(), 2);
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_produces_delete_event() {
        let (_t, _r, engine, bus) = fresh();
        let mut subscriber = bus.subscribe(ChangefeedFilter::all_tables());
        engine.set_observer(Arc::new(KvCdcAdapter::with_default_mapper(bus.clone())));

        engine
            .put(MultiRaftGroupId::new(1), b"k1".to_vec(), b"v1".to_vec())
            .unwrap();
        // Drain the insert event first.
        let _ = subscriber.recv().await.unwrap();

        engine
            .delete(MultiRaftGroupId::new(1), b"k1".to_vec())
            .unwrap();
        let event = time::timeout(Duration::from_millis(100), subscriber.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, ChangefeedEvent::Delete { .. }));
    }

    #[tokio::test]
    async fn multiple_writes_produce_ordered_events() {
        let (_t, _r, engine, bus) = fresh();
        let mut subscriber = bus.subscribe(ChangefeedFilter::all_tables());
        engine.set_observer(Arc::new(KvCdcAdapter::with_default_mapper(bus.clone())));

        for i in 0..10u8 {
            engine
                .put(MultiRaftGroupId::new(1), vec![i], vec![i])
                .unwrap();
        }
        let mut received = 0usize;
        while received < 10 {
            match time::timeout(Duration::from_millis(200), subscriber.recv()).await {
                Ok(Ok(ChangefeedEvent::Insert { .. })) => received += 1,
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        assert_eq!(received, 10);
    }
}
