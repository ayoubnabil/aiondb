use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::TxnId;

use super::*;
use crate::record::{WalEntry, WalRecord};

fn temp_wal_dir(prefix: &str) -> PathBuf {
    let path = crate::segment::test_dir(&format!("replication-{prefix}"));
    fs::create_dir_all(&path).expect("create temp wal directory");
    path
}

fn encode_begin_txn_entry(lsn: u64, txn_id: u64) -> Vec<u8> {
    codec::encode_entry(&WalEntry {
        lsn: Lsn::new(lsn),
        prev_lsn: if lsn > 0 {
            Lsn::new(lsn.saturating_sub(1))
        } else {
            Lsn::ZERO
        },
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: WalRecord::BeginTxn {
            txn_id: TxnId::new(txn_id),
            isolation: crate::IsolationLevel::ReadCommitted,
        },
    })
    .expect("encode WAL entry")
}

fn encode_begin_txn_entry_with_prev(lsn: u64, prev_lsn: u64, txn_id: u64) -> Vec<u8> {
    codec::encode_entry(&WalEntry {
        lsn: Lsn::new(lsn),
        prev_lsn: Lsn::new(prev_lsn),
        database_id: WalEntry::LEGACY_DATABASE_ID,
        record: WalRecord::BeginTxn {
            txn_id: TxnId::new(txn_id),
            isolation: crate::IsolationLevel::ReadCommitted,
        },
    })
    .expect("encode WAL entry")
}

fn raw_replication_frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let payload_len = u32::try_from(payload.len()).expect("test payload should fit u32");
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(tag);
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn replication_message_wal_data_roundtrip() {
    let msg = ReplicationMessage::WalData {
        start_lsn: Lsn::new(10),
        end_lsn: Lsn::new(12),
        data: vec![1, 2, 3, 4, 5],
    };
    let encoded = msg.encode().expect("encode replication message");
    let (decoded, consumed) = ReplicationMessage::decode(&encoded).expect("decode");
    assert_eq!(decoded, msg);
    assert_eq!(consumed, encoded.len());
}

#[test]
fn replication_decode_rejects_empty_wal_data_payload() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&10u64.to_le_bytes());
    payload.extend_from_slice(&10u64.to_le_bytes());

    let encoded = raw_replication_frame(MSG_WAL_DATA, &payload);
    let err = ReplicationMessage::decode(&encoded).expect_err("empty WalData must fail");
    assert!(err.to_string().contains("contains no WAL entries"));
}

#[test]
fn replication_decode_rejects_descending_wal_data_range() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&12u64.to_le_bytes());
    payload.extend_from_slice(&10u64.to_le_bytes());
    payload.push(1);

    let encoded = raw_replication_frame(MSG_WAL_DATA, &payload);
    let err = ReplicationMessage::decode(&encoded).expect_err("descending WalData must fail");
    assert!(err.to_string().contains("start_lsn 12 exceeds end_lsn 10"));
}

#[test]
fn replication_decode_rejects_inconsistent_status_progress() {
    let mut flush_ahead = Vec::new();
    flush_ahead.extend_from_slice(&10u64.to_le_bytes());
    flush_ahead.extend_from_slice(&11u64.to_le_bytes());
    flush_ahead.extend_from_slice(&10u64.to_le_bytes());
    let encoded = raw_replication_frame(MSG_STANDBY_STATUS_UPDATE, &flush_ahead);
    let err = ReplicationMessage::decode(&encoded).expect_err("flush > write must fail");
    assert!(err
        .to_string()
        .contains("flush_lsn 11 exceeds write_lsn 10"));

    let mut apply_ahead = Vec::new();
    apply_ahead.extend_from_slice(&10u64.to_le_bytes());
    apply_ahead.extend_from_slice(&9u64.to_le_bytes());
    apply_ahead.extend_from_slice(&10u64.to_le_bytes());
    let encoded = raw_replication_frame(MSG_STANDBY_STATUS_UPDATE, &apply_ahead);
    let err = ReplicationMessage::decode(&encoded).expect_err("apply > flush must fail");
    assert!(err.to_string().contains("apply_lsn 10 exceeds flush_lsn 9"));
}

#[test]
fn replication_encode_rejects_frames_decode_would_refuse() {
    let empty_wal_data = ReplicationMessage::WalData {
        start_lsn: Lsn::new(10),
        end_lsn: Lsn::new(10),
        data: Vec::new(),
    };
    assert!(empty_wal_data
        .encode()
        .expect_err("empty WalData encode must fail")
        .to_string()
        .contains("contains no WAL entries"));

    let descending_wal_data = ReplicationMessage::WalData {
        start_lsn: Lsn::new(12),
        end_lsn: Lsn::new(10),
        data: vec![1],
    };
    assert!(descending_wal_data
        .encode()
        .expect_err("descending WalData encode must fail")
        .to_string()
        .contains("start_lsn 12 exceeds end_lsn 10"));

    let flush_ahead = ReplicationMessage::StandbyStatusUpdate {
        write_lsn: Lsn::new(10),
        flush_lsn: Lsn::new(11),
        apply_lsn: Lsn::new(10),
    };
    assert!(flush_ahead
        .encode()
        .expect_err("flush > write encode must fail")
        .to_string()
        .contains("flush_lsn 11 exceeds write_lsn 10"));
}

#[test]
fn replication_decode_rejects_oversized_payload_header() {
    let mut frame = Vec::new();
    frame.push(MSG_WAL_DATA);
    frame.extend_from_slice(
        &(u32::try_from(MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES + 1)
            .expect("payload bound should fit into u32"))
        .to_le_bytes(),
    );
    let err = ReplicationMessage::decode(&frame).expect_err("oversized payload must fail");
    assert!(err.to_string().contains("payload too large"));
}

#[test]
fn replication_encode_rejects_payload_above_decode_limit() {
    let msg = ReplicationMessage::WalData {
        start_lsn: Lsn::new(10),
        end_lsn: Lsn::new(10),
        data: vec![0; MAX_REPLICATION_MESSAGE_PAYLOAD_BYTES],
    };

    let err = msg
        .encode()
        .expect_err("encode should reject frames the decoder refuses");
    assert!(err.to_string().contains("payload too large"), "{err}");
}

#[test]
fn replication_decode_rejects_truncated_payload() {
    let mut frame = Vec::new();
    frame.push(MSG_KEEPALIVE_REPLY);
    frame.extend_from_slice(&8_u32.to_le_bytes());
    frame.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
    let err = ReplicationMessage::decode(&frame).expect_err("truncated payload must fail");
    assert!(err.to_string().contains("truncated message payload"));
}

#[test]
fn replication_decode_rejects_trailing_bytes_in_fixed_status_update() {
    let mut msg = ReplicationMessage::StandbyStatusUpdate {
        write_lsn: Lsn::new(10),
        flush_lsn: Lsn::new(9),
        apply_lsn: Lsn::new(8),
    }
    .encode()
    .expect("encode replication message");
    msg[1..5].copy_from_slice(&25_u32.to_le_bytes());
    msg.push(0);

    let err = ReplicationMessage::decode(&msg).expect_err("trailing byte must fail");
    assert!(err.to_string().contains("exactly 24 bytes"));
}

#[test]
fn replication_decode_rejects_trailing_bytes_in_keepalive() {
    let mut msg = ReplicationMessage::Keepalive {
        wal_end: Lsn::new(10),
        timestamp_us: 123,
        reply_requested: true,
    }
    .encode()
    .expect("encode replication message");
    msg[1..5].copy_from_slice(&18_u32.to_le_bytes());
    msg.push(0);

    let err = ReplicationMessage::decode(&msg).expect_err("trailing byte must fail");
    assert!(err.to_string().contains("exactly 17 bytes"));
}

#[test]
fn replication_decode_rejects_trailing_bytes_in_keepalive_reply() {
    let mut msg = ReplicationMessage::KeepaliveReply { timestamp_us: 123 }
        .encode()
        .expect("encode replication message");
    msg[1..5].copy_from_slice(&9_u32.to_le_bytes());
    msg.push(0);

    let err = ReplicationMessage::decode(&msg).expect_err("trailing byte must fail");
    assert!(err.to_string().contains("exactly 8 bytes"));
}

#[test]
fn replication_decode_rejects_non_canonical_keepalive_bool() {
    let mut msg = ReplicationMessage::Keepalive {
        wal_end: Lsn::new(10),
        timestamp_us: 123,
        reply_requested: true,
    }
    .encode()
    .expect("encode replication message");
    let last = msg.last_mut().expect("keepalive has reply flag");
    *last = 2;

    let err = ReplicationMessage::decode(&msg).expect_err("invalid bool flag must fail");
    assert!(err
        .to_string()
        .contains("invalid Keepalive reply_requested"));
}

#[test]
fn replica_registry_recovers_from_poisoned_lock() {
    let registry = Arc::new(ReplicaRegistry::new());
    let registry_for_poison = Arc::clone(&registry);
    let _ = std::thread::spawn(move || {
        let _guard = registry_for_poison.inner.write().expect("lock");
        panic!("poison replica registry lock");
    })
    .join();

    let id = registry.register(Lsn::new(1)).expect("register replica");
    assert_eq!(registry.count(), 1);
    registry.unregister(id);
    assert_eq!(registry.count(), 0);
}

#[test]
fn fresh_replica_retention_uses_start_lsn_before_first_flush() {
    let registry = ReplicaRegistry::new();
    let replica_id = registry.register(Lsn::new(41)).expect("register replica");
    let states = registry.snapshot();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].id, replica_id);
    assert_eq!(states[0].flush_lsn, Lsn::ZERO);
    assert_eq!(states[0].retention_lsn(), Lsn::new(41));
    assert_eq!(registry.min_retention_lsn(), Some(Lsn::new(41)));
}

#[test]
fn replica_retention_advances_with_flush_lsn() {
    let registry = ReplicaRegistry::new();
    let replica_id = registry.register(Lsn::new(10)).expect("register replica");
    registry
        .update_progress(replica_id, Lsn::new(27), Lsn::new(24), Lsn::new(20))
        .expect("slot progress update should persist");
    let states = registry.snapshot();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].retention_lsn(), Lsn::new(24));
    assert_eq!(registry.min_retention_lsn(), Some(Lsn::new(24)));
}

#[test]
fn replica_registry_try_register_enforces_limit_atomically() {
    let registry = Arc::new(ReplicaRegistry::new());
    let barrier = Arc::new(std::sync::Barrier::new(8));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            registry.try_register(Lsn::new(1), 1).is_ok()
        }));
    }

    let success_count = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread join"))
        .filter(|ok| *ok)
        .count();

    assert_eq!(success_count, 1, "only one sender slot may be granted");
    assert_eq!(
        registry.count(),
        1,
        "registry must contain exactly one replica"
    );
}

#[test]
fn replica_registry_rejects_replica_id_overflow_without_panicking() {
    let registry = ReplicaRegistry::new();
    {
        let mut inner = registry.inner.write().expect("lock");
        inner.next_id = u64::MAX;
    }

    let err = registry
        .try_register(Lsn::new(1), usize::MAX)
        .expect_err("replica id overflow must fail cleanly");
    assert!(err.to_string().contains("exhausted replica identifiers"));
    assert_eq!(registry.count(), 0);
}

#[test]
fn physical_slot_persists_across_reopen() {
    let slot_dir = temp_wal_dir("slot_persists");
    let registry = ReplicaRegistry::open(slot_dir.clone()).expect("open persistent registry");
    let created = registry
        .create_physical_slot("standby1", Lsn::new(42), true, 1)
        .expect("create slot");
    assert_eq!(created.restart_lsn, Some(Lsn::new(42)));
    drop(registry);

    let reopened = ReplicaRegistry::open(slot_dir.clone()).expect("reopen persistent registry");
    let slot = reopened
        .read_physical_slot("standby1")
        .expect("read slot")
        .expect("slot should persist");
    assert_eq!(slot.restart_lsn, Some(Lsn::new(42)));
    assert_eq!(reopened.min_retention_lsn(), Some(Lsn::new(42)));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_filename_name_mismatch() {
    let slot_dir = temp_wal_dir("slot_name_mismatch");
    fs::write(
        slot_dir.join("standby1.json"),
        br#"{
  "version": 1,
  "slot_name": "standby2",
  "slot_type": "physical",
  "restart_lsn": 42,
  "restart_tli": 1
}"#,
    )
    .expect("write mismatched slot metadata");

    let err =
        ReplicaRegistry::open(slot_dir.clone()).expect_err("mismatched slot metadata must fail");
    assert!(err.to_string().contains("filename/name mismatch"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_unsupported_metadata_version() {
    let slot_dir = temp_wal_dir("slot_bad_version");
    fs::write(
        slot_dir.join("standby1.json"),
        br#"{
  "version": 99,
  "slot_name": "standby1",
  "slot_type": "physical",
  "restart_lsn": 42,
  "restart_tli": 1
}"#,
    )
    .expect("write invalid slot metadata");

    let err = ReplicaRegistry::open(slot_dir.clone()).expect_err("bad version must fail");
    assert!(err.to_string().contains("unsupported version"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_unsupported_slot_type() {
    let slot_dir = temp_wal_dir("slot_bad_type");
    fs::write(
        slot_dir.join("standby1.json"),
        br#"{
  "version": 1,
  "slot_name": "standby1",
  "slot_type": "logical",
  "restart_lsn": 42,
  "restart_tli": 1
}"#,
    )
    .expect("write invalid slot metadata");

    let err = ReplicaRegistry::open(slot_dir.clone()).expect_err("bad slot_type must fail");
    assert!(err.to_string().contains("unsupported slot_type"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_malformed_restart_lsn() {
    let slot_dir = temp_wal_dir("slot_bad_restart_lsn");
    fs::write(
        slot_dir.join("standby1.json"),
        br#"{
  "version": 1,
  "slot_name": "standby1",
  "slot_type": "physical",
  "restart_lsn": "42",
  "restart_tli": 1
}"#,
    )
    .expect("write invalid slot metadata");

    let err = ReplicaRegistry::open(slot_dir.clone()).expect_err("bad restart_lsn must fail");
    assert!(err.to_string().contains("invalid restart_lsn"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_malformed_restart_tli() {
    let slot_dir = temp_wal_dir("slot_bad_restart_tli");
    fs::write(
        slot_dir.join("standby1.json"),
        br#"{
  "version": 1,
  "slot_name": "standby1",
  "slot_type": "physical",
  "restart_lsn": 42,
  "restart_tli": "1"
}"#,
    )
    .expect("write invalid slot metadata");

    let err = ReplicaRegistry::open(slot_dir.clone()).expect_err("bad restart_tli must fail");
    assert!(err.to_string().contains("invalid restart_tli"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn persistent_slot_rejects_oversized_slot_name() {
    let slot_dir = temp_wal_dir("slot_name_too_long");
    let registry = ReplicaRegistry::open(slot_dir.clone()).expect("open persistent registry");
    let oversized_name = "a".repeat(64);

    let err = registry
        .create_physical_slot(&oversized_name, Lsn::new(42), true, 1)
        .expect_err("oversized slot name must fail");
    assert!(err
        .to_string()
        .contains("invalid physical replication slot name"));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn slot_retention_survives_disconnect_and_reopen() {
    let slot_dir = temp_wal_dir("slot_disconnect");
    let registry = ReplicaRegistry::open(slot_dir.clone()).expect("open persistent registry");
    registry
        .create_physical_slot("standby1", Lsn::new(10), false, 1)
        .expect("create slot");
    let replica_id = registry
        .try_register_with_slot(Lsn::new(11), Some("standby1"), 1)
        .expect("register replica on slot");
    assert_eq!(registry.min_retention_lsn(), Some(Lsn::new(11)));

    registry
        .update_progress(replica_id, Lsn::new(20), Lsn::new(17), Lsn::new(15))
        .expect("slot progress update should persist");
    registry.unregister(replica_id);
    assert_eq!(registry.min_retention_lsn(), Some(Lsn::new(17)));
    drop(registry);

    let reopened = ReplicaRegistry::open(slot_dir.clone()).expect("reopen persistent registry");
    let slot = reopened
        .read_physical_slot("standby1")
        .expect("read slot")
        .expect("slot should persist");
    assert_eq!(slot.restart_lsn, Some(Lsn::new(17)));
    assert_eq!(reopened.min_retention_lsn(), Some(Lsn::new(17)));

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn physical_slot_cannot_be_activated_twice() {
    let slot_dir = temp_wal_dir("slot_active");
    let registry = ReplicaRegistry::open(slot_dir.clone()).expect("open persistent registry");
    registry
        .create_physical_slot("standby1", Lsn::new(5), false, 1)
        .expect("create slot");

    let replica_id = registry
        .try_register_with_slot(Lsn::new(5), Some("standby1"), 2)
        .expect("first slot activation should succeed");
    let err = registry
        .try_register_with_slot(Lsn::new(5), Some("standby1"), 2)
        .expect_err("slot should reject concurrent activation");
    assert!(err.to_string().contains("already active"));

    registry.unregister(replica_id);
    registry
        .try_register_with_slot(Lsn::new(6), Some("standby1"), 2)
        .expect("slot should be reusable after disconnect");

    let _ = fs::remove_dir_all(slot_dir);
}

#[test]
fn wal_notifier_wait_recovers_from_poisoned_lock() {
    let notifier = Arc::new(WalNotifier::new(Lsn::new(10)));
    let notifier_for_poison = Arc::clone(&notifier);
    let _ = std::thread::spawn(move || {
        let _guard = notifier_for_poison.lock.lock().expect("lock");
        panic!("poison WAL notifier lock");
    })
    .join();

    let current = notifier.wait_for_new_wal(Lsn::new(10), Duration::from_millis(1));
    assert_eq!(current, Lsn::new(10));
}

#[tokio::test]
async fn wal_notifier_async_wait_wakes_on_notify() {
    let notifier = Arc::new(WalNotifier::new(Lsn::new(10)));
    let waiter = {
        let notifier = Arc::clone(&notifier);
        tokio::spawn(async move {
            notifier
                .wait_for_new_wal_async(Lsn::new(10), Duration::from_secs(1))
                .await
        })
    };

    tokio::time::sleep(Duration::from_millis(10)).await;
    notifier.notify_new_wal(Lsn::new(11));

    let woken_lsn = waiter.await.expect("wait task");
    assert_eq!(woken_lsn, Lsn::new(11));
}

#[test]
fn wal_receiver_accepts_contiguous_lsn_batch() {
    let wal_dir = temp_wal_dir("contiguous");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let mut data = Vec::new();
    data.extend_from_slice(&encode_begin_txn_entry(1, 1));
    data.extend_from_slice(&encode_begin_txn_entry(2, 2));

    let last_lsn = receiver
        .receive_wal_data(&data)
        .expect("contiguous WAL should be accepted");
    assert_eq!(last_lsn, Lsn::new(2));
    assert_eq!(receiver.write_lsn(), Lsn::new(2));

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_rejects_out_of_order_lsn_batch() {
    let wal_dir = temp_wal_dir("out_of_order");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let data = encode_begin_txn_entry(2, 1);
    let error = receiver
        .receive_wal_data(&data)
        .expect_err("out-of-order WAL must be rejected");
    assert!(error.to_string().contains("out-of-order LSN"));
    assert_eq!(receiver.write_lsn(), Lsn::ZERO);

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_rejects_lsn_overflow_in_batch() {
    let mut data = Vec::new();
    data.extend_from_slice(&encode_begin_txn_entry_with_prev(u64::MAX, 0, 1));
    data.extend_from_slice(&encode_begin_txn_entry_with_prev(u64::MAX, u64::MAX, 2));

    let err = match WalReceiver::decode_incoming_wal_batch(&data, Lsn::ZERO, Lsn::new(u64::MAX)) {
        Ok(_) => panic!("overflowing receiver batch must fail"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("LSN overflow"), "{err}");
}

#[test]
fn wal_receiver_rejects_wal_data_message_with_mismatched_start_lsn() {
    let wal_dir = temp_wal_dir("message_start_mismatch");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let message = ReplicationMessage::WalData {
        start_lsn: Lsn::new(7),
        end_lsn: Lsn::new(1),
        data: encode_begin_txn_entry(1, 1),
    };
    let error = receiver
        .receive_message(&message)
        .expect_err("WalData with mismatched start_lsn must fail");
    assert!(error.to_string().contains("batch start_lsn mismatch"));
    assert_eq!(receiver.write_lsn(), Lsn::ZERO);
    assert_eq!(receiver.status_snapshot().latest_end_lsn, None);

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_rejects_wal_data_message_with_mismatched_end_lsn() {
    let wal_dir = temp_wal_dir("message_end_mismatch");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let mut data = Vec::new();
    data.extend_from_slice(&encode_begin_txn_entry(1, 1));
    data.extend_from_slice(&encode_begin_txn_entry(2, 2));
    let message = ReplicationMessage::WalData {
        start_lsn: Lsn::new(1),
        end_lsn: Lsn::new(9),
        data,
    };
    let error = receiver
        .receive_message(&message)
        .expect_err("WalData with mismatched end_lsn must fail");
    assert!(error.to_string().contains("batch end_lsn mismatch"));
    assert_eq!(receiver.write_lsn(), Lsn::ZERO);

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_sender_errors_when_requested_lsn_is_pruned() {
    let wal_dir = temp_wal_dir("pruned_start");
    let config = WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    };
    let mut writer = WalWriter::open(config).expect("open WAL writer");

    for txn_id in 1..=3 {
        writer
            .append(&WalRecord::BeginTxn {
                txn_id: TxnId::new(txn_id),
                isolation: crate::IsolationLevel::ReadCommitted,
            })
            .expect("append WAL entry");
    }
    writer.flush().expect("flush WAL");
    writer
        .remove_segments_before(Lsn::new(3))
        .expect("prune archived segments");
    drop(writer);

    let mut sender = WalSender::new(wal_dir.clone(), Lsn::new(1), ReplicaId::new(1));
    let error = sender
        .next_batch()
        .expect_err("requesting a pruned LSN must fail");
    let message = error.to_string();
    assert!(message.contains("no longer retained"));
    assert!(message.contains("earliest available LSN is 3"));

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_resyncs_after_failed_append_and_allows_retry() {
    let wal_dir = temp_wal_dir("retry_after_failure");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let mut data = Vec::new();
    data.extend_from_slice(&encode_begin_txn_entry(1, 1));
    data.extend_from_slice(&encode_begin_txn_entry(2, 2));

    crate::writer::inject_fail_next_append_after_write();
    receiver
        .receive_wal_data(&data)
        .expect_err("injected append failure must bubble up");
    assert_eq!(receiver.write_lsn(), Lsn::ZERO);
    assert_eq!(receiver.flush_lsn(), Lsn::ZERO);

    let last_lsn = receiver
        .receive_wal_data(&data)
        .expect("receiver should recover and accept the retried batch");
    assert_eq!(last_lsn, Lsn::new(2));
    assert_eq!(receiver.write_lsn(), Lsn::new(2));
    assert_eq!(receiver.flush_lsn(), Lsn::ZERO);
    assert_eq!(
        receiver.flush_durable().expect("durable flush after retry"),
        Lsn::new(2)
    );
    assert_eq!(receiver.flush_lsn(), Lsn::new(2));

    let mut reader = WalReader::open(wal_dir.clone(), Lsn::new(1)).expect("open WAL reader");
    let entries = reader.collect_all().expect("read back WAL after retry");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].lsn, Lsn::new(1));
    assert_eq!(entries[1].lsn, Lsn::new(2));

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_accepts_matching_upstream_identity() {
    let root = temp_wal_dir("matching_upstream_identity");
    let wal_dir = root.join("wal");
    let replication_dir = root.join("replication");
    fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
    fs::write(replication_dir.join("system_id"), b"42").expect("write local system id");
    fs::write(replication_dir.join("timeline"), b"7").expect("write local timeline");

    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    receiver
        .validate_upstream_identity(42, 7)
        .expect("matching upstream identity should be accepted");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn wal_receiver_rejects_mismatched_upstream_system_identifier() {
    let root = temp_wal_dir("mismatched_upstream_system_id");
    let wal_dir = root.join("wal");
    let replication_dir = root.join("replication");
    fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
    fs::write(replication_dir.join("system_id"), b"42").expect("write local system id");

    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let error = receiver
        .validate_upstream_identity(99, 1)
        .expect_err("mismatched upstream system identifier must fail");
    assert!(error.to_string().contains("system identifier mismatch"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn wal_receiver_rejects_mismatched_upstream_timeline() {
    let root = temp_wal_dir("mismatched_upstream_timeline");
    let wal_dir = root.join("wal");
    let replication_dir = root.join("replication");
    fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
    fs::write(replication_dir.join("timeline"), b"7").expect("write local timeline");

    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let error = receiver
        .validate_upstream_identity(42, 9)
        .expect_err("mismatched upstream timeline must fail");
    assert!(error.to_string().contains("timeline mismatch"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn wal_receiver_status_snapshot_exposes_progress_and_local_identity() {
    let root = temp_wal_dir("receiver_status_snapshot");
    let wal_dir = root.join("wal");
    let replication_dir = root.join("replication");
    fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
    fs::write(replication_dir.join("system_id"), b"42").expect("write local system id");
    fs::write(replication_dir.join("timeline"), b"7").expect("write local timeline");

    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let message = ReplicationMessage::WalData {
        start_lsn: Lsn::new(1),
        end_lsn: Lsn::new(1),
        data: encode_begin_txn_entry(1, 1),
    };
    receiver
        .receive_message(&message)
        .expect("apply wal data for snapshot");
    receiver
        .flush_durable()
        .expect("durable flush for snapshot");
    receiver.set_apply_lsn(Lsn::new(1));

    let snapshot = receiver.status_snapshot();
    assert_eq!(snapshot.receive_start_lsn, Some(Lsn::new(1)));
    assert_eq!(snapshot.write_lsn, Lsn::new(1));
    assert_eq!(snapshot.flush_lsn, Lsn::new(1));
    assert_eq!(snapshot.apply_lsn, Lsn::new(1));
    assert_eq!(snapshot.latest_end_lsn, Some(Lsn::new(1)));
    assert!(snapshot.last_msg_receipt_time.is_some());
    assert!(snapshot.latest_end_time.is_some());
    assert_eq!(snapshot.local_system_identifier, Some(42));
    assert_eq!(snapshot.local_timeline_id, Some(7));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn wal_receiver_apply_lsn_is_bounded_by_flush_lsn() {
    let wal_dir = temp_wal_dir("receiver_apply_bound");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    receiver.set_apply_lsn(Lsn::new(99));
    assert_eq!(receiver.apply_lsn(), Lsn::ZERO);
    receiver.status_update().encode().expect("status is valid");

    receiver
        .receive_wal_data(&encode_begin_txn_entry(1, 1))
        .expect("receive WAL");
    receiver.flush_durable().expect("flush WAL");
    receiver.set_apply_lsn(Lsn::new(99));

    assert_eq!(receiver.apply_lsn(), Lsn::new(1));
    receiver.status_update().encode().expect("status is valid");

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_resync_clamps_apply_lsn_to_flush_lsn() {
    let wal_dir = temp_wal_dir("receiver_resync_apply_bound");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    receiver
        .receive_wal_data(&encode_begin_txn_entry(1, 1))
        .expect("receive WAL");
    receiver.flush_durable().expect("flush WAL");
    receiver.set_apply_lsn(Lsn::new(1));

    let mut writer = receiver.writer.lock().expect("writer lock");
    let mut write_lsn = receiver.write_lsn.lock().expect("write_lsn lock");
    let mut flush_lsn = receiver.flush_lsn.lock().expect("flush_lsn lock");
    *write_lsn = Lsn::ZERO;
    *flush_lsn = Lsn::ZERO;
    receiver
        .resync_after_failed_receive(&mut writer, &mut write_lsn, &mut flush_lsn)
        .expect("resync");
    drop(flush_lsn);
    drop(write_lsn);
    drop(writer);

    assert_eq!(receiver.apply_lsn(), receiver.flush_lsn());
    receiver.status_update().encode().expect("status is valid");

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn wal_receiver_keepalive_updates_sender_and_receipt_timestamps() {
    let wal_dir = temp_wal_dir("receiver_keepalive");
    let receiver = WalReceiver::open(WalConfig {
        dir: wal_dir.clone(),
        segment_max_bytes: 1024 * 1024,
        sync_on_flush: false,
        group_commit_delay_micros: 0,
        wal_compression: WalCompression::None,
        wal_lsn_mode: crate::WalLsnMode::Logical,
    })
    .expect("open WAL receiver");

    let sent_at = current_timestamp_us();
    let last_lsn = receiver
        .receive_message(&ReplicationMessage::Keepalive {
            wal_end: Lsn::new(9),
            timestamp_us: sent_at,
            reply_requested: false,
        })
        .expect("keepalive should be accepted");
    assert_eq!(last_lsn, Lsn::ZERO);

    let snapshot = receiver.status_snapshot();
    assert_eq!(snapshot.latest_end_lsn, Some(Lsn::new(9)));
    assert!(snapshot.last_msg_send_time.is_some());
    assert!(snapshot.last_msg_receipt_time.is_some());
    assert!(snapshot.latest_end_time.is_some());

    let _ = fs::remove_dir_all(wal_dir);
}

#[test]
fn any_replica_flushed_to_returns_false_when_empty() {
    let registry = ReplicaRegistry::new();
    assert!(!registry.any_replica_flushed_to(Lsn::new(1)));
}

#[test]
fn any_replica_flushed_to_tracks_progress() {
    let registry = ReplicaRegistry::new();
    let id = registry.register(Lsn::new(1)).expect("register replica");
    assert!(!registry.any_replica_flushed_to(Lsn::new(5)));
    registry
        .update_progress(id, Lsn::new(5), Lsn::new(5), Lsn::ZERO)
        .unwrap();
    assert!(registry.any_replica_flushed_to(Lsn::new(5)));
    assert!(!registry.any_replica_flushed_to(Lsn::new(6)));
}

#[test]
fn wait_for_any_replica_flush_returns_immediately_when_already_flushed() {
    let registry = ReplicaRegistry::new();
    let id = registry.register(Lsn::new(1)).expect("register replica");
    registry
        .update_progress(id, Lsn::new(10), Lsn::new(10), Lsn::ZERO)
        .unwrap();
    let result =
        registry.wait_for_any_replica_flush(Lsn::new(5), std::time::Duration::from_millis(100));
    assert!(result);
}

#[test]
fn wait_for_any_replica_flush_times_out_when_no_replicas() {
    let registry = ReplicaRegistry::new();
    let result =
        registry.wait_for_any_replica_flush(Lsn::new(5), std::time::Duration::from_millis(50));
    assert!(!result);
}

#[test]
fn wait_for_any_replica_flush_wakes_on_progress() {
    let registry = Arc::new(ReplicaRegistry::new());
    let id = registry.register(Lsn::new(1)).expect("register replica");

    let registry_clone = Arc::clone(&registry);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        registry_clone
            .update_progress(id, Lsn::new(10), Lsn::new(10), Lsn::ZERO)
            .unwrap();
    });

    let result = registry.wait_for_any_replica_flush(Lsn::new(10), Duration::from_secs(5));
    assert!(result);
    handle.join().unwrap();
}

#[test]
fn sync_commit_deadline_handles_unrepresentable_timeout() {
    let deadline = sync_commit_deadline(Duration::MAX);
    assert!(deadline.is_none());
    assert_eq!(
        sync_commit_wait_remaining(deadline),
        MAX_SYNC_COMMIT_WAIT_CHUNK
    );
    assert!(!sync_commit_deadline_expired(deadline));
}

#[test]
fn wait_for_any_replica_flush_handles_unrepresentable_timeout_and_wakes() {
    let registry = Arc::new(ReplicaRegistry::new());
    let id = registry.register(Lsn::new(1)).expect("register replica");

    let registry_clone = Arc::clone(&registry);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        registry_clone
            .update_progress(id, Lsn::new(10), Lsn::new(10), Lsn::ZERO)
            .unwrap();
    });

    let result = registry.wait_for_any_replica_flush(Lsn::new(10), Duration::MAX);
    assert!(result);
    handle.join().unwrap();
}

#[test]
fn wait_for_write_concern_handles_unrepresentable_timeout_and_wakes() {
    let registry = Arc::new(ReplicaRegistry::new());
    let id = registry.register(Lsn::new(1)).expect("register replica");

    let registry_clone = Arc::clone(&registry);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        registry_clone
            .update_progress(id, Lsn::new(10), Lsn::new(10), Lsn::ZERO)
            .unwrap();
    });

    let result = registry.wait_for_write_concern(Lsn::new(10), 1, Duration::MAX);
    assert!(result);
    handle.join().unwrap();
}
