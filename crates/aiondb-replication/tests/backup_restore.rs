//! Backup + restore round trip.
//!
//! Take a snapshot of a KV engine, encode it, persist to disk,
//! restore on a fresh engine, and assert state equivalence.

use std::sync::Arc;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_ha::raft_snapshot::{apply_snapshot, decode_snapshot, encode_snapshot, take_snapshot};
use aiondb_replication::backup_coordinator::{BackupCoordinator, RangeManifestEntry};

#[test]
fn full_backup_and_restore_preserves_state() {
    // SOURCE node : load data.
    let src_tmp = tempfile::tempdir().unwrap();
    let src_reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), src_tmp.path()).unwrap());
    let g = MultiRaftGroupId::new(1);
    src_reg.create_group(g, 1).unwrap();
    src_reg.become_leader(g, &[]).unwrap();
    let src_engine = KvEngine::new(Arc::clone(&src_reg));
    for i in 0..100u8 {
        src_engine.put(g, vec![i], vec![i, i, 0xFF]).unwrap();
    }
    let src_state = src_engine.snapshot(g);
    assert_eq!(src_state.len(), 100);

    // BACKUP : encode snapshot, persist via coordinator manifest.
    let coord = BackupCoordinator::new();
    let manifest_id = coord.start_full(0);
    let snap = take_snapshot(&src_reg, &src_engine, g).unwrap();
    let bytes = encode_snapshot(&snap).unwrap();
    let backup_tmp = tempfile::tempdir().unwrap();
    let backup_path = backup_tmp.path().join("range-1.bin");
    std::fs::write(&backup_path, &bytes).unwrap();
    coord.record_range(
        &manifest_id,
        RangeManifestEntry {
            range_id: 1,
            start_key: b"".to_vec(),
            end_key: b"".to_vec(),
            bytes: bytes.len() as u64,
            from_ts_us: 0,
            to_ts_us: snap.index,
            artefact_path: backup_path.to_string_lossy().into_owned(),
        },
    );
    let manifest = coord.finalize(&manifest_id).unwrap();
    assert_eq!(manifest.ranges.len(), 1);

    // RESTORE on a fresh node.
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst_reg = Arc::new(MultiRaftRegistry::new(NodeId::new(2), dst_tmp.path()).unwrap());
    dst_reg.create_group(g, 1).unwrap();
    dst_reg.become_leader(g, &[]).unwrap();
    let dst_engine = KvEngine::new(Arc::clone(&dst_reg));
    assert!(dst_engine.snapshot(g).is_empty());

    let restored_bytes = std::fs::read(&manifest.ranges[0].artefact_path).unwrap();
    let restored_snap = decode_snapshot(&restored_bytes).unwrap();
    apply_snapshot(&dst_reg, &dst_engine, &restored_snap).unwrap();

    let restored_state = dst_engine.snapshot(g);
    assert_eq!(restored_state, src_state);
}
