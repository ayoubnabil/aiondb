//! Full 2PC flow integration : real KV engine + intent registry +
//! 2PC coordinator on the same node, end-to-end.

use std::collections::HashMap;
use std::sync::Arc;

use aiondb_distrib::prelude::*;
use async_trait::async_trait;
use tokio::sync::Mutex;

#[derive(Default)]
struct InProcParticipant {
    engine: Option<KvEngine>,
    group: MultiRaftGroupId,
    #[allow(dead_code)]
    intents: aiondb_distrib::tx::intent_registry::IntentRegistry,
    pending: Mutex<
        HashMap<aiondb_distrib::tx::distributed_record::DistributedTxnId, Vec<(Vec<u8>, Vec<u8>)>>,
    >,
}

impl InProcParticipant {
    fn new(engine: KvEngine, group: MultiRaftGroupId) -> Self {
        Self {
            engine: Some(engine),
            group,
            intents: aiondb_distrib::tx::intent_registry::IntentRegistry::new(),
            pending: Mutex::new(HashMap::new()),
        }
    }

    async fn stage(
        &self,
        txn: aiondb_distrib::tx::distributed_record::DistributedTxnId,
        key: Vec<u8>,
        value: Vec<u8>,
    ) {
        self.pending
            .lock()
            .await
            .entry(txn)
            .or_default()
            .push((key, value));
    }
}

#[async_trait]
impl TwoPhaseParticipant for InProcParticipant {
    async fn prepare(
        &self,
        txn_id: aiondb_distrib::tx::distributed_record::DistributedTxnId,
        _write_spans: &[aiondb_distrib::tx::distributed_record::KeySpan],
        _deadline: HlcTimestamp,
    ) -> aiondb_core::DbResult<PrepareVote> {
        let buf = self.pending.lock().await;
        if buf.get(&txn_id).map(|v| v.is_empty()).unwrap_or(true) {
            return Ok(PrepareVote::No {
                reason: format!("no staged writes for {txn_id}"),
            });
        }
        Ok(PrepareVote::Yes {
            prepared_ts: HlcTimestamp::new(2, 0),
        })
    }

    async fn commit(
        &self,
        txn_id: aiondb_distrib::tx::distributed_record::DistributedTxnId,
        _commit_ts: HlcTimestamp,
    ) -> aiondb_core::DbResult<()> {
        let writes = self
            .pending
            .lock()
            .await
            .remove(&txn_id)
            .unwrap_or_default();
        let engine = self.engine.as_ref().unwrap();
        for (k, v) in writes {
            engine.put(self.group, k, v)?;
        }
        Ok(())
    }

    async fn rollback(
        &self,
        txn_id: aiondb_distrib::tx::distributed_record::DistributedTxnId,
    ) -> aiondb_core::DbResult<()> {
        self.pending.lock().await.remove(&txn_id);
        Ok(())
    }
}

#[tokio::test]
async fn two_participants_commit_together() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    let reg_a = Arc::new(
        MultiRaftRegistry::new(aiondb_distrib::ha::protocol::NodeId::new(1), tmp_a.path()).unwrap(),
    );
    let reg_b = Arc::new(
        MultiRaftRegistry::new(aiondb_distrib::ha::protocol::NodeId::new(2), tmp_b.path()).unwrap(),
    );
    let g_a = MultiRaftGroupId::new(1);
    let g_b = MultiRaftGroupId::new(2);
    reg_a.create_group(g_a, 1).unwrap();
    reg_a.become_leader(g_a, &[]).unwrap();
    reg_b.create_group(g_b, 1).unwrap();
    reg_b.become_leader(g_b, &[]).unwrap();
    let engine_a = KvEngine::new(Arc::clone(&reg_a));
    let engine_b = KvEngine::new(Arc::clone(&reg_b));

    let p1 = Arc::new(InProcParticipant::new(engine_a.clone(), g_a));
    let p2 = Arc::new(InProcParticipant::new(engine_b.clone(), g_b));

    let txn = aiondb_distrib::tx::distributed_record::DistributedTxnId {
        coordinator: 7,
        start_ts: HlcTimestamp::new(1000, 0),
        seq: 0,
    };
    p1.stage(txn, b"k1".to_vec(), b"v1".to_vec()).await;
    p2.stage(txn, b"k2".to_vec(), b"v2".to_vec()).await;

    let registry = DistributedTxnRegistry::new();
    let clock = Arc::new(HybridLogicalClock::new());
    let coord = TwoPhaseCoordinator::new(registry, clock, CoordinatorConfig::default());
    coord
        .register_participant(
            ParticipantId(1),
            Arc::clone(&p1) as Arc<dyn TwoPhaseParticipant>,
        )
        .await;
    coord
        .register_participant(
            ParticipantId(2),
            Arc::clone(&p2) as Arc<dyn TwoPhaseParticipant>,
        )
        .await;

    let outcome = coord
        .execute(txn, 1, &[ParticipantId(1), ParticipantId(2)], Vec::new())
        .await
        .unwrap();
    assert!(matches!(outcome, CommitOutcome::Committed { .. }));
    assert_eq!(engine_a.get(g_a, b"k1").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(engine_b.get(g_b, b"k2").unwrap(), Some(b"v2".to_vec()));
}
