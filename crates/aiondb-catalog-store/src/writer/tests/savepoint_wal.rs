//! Regression tests for savepoint WAL durability.
//!
//! The catalog WAL writes `CatalogCreateTable` records eagerly as each
//! `CREATE TABLE` runs inside an open transaction. If a `SAVEPOINT` is later
//! rolled back, the in-memory pending txn reverts but the catalog WAL still
//! contains the records. When the enclosing `COMMIT` succeeds, recovery must
//! not resurrect the rolled-back DDL.
//!
//! This test drives `CatalogTxnParticipant` directly (begin / `create_table` /
//! `create_savepoint` / `rollback_to_savepoint` / commit) and then feeds the WAL
//! directory to `recover_catalog_state` to observe what a post-crash replay
//! would produce.

use aiondb_catalog::{CatalogTxnParticipant, CatalogWriter, QualifiedName};
use aiondb_core::TxnId;

use super::{make_table, store_with_wal};
use crate::recovery::recover_catalog_state;

#[test]
fn rollback_to_savepoint_does_not_resurrect_created_table_after_recovery() {
    let (store, wal, dir) = store_with_wal("savepoint_wal_rollback_then_commit");

    let txn = TxnId::new(7_777);
    store.begin_txn(txn).expect("begin_txn");

    // t1 belongs to the outer txn and MUST survive recovery.
    let mut t1 = make_table("savepoint_wal_t1");
    t1.name = QualifiedName::qualified("public", "savepoint_wal_t1");
    store.create_table(txn, t1).expect("create t1");

    // Establish a savepoint, then create t2 that we will roll back.
    let savepoint_id = store.create_savepoint(txn).expect("create_savepoint");

    let mut t2 = make_table("savepoint_wal_t2");
    t2.name = QualifiedName::qualified("public", "savepoint_wal_t2");
    store.create_table(txn, t2).expect("create t2");

    store
        .rollback_to_savepoint(txn, savepoint_id)
        .expect("rollback_to_savepoint");

    // Outer transaction commits. Because t2 was rolled back, the committed
    // catalog state must only contain t1.
    store.commit_txn(txn).expect("commit_txn");
    wal.flush().expect("wal flush");

    // Drop the store so the recovery path reads the WAL from scratch.
    drop(store);
    drop(wal);

    let recovered = recover_catalog_state(&dir).expect("recover_catalog_state");

    let t1_present = recovered
        .table_names
        .keys()
        .any(|(_, name)| name == "savepoint_wal_t1");
    let t2_present = recovered
        .table_names
        .keys()
        .any(|(_, name)| name == "savepoint_wal_t2");

    assert!(
        t1_present,
        "outer-transaction table t1 must be present after recovery"
    );
    assert!(
        !t2_present,
        "rolled-back table t2 must NOT be present after recovery; \
         savepoint rollback is not durable in the catalog WAL"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
