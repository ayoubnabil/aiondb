use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
struct CountingTransactionManager {
    next_txn_id: AtomicU64,
    begins: AtomicU64,
    commits: AtomicU64,
    rollbacks: AtomicU64,
}

impl CountingTransactionManager {
    fn counts(&self) -> (u64, u64, u64) {
        (
            self.begins.load(Ordering::SeqCst),
            self.commits.load(Ordering::SeqCst),
            self.rollbacks.load(Ordering::SeqCst),
        )
    }
}

impl aiondb_tx::TransactionLifecycle for CountingTransactionManager {
    fn begin(
        &self,
        isolation: aiondb_tx::IsolationLevel,
    ) -> aiondb_core::DbResult<aiondb_tx::ActiveTransaction> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        let txn_id = aiondb_core::TxnId::new(self.next_txn_id.fetch_add(1, Ordering::SeqCst) + 1);
        Ok(aiondb_tx::ActiveTransaction {
            id: txn_id,
            isolation,
            start_ts: txn_id.get().saturating_sub(1),
            snapshot: aiondb_tx::Snapshot::new(
                txn_id,
                aiondb_core::TxnId::new(txn_id.get() + 1),
                Vec::new(),
            ),
        })
    }

    fn commit(
        &self,
        txn: aiondb_tx::ActiveTransaction,
    ) -> aiondb_core::DbResult<aiondb_tx::CommitResult> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        Ok(aiondb_tx::CommitResult {
            txn_id: txn.id,
            commit_ts: txn.id.get(),
        })
    }

    fn rollback(&self, _txn: aiondb_tx::ActiveTransaction) -> aiondb_core::DbResult<()> {
        self.rollbacks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl aiondb_tx::SnapshotOracle for CountingTransactionManager {
    fn statement_snapshot(
        &self,
        txn: &aiondb_tx::ActiveTransaction,
    ) -> aiondb_core::DbResult<aiondb_tx::Snapshot> {
        Ok(txn.snapshot.clone())
    }
}

#[test]
fn repeated_select_reuses_cached_physical_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_hits (id INT NOT NULL); \
             INSERT INTO cache_hits VALUES (1), (2), (3)",
        )
        .expect("setup");

    assert_eq!(engine.plan_cache_hits(), 0);

    let sql = "SELECT id FROM cache_hits WHERE id = 2";
    engine.execute_sql(&session, sql).expect("first select");
    assert_eq!(engine.plan_cache_hits(), 0);
    assert!(
        engine.session_plan_cache_len(&session).expect("cache len") <= 1,
        "first occurrence may admit a literal-fast-path plan"
    );
    assert!(!engine
        .session_has_cached_sql_plan_fingerprints(&session, sql)
        .expect("parsed SQL cache fingerprints are populated after cache admission"));

    engine.execute_sql(&session, sql).expect("second select");
    assert_eq!(engine.plan_cache_hits(), 1);
    assert!(engine
        .session_has_cached_sql_plan_fingerprints(&session, sql)
        .expect("cached plan fingerprints after parsed SQL cache hit"));

    engine.execute_sql(&session, sql).expect("third select");
    assert_eq!(engine.plan_cache_hits(), 2);
}

#[test]
fn literal_range_select_reuses_cached_physical_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_range_hits (id INT NOT NULL, likes INT NOT NULL); \
             CREATE INDEX cache_range_hits_likes_idx ON cache_range_hits (likes); \
             INSERT INTO cache_range_hits VALUES (1, 10), (2, 20), (3, 30), (4, 40)",
        )
        .expect("setup");

    assert_eq!(engine.plan_cache_hits(), 0);
    engine
        .execute_sql(
            &session,
            "SELECT id, likes FROM cache_range_hits WHERE likes >= 10 AND likes < 30 ORDER BY likes LIMIT 2",
        )
        .expect("first range select");
    assert_eq!(engine.plan_cache_hits(), 0);

    engine
        .execute_sql(
            &session,
            "SELECT id, likes FROM cache_range_hits WHERE likes >= 20 AND likes < 50 ORDER BY likes LIMIT 2",
        )
        .expect("second range select");
    assert_eq!(engine.plan_cache_hits(), 1);
}

#[test]
fn autocommit_read_only_select_skips_implicit_transaction_runtime() {
    let tx_manager = Arc::new(CountingTransactionManager::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager.clone())
        .with_snapshot_oracle(tx_manager.clone())
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE readonly_fast_path (id INT NOT NULL); \
             INSERT INTO readonly_fast_path VALUES (1), (2), (3)",
        )
        .expect("setup");
    let after_setup = tx_manager.counts();
    assert_eq!(after_setup.0, after_setup.1);
    assert_eq!(after_setup.2, 0);

    engine
        .execute_sql(&session, "SELECT COUNT(*) FROM readonly_fast_path")
        .expect("read-only select");
    assert_eq!(
        tx_manager.counts(),
        after_setup,
        "pure autocommit SELECT should not allocate and commit a transaction"
    );

    engine
        .execute_sql(&session, "INSERT INTO readonly_fast_path VALUES (4)")
        .expect("write");
    let after_write = tx_manager.counts();
    assert_eq!(after_write.0, after_setup.0 + 1);
    assert_eq!(after_write.1, after_setup.1 + 1);
    assert_eq!(after_write.2, 0);
}

#[test]
fn multi_statement_dml_batch_uses_one_implicit_transaction() {
    let tx_manager = Arc::new(CountingTransactionManager::default());
    let engine = EngineBuilder::for_testing()
        .with_transaction_manager(tx_manager.clone())
        .with_snapshot_oracle(tx_manager.clone())
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE dml_batch (id INT PRIMARY KEY)")
        .expect("create table");
    let before_batch = tx_manager.counts();

    engine
        .execute_sql(
            &session,
            "DELETE FROM dml_batch WHERE id = 100; INSERT INTO dml_batch VALUES (1)",
        )
        .expect("dml batch");

    let after_batch = tx_manager.counts();
    assert_eq!(after_batch.0, before_batch.0 + 1);
    assert_eq!(after_batch.1, before_batch.1 + 1);
    assert_eq!(after_batch.2, before_batch.2);
}

#[test]
fn explicit_read_only_transaction_reuses_cached_select_plan_across_transactions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_txn_hits (id INT NOT NULL); \
             INSERT INTO cache_txn_hits VALUES (1), (2), (3)",
        )
        .expect("setup");

    let sql = "SELECT id FROM cache_txn_hits WHERE id = 2";
    for _ in 0..3 {
        engine.execute_sql(&session, "BEGIN").expect("begin");
        engine
            .execute_sql(&session, sql)
            .expect("select in explicit txn");
        engine.execute_sql(&session, "COMMIT").expect("commit");
    }

    assert_eq!(engine.plan_cache_hits(), 2);
}

#[test]
fn repeated_update_reuses_cached_physical_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_updates (id INT NOT NULL, name TEXT); \
             INSERT INTO cache_updates VALUES (1, 'alpha')",
        )
        .expect("setup");

    assert_eq!(engine.plan_cache_hits(), 0);

    let sql = "UPDATE cache_updates SET name = 'beta' WHERE id = 1";
    engine.execute_sql(&session, sql).expect("first update");
    assert_eq!(engine.plan_cache_hits(), 0);

    engine.execute_sql(&session, sql).expect("second update");
    assert_eq!(engine.plan_cache_hits(), 0);

    engine.execute_sql(&session, sql).expect("third update");
    assert_eq!(engine.plan_cache_hits(), 1);
}

#[test]
fn create_index_invalidates_cached_select_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_invalidate (id INT NOT NULL, name TEXT); \
             INSERT INTO cache_invalidate VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("setup");

    let sql = "SELECT id FROM cache_invalidate WHERE id = 2";
    engine.execute_sql(&session, sql).expect("seed cache");
    engine.execute_sql(&session, sql).expect("cache hit");
    assert_eq!(engine.plan_cache_hits(), 1);

    engine
        .execute_sql(
            &session,
            "CREATE INDEX cache_invalidate_idx ON cache_invalidate (id)",
        )
        .expect("create index");

    let hits_before_replan = engine.plan_cache_hits();
    engine
        .execute_sql(&session, sql)
        .expect("first select after invalidation");
    assert_eq!(engine.plan_cache_hits(), hits_before_replan);

    engine
        .execute_sql(&session, sql)
        .expect("second select after invalidation");
    assert_eq!(engine.plan_cache_hits(), hits_before_replan + 1);

    match access_path_for_query(&engine, &session, sql) {
        aiondb_plan::ScanAccessPath::IndexEq { .. }
        | aiondb_plan::ScanAccessPath::IndexRange { .. } => {}
        other => panic!("expected indexed access path, got {other:?}"),
    }
}

#[test]
fn external_catalog_change_invalidates_cached_plan() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine_a = build_engine_with_store(catalog.clone(), storage.clone());
    let engine_b = build_engine_with_store(catalog, storage);
    let (session_a, _) = engine_a.startup(startup_params()).expect("startup A");
    let (session_b, _) = engine_b.startup(startup_params()).expect("startup B");

    engine_a
        .execute_sql(
            &session_a,
            "CREATE TABLE cache_external (id INT NOT NULL, name TEXT); \
             INSERT INTO cache_external VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("setup");

    let sql = "SELECT id FROM cache_external WHERE id = 2";
    engine_a.execute_sql(&session_a, sql).expect("seed cache");
    engine_a.execute_sql(&session_a, sql).expect("cache hit");
    assert_eq!(engine_a.plan_cache_hits(), 1);

    engine_b
        .execute_sql(
            &session_b,
            "CREATE INDEX cache_external_idx ON cache_external (id)",
        )
        .expect("create index from engine B");

    let hits_before_replan = engine_a.plan_cache_hits();
    engine_a
        .execute_sql(&session_a, sql)
        .expect("first select after external catalog change");
    assert_eq!(engine_a.plan_cache_hits(), hits_before_replan);

    engine_a
        .execute_sql(&session_a, sql)
        .expect("second select after external catalog change");
    assert_eq!(engine_a.plan_cache_hits(), hits_before_replan + 1);

    match access_path_for_query(&engine_a, &session_a, sql) {
        aiondb_plan::ScanAccessPath::IndexEq { .. }
        | aiondb_plan::ScanAccessPath::IndexRange { .. } => {}
        other => panic!("expected indexed access path, got {other:?}"),
    }
}

#[test]
fn parsed_sql_cache_uses_lru_eviction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for i in 0..63 {
        engine
            .execute_sql(&session, &format!("SELECT {i}"))
            .expect("seed parsed SQL cache");
    }

    let hot_sql = "SELECT 999";
    engine
        .execute_sql(&session, hot_sql)
        .expect("insert hot parsed SQL entry");
    assert_eq!(
        engine
            .session_parsed_sql_cache_len(&session)
            .expect("parsed SQL cache len"),
        64
    );

    engine
        .execute_sql(&session, hot_sql)
        .expect("refresh hot parsed SQL entry");
    engine
        .execute_sql(&session, "SELECT 1000")
        .expect("force parsed SQL cache eviction");

    assert_eq!(
        engine
            .session_parsed_sql_cache_len(&session)
            .expect("parsed SQL cache len after eviction"),
        64
    );
    assert!(engine
        .session_has_cached_sql(&session, hot_sql)
        .expect("hot SQL should remain cached"));
}

#[test]
fn parsed_sql_cache_enforces_total_sql_byte_budget() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let comment_blob = "x".repeat(200_000);
    for i in 0..50 {
        let sql = format!("SELECT {i} /*{comment_blob}*/");
        engine
            .execute_sql(&session, &sql)
            .expect("seed parsed SQL cache with large entries");
    }

    let cache_len = engine
        .session_parsed_sql_cache_len(&session)
        .expect("parsed SQL cache len");
    let cache_bytes = engine
        .session_parsed_sql_cache_sql_bytes(&session)
        .expect("parsed SQL cache bytes");

    assert!(
        cache_len < 50,
        "byte-budget eviction should trim cache entries"
    );
    assert!(
        cache_bytes <= 8 * 1024 * 1024,
        "cache bytes exceeded budget: {cache_bytes}"
    );
}

#[test]
fn plan_cache_uses_lru_eviction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE cache_lru (id INT NOT NULL); \
             INSERT INTO cache_lru VALUES (1), (2), (3)",
        )
        .expect("setup");

    for i in 0..127 {
        let sql = format!("SELECT id FROM cache_lru WHERE id = {i}");
        engine
            .execute_sql(&session, &sql)
            .expect("seed parsed SQL cache");
        engine.execute_sql(&session, &sql).expect("seed plan cache");
    }

    let hot_sql = "SELECT id FROM cache_lru WHERE id = 999";
    engine
        .execute_sql(&session, hot_sql)
        .expect("first hot plan occurrence");
    engine
        .execute_sql(&session, hot_sql)
        .expect("second hot plan occurrence should seed plan cache");
    let hits_before_refresh = engine.plan_cache_hits();

    engine
        .execute_sql(&session, hot_sql)
        .expect("third hot plan occurrence should hit cache");
    assert_eq!(engine.plan_cache_hits(), hits_before_refresh + 1);

    let eviction_sql = "SELECT id FROM cache_lru WHERE id = 1000";
    engine
        .execute_sql(&session, eviction_sql)
        .expect("first eviction candidate occurrence");
    engine
        .execute_sql(&session, eviction_sql)
        .expect("second eviction candidate occurrence should seed parsed SQL cache");
    engine
        .execute_sql(&session, eviction_sql)
        .expect("third eviction candidate occurrence should admit plan");

    let hits_before_final = engine.plan_cache_hits();
    engine
        .execute_sql(&session, hot_sql)
        .expect("hot plan should survive LRU eviction");
    assert_eq!(engine.plan_cache_hits(), hits_before_final + 1);
    let cache_len = engine
        .session_plan_cache_len(&session)
        .expect("plan cache len after eviction");
    assert!(
        cache_len <= 128,
        "plan cache must respect capacity, got {cache_len}"
    );
}
