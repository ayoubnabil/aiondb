//! Catalog recovery from snapshot + WAL.
//!
//! Recovery proceeds in two phases:
//! 1. Load the catalog snapshot (if one exists)
//! 2. Replay catalog WAL records that are newer than the snapshot

use std::collections::BTreeMap;
use std::path::Path;

use aiondb_core::{DbError, DbResult, TxnId};
use aiondb_wal::{Lsn, WalReader, WalRecord};
use tracing::info;

use crate::catalog_wal::replay_catalog_record;
use crate::snapshot::load_catalog_snapshot;
use crate::{bootstrap, CatalogState};

/// Tracked state for a single transaction during catalog WAL replay.
#[derive(Default)]
struct ReplayCatalogTransaction {
    /// Catalog records collected for this transaction.
    records: Vec<WalRecord>,
}

fn recovery_target_lsn_from_env() -> DbResult<Option<Lsn>> {
    let raw = match std::env::var("AIONDB_RECOVERY_TARGET_LSN") {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(DbError::internal(
                "AIONDB_RECOVERY_TARGET_LSN contains non-Unicode bytes",
            ));
        }
    };

    Lsn::from_str_value(&raw)
        .ok_or_else(|| {
            DbError::internal(format!(
                "invalid AIONDB_RECOVERY_TARGET_LSN value '{raw}' (expected decimal or PostgreSQL-style hex like 0/1A3F)"
            ))
        })
        .map(Some)
}

/// Recover a `CatalogState` from a snapshot file + WAL entries.
///
/// 1. If a catalog snapshot exists in `wal_dir`, load it and use its
///    `checkpoint_lsn` as the starting point for WAL replay.
/// 2. Stream WAL entries from `replay_start_lsn` onward in LSN order.
/// 3. Buffer catalog records per open transaction and replay them at commit.
/// 4. If no snapshot exists and no WAL entries with catalog records are
///    found, return a bootstrapped default state.
pub fn recover_catalog_state(wal_dir: &Path) -> DbResult<CatalogState> {
    // Phase 0: Try to load catalog snapshot.
    let (mut state, replay_start_lsn) = match load_catalog_snapshot(wal_dir)? {
        Some((header, snapshot_state)) => {
            // Replay from the snapshot boundary. Control records that do not
            // have open transaction context are ignored by classification.
            (snapshot_state, header.checkpoint_lsn)
        }
        None => {
            let mut state = CatalogState::default();
            bootstrap::bootstrap_state(&mut state);
            (state, Lsn::new(1))
        }
    };

    // Phase 1: Read WAL entries from replay_start_lsn onward.
    // The WAL dir may not exist yet on a fresh database.
    if !wal_dir.exists() {
        return Ok(state);
    }

    let target_lsn = recovery_target_lsn_from_env()?;
    let mut reader = WalReader::open(wal_dir.to_path_buf(), replay_start_lsn)?;
    let mut open_txns: BTreeMap<TxnId, ReplayCatalogTransaction> = BTreeMap::new();
    let mut first_seen_lsn: Option<Lsn> = None;
    let mut replayed_count: u64 = 0;

    // Phase 2: Stream WAL replay in entry order to avoid retaining the full WAL
    // history in memory during recovery.
    while let Some(entry) = reader.next_entry()? {
        if first_seen_lsn.is_none() {
            if entry.lsn > replay_start_lsn {
                return Err(aiondb_core::DbError::internal(format!(
                    "catalog WAL replay gap detected after snapshot: expected first replay LSN {}, found {}",
                    replay_start_lsn.get(),
                    entry.lsn.get()
                )));
            }
            first_seen_lsn = Some(entry.lsn);
        }

        if target_lsn.is_some_and(|target| entry.lsn > target) {
            break;
        }

        match entry.record {
            WalRecord::BeginTxn { txn_id, .. } => {
                open_txns.insert(txn_id, ReplayCatalogTransaction::default());
            }
            WalRecord::CommitTxn { txn_id, .. } => {
                if let Some(replay) = open_txns.remove(&txn_id) {
                    for record in replay.records {
                        replay_catalog_record(&mut state, &record)?;
                    }
                    replayed_count += 1;
                }
            }
            WalRecord::AbortTxn { txn_id } => {
                open_txns.remove(&txn_id);
            }
            WalRecord::Checkpoint { .. } => {}
            record if record.is_catalog_record() => {
                if let Some(txn_id) = record.txn_id() {
                    if txn_id == TxnId::default() {
                        replay_catalog_record(&mut state, &record)?;
                        replayed_count += 1;
                    } else {
                        open_txns.entry(txn_id).or_default().records.push(record);
                    }
                }
            }
            _ => {} // Skip non-catalog storage records
        }
    }

    if replayed_count > 0 {
        info!(
            replayed_count,
            "catalog recovery: replayed committed transactions from WAL"
        );
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog_wal, snapshot::save_catalog_snapshot, CatalogStore, CatalogWalHandle};
    use aiondb_catalog::{
        CatalogTxnParticipant, CatalogWriter, ColumnDescriptor, EdgeLabelDescriptor,
        IndexDescriptor, IndexKeyColumn, IndexKind, NodeLabelDescriptor, QualifiedName,
        RoleDescriptor, SchemaDescriptor, SequenceDescriptor, SortOrder, TableDescriptor,
        TableStatistics, TenantDescriptor, TriggerDescriptor, TriggerEventDescriptor,
        TriggerTimingDescriptor, ViewDescriptor,
    };
    use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SchemaId, SequenceId, TxnId};
    use aiondb_wal::{Lsn, WalConfig, WalLsnMode, WalWriter};
    use std::{path::PathBuf, sync::Arc};

    fn test_dir(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path("recovery-test", name)
    }
    fn test_config(dir: PathBuf) -> WalConfig {
        WalConfig {
            dir,
            segment_max_bytes: 16 * 1024 * 1024,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: WalLsnMode::Logical,
        }
    }

    #[test]
    fn recovery_from_empty_wal() {
        let dir = test_dir("empty_wal");
        aiondb_wal::segment::ensure_wal_dir(&dir).unwrap();
        let state = recover_catalog_state(&dir).unwrap();
        // Should have bootstrapped schemas
        assert!(!state.schemas_by_id.is_empty());
        assert!(state.schema_names.contains_key("public"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_from_wal_with_schema() {
        let dir = test_dir("wal_schema");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let txn = TxnId::new(1);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let desc = SchemaDescriptor {
            schema_id: SchemaId::new(100),
            name: "recovered_schema".to_owned(),
        };
        let record = catalog_wal::create_schema_record(txn, &desc).unwrap();
        writer.append(&record).unwrap();

        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.schema_names.contains_key("recovered_schema"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_propagates_wal_directory_read_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("wal_dir_permission_denied");
        std::fs::create_dir_all(&dir).unwrap();
        let original_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o111)).unwrap();

        let err = recover_catalog_state(&dir)
            .expect_err("catalog recovery must fail when WAL segments cannot be listed");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(original_mode)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.to_string().contains("listing segments"));
    }

    #[test]
    fn recovery_from_wal_with_role() {
        let dir = test_dir("wal_role");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        let txn = TxnId::new(1);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let role = RoleDescriptor {
            name: "testuser".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };
        let record = catalog_wal::create_role_record(txn, &role).unwrap();
        writer.append(&record).unwrap();

        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.roles.contains_key("testuser"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_errors_on_gap_after_snapshot() {
        let dir = test_dir("snapshot_gap");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 1,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: WalLsnMode::Logical,
        };
        let mut writer = WalWriter::open(config).unwrap();
        writer
            .append(&WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(1),
            })
            .unwrap();
        writer.flush().unwrap();

        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let txn = TxnId::new(91);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        writer.remove_segments_before(Lsn::new(3)).unwrap();
        drop(writer);

        let error = recover_catalog_state(&dir)
            .expect_err("catalog recovery must reject WAL gaps after a snapshot");
        assert!(error.to_string().contains("replay gap"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_errors_on_mid_history_gap_after_snapshot() {
        let dir = test_dir("snapshot_mid_gap");
        let config = WalConfig {
            dir: dir.clone(),
            segment_max_bytes: 1,
            sync_on_flush: false,
            group_commit_delay_micros: 0,
            wal_compression: aiondb_wal::WalCompression::None,
            wal_lsn_mode: WalLsnMode::Logical,
        };
        let mut writer = WalWriter::open(config).unwrap();
        writer
            .append(&WalRecord::Checkpoint {
                last_committed_lsn: Lsn::new(1),
            })
            .unwrap();
        writer.flush().unwrap();

        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        save_catalog_snapshot(&state, Lsn::new(1), &dir).unwrap();

        let first_txn = TxnId::new(92);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: first_txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: first_txn,
                commit_ts: 1,
            })
            .unwrap();
        let second_txn = TxnId::new(93);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: second_txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: second_txn,
                commit_ts: 2,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let segments = aiondb_wal::segment::list_segments(&dir).unwrap();
        assert!(
            segments.len() >= 5,
            "expected one checkpoint segment plus four WAL segments"
        );
        std::fs::remove_file(dir.join(segments[2].filename())).unwrap();

        let error = recover_catalog_state(&dir)
            .expect_err("catalog recovery must reject mid-history WAL gaps after a snapshot");
        let message = error.to_string();
        assert!(
            message.contains("WAL gap detected") || message.contains("backward-chain mismatch"),
            "unexpected error: {message}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_autocommit_catalog_records() {
        let dir = test_dir("autocommit");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let role = RoleDescriptor {
            name: "autocommit".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };
        let record = catalog_wal::create_role_record(TxnId::default(), &role).unwrap();
        writer.append(&record).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.roles.contains_key("autocommit"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_distinguishes_reused_txn_ids_across_wal_history() {
        let dir = test_dir("reused_txn_ids");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let schema = SchemaDescriptor {
            schema_id: SchemaId::new(100),
            name: "schema_from_first_txn".to_owned(),
        };
        let first_role = RoleDescriptor {
            name: "role_from_second_txn".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };

        writer
            .append(&WalRecord::BeginTxn {
                txn_id: TxnId::new(1),
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::create_schema_record(TxnId::new(1), &schema).unwrap())
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: TxnId::new(1),
                commit_ts: 1,
            })
            .unwrap();
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: TxnId::new(1),
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::create_role_record(TxnId::new(1), &first_role).unwrap())
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: TxnId::new(1),
                commit_ts: 2,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.schema_names.contains_key("schema_from_first_txn"));
        assert!(state.roles.contains_key("role_from_second_txn"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_catalog_store_explicit_commit() {
        let dir = test_dir("store_explicit_commit");
        let wal = Arc::new(CatalogWalHandle::open(test_config(dir.clone())).unwrap());
        let store = CatalogStore::new_with_wal(Arc::clone(&wal));
        let txn = TxnId::new(44);

        store.begin_txn(txn).unwrap();
        store
            .create_role(
                txn,
                RoleDescriptor {
                    name: "explicit_commit".to_owned(),
                    login: true,
                    superuser: false,
                    password_hash: None,
                    ..RoleDescriptor::default()
                },
            )
            .unwrap();
        store.commit_txn(txn).unwrap();
        drop(store);
        drop(wal);

        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.roles.contains_key("explicit_commit"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_aborted_txn_not_replayed() {
        let dir = test_dir("wal_aborted");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let txn = TxnId::new(1);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let desc = SchemaDescriptor {
            schema_id: SchemaId::new(50),
            name: "should_not_exist".to_owned(),
        };
        let record = catalog_wal::create_schema_record(txn, &desc).unwrap();
        writer.append(&record).unwrap();

        writer.append(&WalRecord::AbortTxn { txn_id: txn }).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let state = recover_catalog_state(&dir).unwrap();
        assert!(!state.schema_names.contains_key("should_not_exist"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_snapshot_plus_wal() {
        let dir = test_dir("snapshot_plus_wal");

        // Create a state with a schema and snapshot it
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);
        let sid = SchemaId::new(state.next_schema_id);
        state.next_schema_id += 1;
        state.schemas_by_id.insert(
            sid,
            SchemaDescriptor {
                schema_id: sid,
                name: "from_snapshot".to_owned(),
            },
        );
        state.schema_names.insert("from_snapshot".to_owned(), sid);

        let snapshot_lsn = Lsn::new(5);
        save_catalog_snapshot(&state, snapshot_lsn, &dir).unwrap();

        // Write WAL entries after the snapshot
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        // Advance past snapshot LSN
        for _ in 0..5 {
            writer
                .append(&WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                })
                .unwrap();
        }

        let txn = TxnId::new(100);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let role = RoleDescriptor {
            name: "from_wal".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };
        let record = catalog_wal::create_role_record(txn, &role).unwrap();
        writer.append(&record).unwrap();

        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        // Recover: should have both snapshot data and WAL data
        let recovered = recover_catalog_state(&dir).unwrap();
        assert!(
            recovered.schema_names.contains_key("from_snapshot"),
            "schema from snapshot should exist"
        );
        assert!(
            recovered.roles.contains_key("from_wal"),
            "role from WAL should exist"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_full_cycle() {
        let dir = test_dir("full_cycle");

        // Create initial state, snapshot, then add more via WAL
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        // Add a table via direct state manipulation (pre-snapshot)
        let tid = RelationId::new(state.next_table_id);
        state.next_table_id += 1;
        let table = TableDescriptor {
            table_id: tid,
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "pre_crash"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        state
            .table_names
            .insert((SchemaId::new(1), "pre_crash".to_owned()), tid);
        state.tables_by_id.insert(tid, table.clone());

        let snapshot_lsn = Lsn::new(3);
        save_catalog_snapshot(&state, snapshot_lsn, &dir).unwrap();

        // Write WAL entries to add a role after the snapshot
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..3 {
            writer
                .append(&WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                })
                .unwrap();
        }

        let txn = TxnId::new(50);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let role = RoleDescriptor {
            name: "post_crash_role".to_owned(),
            login: true,
            superuser: true,
            password_hash: Some("hash".to_owned()),
            ..RoleDescriptor::default()
        };
        let record = catalog_wal::create_role_record(txn, &role).unwrap();
        writer.append(&record).unwrap();

        // Also add a table descriptor via WAL
        let table2 = TableDescriptor {
            table_id: RelationId::new(99),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "post_crash"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(10),
                name: "val".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        let table_record = catalog_wal::set_table_descriptor_record(txn, &table2).unwrap();
        writer.append(&table_record).unwrap();

        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        // Recover
        let recovered = recover_catalog_state(&dir).unwrap();

        // Pre-crash table from snapshot
        assert!(
            recovered.tables_by_id.contains_key(&tid),
            "pre_crash table should exist"
        );
        // Post-crash role from WAL
        assert!(
            recovered.roles.contains_key("post_crash_role"),
            "post_crash_role should exist"
        );
        // Post-crash table from WAL
        assert!(
            recovered.tables_by_id.contains_key(&RelationId::new(99)),
            "post_crash table should exist"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_nonexistent_dir_returns_bootstrapped() {
        let dir = test_dir("nonexistent_dir");
        // Don't create the directory
        let state = recover_catalog_state(&dir).unwrap();
        assert!(state.schema_names.contains_key("public"));
        // No cleanup needed since dir doesn't exist
    }

    #[test]
    fn recovery_replays_catalog_statistics_and_graph_labels() {
        let dir = test_dir("stats_and_graph_labels");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let txn = TxnId::new(7);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();

        let node_table = TableDescriptor {
            table_id: RelationId::new(101),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "people"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        let edge_table = TableDescriptor {
            table_id: RelationId::new(102),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "knows_edges"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        writer
            .append(&catalog_wal::set_table_descriptor_record(txn, &node_table).unwrap())
            .unwrap();
        writer
            .append(&catalog_wal::set_table_descriptor_record(txn, &edge_table).unwrap())
            .unwrap();

        writer
            .append(
                &catalog_wal::update_statistics_record(
                    txn,
                    &TableStatistics {
                        table_id: node_table.table_id,
                        row_count: 42,
                        total_bytes: 4096,
                        dead_row_count: 2,
                        last_updated_by: Some(txn),
                        column_stats: Vec::new(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer
            .append(
                &catalog_wal::create_node_label_record(
                    txn,
                    &NodeLabelDescriptor {
                        label: "person".to_owned(),
                        table_id: node_table.table_id,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        writer
            .append(
                &catalog_wal::create_edge_label_record(
                    txn,
                    &EdgeLabelDescriptor {
                        label: "knows".to_owned(),
                        table_id: edge_table.table_id,
                        source_label: "person".to_owned(),
                        target_label: "person".to_owned(),
                        endpoints: None,
                    },
                )
                .unwrap(),
            )
            .unwrap();

        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let recovered = recover_catalog_state(&dir).unwrap();
        let stats = recovered.statistics.get(&node_table.table_id).unwrap();
        assert_eq!(stats.row_count, 42);
        assert_eq!(stats.total_bytes, 4096);
        assert!(recovered.node_labels.contains_key("person"));
        assert!(recovered.edge_labels.contains_key("knows"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_drop_table_and_drop_index() {
        let dir = test_dir("drop_table_and_drop_index");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        let keep_table = TableDescriptor {
            table_id: RelationId::new(201),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "keep_table"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        let drop_table = TableDescriptor {
            table_id: RelationId::new(202),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "drop_table"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        state.table_names.insert(
            (SchemaId::new(1), "keep_table".to_owned()),
            keep_table.table_id,
        );
        state
            .tables_by_id
            .insert(keep_table.table_id, keep_table.clone());
        state.table_names.insert(
            (SchemaId::new(1), "drop_table".to_owned()),
            drop_table.table_id,
        );
        state
            .tables_by_id
            .insert(drop_table.table_id, drop_table.clone());

        let keep_index_id = IndexId::new(301);
        let drop_index_id = IndexId::new(302);
        let keep_index = IndexDescriptor {
            index_id: keep_index_id,
            schema_id: SchemaId::new(1),
            table_id: keep_table.table_id,
            name: QualifiedName::qualified("public", "keep_idx"),
            unique: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
            nulls_not_distinct: false,
        };
        let drop_index = IndexDescriptor {
            index_id: drop_index_id,
            schema_id: SchemaId::new(1),
            table_id: drop_table.table_id,
            name: QualifiedName::qualified("public", "drop_idx"),
            unique: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(2),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
            nulls_not_distinct: false,
        };
        state
            .index_names
            .insert((SchemaId::new(1), "keep_idx".to_owned()), keep_index_id);
        state.indexes_by_id.insert(keep_index_id, keep_index);
        state
            .indexes_by_table
            .insert(keep_table.table_id, vec![keep_index_id]);
        state
            .index_names
            .insert((SchemaId::new(1), "drop_idx".to_owned()), drop_index_id);
        state.indexes_by_id.insert(drop_index_id, drop_index);
        state
            .indexes_by_table
            .insert(drop_table.table_id, vec![drop_index_id]);
        state.statistics.insert(
            drop_table.table_id,
            TableStatistics {
                table_id: drop_table.table_id,
                row_count: 10,
                total_bytes: 1000,
                dead_row_count: 1,
                last_updated_by: None,
                column_stats: Vec::new(),
            },
        );

        let seq_id = SequenceId::new(401);
        let sequence = SequenceDescriptor {
            sequence_id: seq_id,
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "drop_table_id_seq"),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: 1,
            cycle: false,
            owned_by: Some((drop_table.table_id, ColumnId::new(2))),
            owner: None,
        };
        state
            .sequence_names
            .insert((SchemaId::new(1), "drop_table_id_seq".to_owned()), seq_id);
        state.sequences_by_id.insert(seq_id, sequence.clone());
        state.sequence_values.insert(
            seq_id,
            crate::CatalogStore::default_sequence_state(&sequence),
        );
        state.triggers.push(TriggerDescriptor {
            name: "drop_trg".to_owned(),
            table_name: "drop_table".to_owned(),
            timing: TriggerTimingDescriptor::Before,
            event: TriggerEventDescriptor::Insert,
            extra_events: Vec::new(),
            function_name: "drop_fn".to_owned(),
            for_each_row: true,
            function_args: Vec::new(),
            update_columns: Vec::new(),
        });

        let snapshot_lsn = Lsn::new(2);
        save_catalog_snapshot(&state, snapshot_lsn, &dir).unwrap();

        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..2 {
            writer
                .append(&WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                })
                .unwrap();
        }

        let txn = TxnId::new(8);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::drop_index_record(txn, keep_index_id))
            .unwrap();
        writer
            .append(&catalog_wal::drop_table_record(txn, drop_table.table_id))
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let recovered = recover_catalog_state(&dir).unwrap();
        assert!(recovered.tables_by_id.contains_key(&keep_table.table_id));
        assert!(!recovered.indexes_by_id.contains_key(&keep_index_id));
        assert!(!recovered.tables_by_id.contains_key(&drop_table.table_id));
        assert!(!recovered.statistics.contains_key(&drop_table.table_id));
        assert!(!recovered.indexes_by_id.contains_key(&drop_index_id));
        assert!(!recovered.sequences_by_id.contains_key(&seq_id));
        assert!(recovered.triggers.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_create_and_alter_index_descriptors() {
        let dir = test_dir("create_and_alter_index_descriptors");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        let table = TableDescriptor {
            table_id: RelationId::new(501),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "users"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        state
            .table_names
            .insert((SchemaId::new(1), "users".to_owned()), table.table_id);
        state.tables_by_id.insert(table.table_id, table.clone());

        let snapshot_lsn = Lsn::new(2);
        save_catalog_snapshot(&state, snapshot_lsn, &dir).unwrap();

        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..2 {
            writer
                .append(&WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                })
                .unwrap();
        }

        let txn = TxnId::new(9);
        let initial_index = IndexDescriptor {
            index_id: IndexId::new(601),
            schema_id: SchemaId::new(1),
            table_id: table.table_id,
            name: QualifiedName::qualified("public", "old_idx"),
            unique: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
            nulls_not_distinct: false,
        };
        let renamed_index = IndexDescriptor {
            name: QualifiedName::qualified("public", "new_idx"),
            ..initial_index.clone()
        };

        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::set_index_descriptor_record(txn, &initial_index).unwrap())
            .unwrap();
        writer
            .append(&catalog_wal::set_index_descriptor_record(txn, &renamed_index).unwrap())
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let recovered = recover_catalog_state(&dir).unwrap();
        let recovered_index = recovered
            .indexes_by_id
            .get(&renamed_index.index_id)
            .expect("index should be recovered");
        assert_eq!(recovered_index.name.name, "new_idx");
        assert!(!recovered
            .index_names
            .contains_key(&(SchemaId::new(1), "old_idx".to_owned())));
        assert_eq!(
            recovered
                .index_names
                .get(&(SchemaId::new(1), "new_idx".to_owned())),
            Some(&renamed_index.index_id)
        );
        assert_eq!(
            recovered.indexes_by_table.get(&table.table_id),
            Some(&vec![renamed_index.index_id])
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_create_tenant() {
        let dir = test_dir("create_tenant");
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();

        let txn = TxnId::new(10);
        let tenant = TenantDescriptor {
            tenant_id: aiondb_core::TenantId::new(1),
            name: "acme".to_owned(),
            schema_id: SchemaId::new(10),
        };
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::create_tenant_record(txn, &tenant).unwrap())
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);

        let recovered = recover_catalog_state(&dir).unwrap();
        assert_eq!(recovered.tenants_by_name.get("acme"), Some(&tenant),);
        assert_eq!(
            recovered.schema_names.get("tenant_acme"),
            Some(&tenant.schema_id)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_replays_drop_tenant_with_cascade() {
        let dir = test_dir("drop_tenant");
        let mut state = CatalogState::default();
        bootstrap::bootstrap_state(&mut state);

        let tenant = TenantDescriptor {
            tenant_id: aiondb_core::TenantId::new(1),
            name: "acme".to_owned(),
            schema_id: SchemaId::new(10),
        };
        state
            .tenants_by_name
            .insert("acme".to_owned(), tenant.clone());
        state.schemas_by_id.insert(
            tenant.schema_id,
            SchemaDescriptor {
                schema_id: tenant.schema_id,
                name: "tenant_acme".to_owned(),
            },
        );
        state
            .schema_names
            .insert("tenant_acme".to_owned(), tenant.schema_id);

        let table = TableDescriptor {
            table_id: RelationId::new(701),
            schema_id: tenant.schema_id,
            name: QualifiedName::qualified("tenant_acme", "users"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        state
            .table_names
            .insert((tenant.schema_id, "users".to_owned()), table.table_id);
        state.tables_by_id.insert(table.table_id, table.clone());
        state.statistics.insert(
            table.table_id,
            TableStatistics {
                table_id: table.table_id,
                row_count: 1,
                total_bytes: 64,
                dead_row_count: 0,
                last_updated_by: None,
                column_stats: Vec::new(),
            },
        );

        let index = IndexDescriptor {
            index_id: IndexId::new(702),
            schema_id: tenant.schema_id,
            table_id: table.table_id,
            name: QualifiedName::qualified("tenant_acme", "users_idx"),
            unique: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
            nulls_not_distinct: false,
        };
        state
            .index_names
            .insert((tenant.schema_id, "users_idx".to_owned()), index.index_id);
        state.indexes_by_id.insert(index.index_id, index.clone());
        state
            .indexes_by_table
            .insert(table.table_id, vec![index.index_id]);

        let sequence = SequenceDescriptor {
            sequence_id: SequenceId::new(703),
            schema_id: tenant.schema_id,
            name: QualifiedName::qualified("tenant_acme", "users_id_seq"),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: 1,
            cycle: false,
            owned_by: None,
            owner: None,
        };
        state.sequence_names.insert(
            (tenant.schema_id, "users_id_seq".to_owned()),
            sequence.sequence_id,
        );
        state
            .sequences_by_id
            .insert(sequence.sequence_id, sequence.clone());
        state.sequence_values.insert(
            sequence.sequence_id,
            crate::CatalogStore::default_sequence_state(&sequence),
        );
        let view = ViewDescriptor {
            view_id: RelationId::new(704),
            schema_id: tenant.schema_id,
            name: QualifiedName::qualified("tenant_acme", "users_view"),
            query_sql: "SELECT id FROM tenant_acme.users".to_owned(),
            creation_search_path_schemas: Vec::new(),
            check_option: None,
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
        };
        state
            .view_names
            .insert((tenant.schema_id, "users_view".to_owned()), view.view_id);
        state.views_by_id.insert(view.view_id, view);
        let snapshot_lsn = Lsn::new(2);
        save_catalog_snapshot(&state, snapshot_lsn, &dir).unwrap();
        let config = test_config(dir.clone());
        let mut writer = WalWriter::open(config).unwrap();
        for _ in 0..2 {
            writer
                .append(&WalRecord::Checkpoint {
                    last_committed_lsn: Lsn::ZERO,
                })
                .unwrap();
        }
        let txn = TxnId::new(11);
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: txn,
                isolation: aiondb_wal::IsolationLevel::ReadCommitted,
            })
            .unwrap();
        writer
            .append(&catalog_wal::drop_tenant_record(txn, "acme"))
            .unwrap();
        writer
            .append(&WalRecord::CommitTxn {
                txn_id: txn,
                commit_ts: 1,
            })
            .unwrap();
        writer.flush().unwrap();
        drop(writer);
        let recovered = recover_catalog_state(&dir).unwrap();
        assert!(!recovered.tenants_by_name.contains_key("acme"));
        assert!(!recovered.schemas_by_id.contains_key(&tenant.schema_id));
        assert!(!recovered.tables_by_id.contains_key(&table.table_id));
        assert!(!recovered.indexes_by_id.contains_key(&index.index_id));
        assert!(!recovered
            .sequences_by_id
            .contains_key(&sequence.sequence_id));
        assert!(!recovered.views_by_id.contains_key(&RelationId::new(704)));
        assert!(!recovered.statistics.contains_key(&table.table_id));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
