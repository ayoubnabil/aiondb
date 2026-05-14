use super::*;
use std::path::Path;

fn unique_wal_dir(label: &str) -> PathBuf {
    unique_temp_test_dir("replication", label)
}

fn seed_wal_records(dir: &Path, records: &[WalRecord]) -> Lsn {
    let mut writer = WalWriter::open(WalConfig {
        dir: dir.to_path_buf(),
        wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
        ..WalConfig::default()
    })
    .expect("wal writer");
    let mut last_lsn = Lsn::ZERO;
    for record in records {
        last_lsn = writer.append(record).expect("append");
    }
    writer.flush_durable().expect("flush durable");
    last_lsn
}

fn decode_text_row(payload: &[u8]) -> Vec<Option<String>> {
    let column_count = i16::from_be_bytes(payload[0..2].try_into().expect("column count"));
    let mut offset = 2usize;
    let mut columns = Vec::new();
    for _ in 0..column_count {
        let len = i32::from_be_bytes(payload[offset..offset + 4].try_into().expect("len"));
        offset += 4;
        if len == -1 {
            columns.push(None);
            continue;
        }
        let len = usize::try_from(len).expect("positive length");
        columns.push(Some(
            String::from_utf8(payload[offset..offset + len].to_vec()).expect("utf8"),
        ));
        offset += len;
    }
    columns
}

#[tokio::test]
async fn replication_mode_requires_superuser_privilege() {
    let wal_dir = unique_wal_dir("startup_denied");
    let engine =
        Arc::new(ReplicationTestEngine::new(wal_dir, "424242").without_replication_privilege());
    let input = build_startup_bytes_with_params(&[
        ("user", "test"),
        ("database", "replica_db"),
        ("replication", "true"),
    ]);

    let mut conn = make_connection_with_query_engine(input, engine);
    let error = conn
        .run()
        .await
        .expect_err("replication startup without privilege should fail");

    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InsufficientPrivilege
    );
    assert!(error.report().message.contains("superuser"));
    assert_eq!(error_response_codes(&conn.writer), vec!["42501"]);
}

#[tokio::test]
async fn replication_identify_system_returns_cluster_identity() {
    let wal_dir = unique_wal_dir("identify_system");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "424242"));
    let mut input = build_startup_bytes_with_params(&[
        ("user", "test"),
        ("database", "replica_db"),
        ("replication", "true"),
    ]);
    input.extend_from_slice(&build_query_bytes("IDENTIFY_SYSTEM"));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.expect("replication query should succeed");

    let messages = backend_messages(&conn.writer);
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"TDCZ");

    let row = decode_text_row(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1);
    assert_eq!(row[0].as_deref(), Some("424242"));
    assert_eq!(row[1].as_deref(), Some("1"));
    assert_eq!(row[2].as_deref(), Some("0/00000000"));
    assert_eq!(row[3].as_deref(), Some("replica_db"));
}

#[tokio::test]
async fn start_replication_enters_copy_both_and_streams_wal_batch() {
    let wal_dir = unique_wal_dir("start_replication");
    let last_lsn = seed_wal_records(
        &wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(7),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(7),
                commit_ts: 99,
            },
        ],
    );
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "9001"));
    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes("START_REPLICATION 0/1"));

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run()
        .await
        .expect("replication stream should complete");

    let messages = backend_messages(&conn.writer);
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(
        &tags[STARTUP_RESPONSE_TAG_COUNT..STARTUP_RESPONSE_TAG_COUNT + 2],
        b"Wd"
    );

    let (message, consumed) =
        ReplicationMessage::decode(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1)
            .expect("replication payload should decode");
    assert_eq!(consumed, messages[STARTUP_RESPONSE_TAG_COUNT + 1].1.len());
    let ReplicationMessage::WalData {
        start_lsn,
        end_lsn,
        data,
    } = message
    else {
        panic!("expected WalData message");
    };
    assert_eq!(start_lsn, Lsn::new(1));
    assert_eq!(end_lsn, last_lsn);
    assert!(!data.is_empty(), "streamed WAL batch should not be empty");
}

#[tokio::test]
async fn start_replication_tracks_startup_application_name() {
    let wal_dir = unique_wal_dir("start_replication_application_name");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "9001"));
    let mut input = build_startup_bytes_with_params(&[
        ("user", "test"),
        ("replication", "true"),
        ("application_name", "node-d"),
    ]);
    input.extend_from_slice(&build_query_bytes("START_REPLICATION 0/1"));

    let (server_io, mut client_io) = duplex(16 * 1024);
    let (server_reader, server_writer) = split(server_io);
    client_io
        .write_all(&input)
        .await
        .expect("write startup and start replication");

    let mut conn = Connection::new(
        Arc::clone(&engine),
        server_reader,
        server_writer,
        1,
        42,
        CancelRegistry::new(),
    );
    let task = tokio::spawn(async move { conn.run().await });

    let mut observed = None;
    for _ in 0..100 {
        let states = engine.manager.state().replica_states();
        if let Some(state) = states.first() {
            observed = state.application_name.clone();
            if observed.as_deref() == Some("node-d") {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(observed.as_deref(), Some("node-d"));

    client_io
        .write_all(&build_terminate_bytes())
        .await
        .expect("terminate replication stream");
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("connection task should stop")
        .expect("connection task should join")
        .expect("replication connection should close cleanly");
}

#[tokio::test]
async fn replication_mode_rejects_start_replication_without_lsn() {
    let wal_dir = unique_wal_dir("missing_lsn");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "77"));
    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes("START_REPLICATION"));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run()
        .await
        .expect("connection should stay protocol-clean");

    let tags = backend_message_tags(&conn.writer);
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"EZ");
}

#[tokio::test]
async fn create_replication_slot_returns_consistent_point() {
    let wal_dir = unique_wal_dir("create_slot");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "123"));
    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes(
        "CREATE_REPLICATION_SLOT slot_a PHYSICAL RESERVE_WAL",
    ));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.expect("create slot query should succeed");

    let messages = backend_messages(&conn.writer);
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(&tags[STARTUP_RESPONSE_TAG_COUNT..], b"TDCZ");

    let row = decode_text_row(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1);
    assert_eq!(row[0].as_deref(), Some("slot_a"));
    assert_eq!(row[1].as_deref(), Some("0/00000000"));
    assert_eq!(row[2], None);
    assert_eq!(row[3], None);
}

#[tokio::test]
async fn read_replication_slot_reports_existing_slot_state() {
    let wal_dir = unique_wal_dir("read_slot");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "123"));
    engine
        .manager
        .create_physical_slot("slot_a", true)
        .expect("slot creation should succeed");

    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes("READ_REPLICATION_SLOT slot_a"));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run().await.expect("read slot query should succeed");

    let messages = backend_messages(&conn.writer);
    let row = decode_text_row(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1);
    assert_eq!(row[0].as_deref(), Some("physical"));
    assert_eq!(row[1].as_deref(), Some("0/00000000"));
    assert_eq!(row[2].as_deref(), Some("1"));
}

#[tokio::test]
async fn read_replication_slot_returns_nulls_for_missing_slot() {
    let wal_dir = unique_wal_dir("read_missing_slot");
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "123"));
    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes("READ_REPLICATION_SLOT missing_slot"));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run()
        .await
        .expect("read missing slot query should succeed");

    let messages = backend_messages(&conn.writer);
    let row = decode_text_row(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1);
    assert_eq!(row, vec![None, None, None]);
}

#[tokio::test]
async fn start_replication_slot_physical_streams_wal_batch() {
    let wal_dir = unique_wal_dir("slot_stream");
    let last_lsn = seed_wal_records(
        &wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(8),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(8),
                commit_ts: 100,
            },
        ],
    );
    let engine = Arc::new(ReplicationTestEngine::new(wal_dir, "9002"));
    engine
        .manager
        .create_physical_slot("slot_a", false)
        .expect("slot creation should succeed");

    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes(
        "START_REPLICATION SLOT slot_a PHYSICAL 0/1",
    ));

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run()
        .await
        .expect("slot replication stream should complete");

    let messages = backend_messages(&conn.writer);
    let (message, consumed) =
        ReplicationMessage::decode(&messages[STARTUP_RESPONSE_TAG_COUNT + 1].1)
            .expect("replication payload should decode");
    assert_eq!(consumed, messages[STARTUP_RESPONSE_TAG_COUNT + 1].1.len());
    let ReplicationMessage::WalData {
        start_lsn,
        end_lsn,
        data,
    } = message
    else {
        panic!("expected WalData message");
    };
    assert_eq!(start_lsn, Lsn::new(1));
    assert_eq!(end_lsn, last_lsn);
    assert!(!data.is_empty(), "streamed WAL batch should not be empty");
}

#[tokio::test]
async fn base_backup_streams_wal_dir_files_to_replica() {
    use aiondb_replication::base_backup::{decode_frame, BackupFrame};

    let wal_dir = unique_wal_dir("base_backup_e2e");
    // Plant two segment files plus the replication metadata sidecar so
    // the primary's BASE_BACKUP handler has something concrete to stream.
    std::fs::create_dir_all(wal_dir.join("replication")).expect("create replication dir");
    std::fs::write(wal_dir.join("000000010000000000000001"), b"AAAA0001").expect("seed segment 1");
    std::fs::write(wal_dir.join("000000010000000000000002"), b"BBBB0002").expect("seed segment 2");
    std::fs::write(wal_dir.join("replication/system_id"), b"4242\n").expect("seed system id");

    let engine = Arc::new(ReplicationTestEngine::new(wal_dir.clone(), "4242"));
    let mut input = build_startup_bytes_with_params(&[("user", "test"), ("replication", "true")]);
    input.extend_from_slice(&build_query_bytes("BASE_BACKUP"));
    input.extend_from_slice(&build_terminate_bytes());

    let mut conn = make_connection_with_query_engine(input, engine);
    conn.run()
        .await
        .expect("BASE_BACKUP stream should complete");

    let messages = backend_messages(&conn.writer);
    let tags: Vec<u8> = messages.iter().map(|(tag, _)| *tag).collect();
    let trailing = &tags[STARTUP_RESPONSE_TAG_COUNT..];
    assert_eq!(
        trailing.first(),
        Some(&b'H'),
        "first frame must be CopyOutResponse"
    );
    assert!(
        trailing.contains(&b'c'),
        "CopyDone must terminate the stream: {trailing:?}"
    );
    assert!(
        trailing.contains(&b'C'),
        "CommandComplete must follow CopyDone: {trailing:?}"
    );
    assert!(
        trailing.contains(&b'Z'),
        "ReadyForQuery must close the session: {trailing:?}"
    );

    let target = unique_wal_dir("base_backup_target");
    std::fs::remove_dir_all(&target).ok();
    let mut writer = aiondb_replication::base_backup::BaseBackupWriter::new(target.clone())
        .expect("create writer");
    let mut buffer: Vec<u8> = Vec::new();
    for (tag, payload) in messages
        .iter()
        .skip(STARTUP_RESPONSE_TAG_COUNT)
        .filter(|(tag, _)| *tag == b'd')
    {
        assert_eq!(*tag, b'd');
        buffer.extend_from_slice(payload);
        loop {
            if buffer.is_empty() {
                break;
            }
            match decode_frame(&buffer) {
                Ok((frame, consumed)) => {
                    let header_matched = matches!(frame, BackupFrame::Header(_));
                    let _ = writer.apply(frame).expect("apply frame");
                    buffer.drain(..consumed);
                    if header_matched {
                        assert_eq!(
                            writer.header().expect("header observed").system_identifier,
                            "4242"
                        );
                    }
                }
                Err(err) => {
                    assert!(
                        err.to_string().contains("truncated"),
                        "unexpected decode error: {err}"
                    );
                    break;
                }
            }
        }
    }
    assert!(writer.completed(), "BackupEnd frame must have been seen");

    let seg1 = std::fs::read(target.join("000000010000000000000001")).expect("seg1");
    assert_eq!(seg1, b"AAAA0001");
    let seg2 = std::fs::read(target.join("000000010000000000000002")).expect("seg2");
    assert_eq!(seg2, b"BBBB0002");
    let sysid = std::fs::read(target.join("replication/system_id")).expect("sysid");
    assert_eq!(sysid, b"4242\n");

    let _ = std::fs::remove_dir_all(target);
    let _ = std::fs::remove_dir_all(wal_dir);
}

// ===========================================================================
// Realistic local simulation: primary pgwire over real TCP loopback, replica
// driver from `aiondb-replication` runs the actual startup + START_REPLICATION
// + CopyBoth pump. Asserts the receiver's flush_lsn converges on the WAL the
// primary already had on disk.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulate_primary_streams_seeded_wal_to_replica_over_tcp() {
    use aiondb_replication::client::{run_with_metrics, ConnInfo, ReplicaClientConfig};
    use aiondb_replication::ReplicaMetrics;
    use aiondb_wal::replication::WalReceiver;
    use aiondb_wal::{WalConfig, WalLsnMode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let primary_wal_dir = unique_wal_dir("sim_primary");
    let last_lsn = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(101),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(101),
                commit_ts: 11,
            },
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(102),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(102),
                commit_ts: 12,
            },
        ],
    );

    let primary_engine = Arc::new(ReplicationTestEngine::new(
        primary_wal_dir.clone(),
        "1234567890",
    ));
    primary_engine
        .manager
        .state()
        .wal_notifier()
        .notify_new_wal(last_lsn);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let primary_task = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await.expect("accept");
        stream.set_nodelay(true).ok();
        let (reader, writer) = tokio::io::split(stream);
        let mut conn = Connection::new(
            Arc::clone(&primary_engine),
            reader,
            writer,
            1,
            42,
            crate::server::CancelRegistry::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.run()).await;
    });

    let replica_wal_dir = unique_wal_dir("sim_replica");
    std::fs::create_dir_all(replica_wal_dir.join("replication"))
        .expect("create replication metadata dir");
    std::fs::write(
        replica_wal_dir.join("replication").join("system_id"),
        b"1234567890",
    )
    .expect("write system id");
    std::fs::write(replica_wal_dir.join("replication").join("timeline"), b"1")
        .expect("write timeline");

    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: replica_wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open replica receiver"),
    );

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let metrics = ReplicaMetrics::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client_metrics = metrics.clone();
    let client_task = tokio::spawn(async move {
        run_with_metrics(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(80),
                expected_system_identifier: Some("1234567890".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
            client_metrics,
        )
        .await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if receiver.write_lsn() >= last_lsn {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= last_lsn,
        "replica did not catch up to primary's WAL: write_lsn = {}, expected ≥ {}",
        receiver.write_lsn().get(),
        last_lsn.get()
    );
    assert!(
        receiver.flush_lsn() >= last_lsn,
        "replica did not flush primary's WAL: flush_lsn = {}, expected ≥ {}",
        receiver.flush_lsn().get(),
        last_lsn.get()
    );

    let snap = metrics.snapshot();
    assert!(
        snap.sessions_started >= 1,
        "metrics must record at least one session"
    );
    assert!(
        snap.wal_bytes_received > 0,
        "metrics must record at least one WAL byte"
    );

    shutdown_tx.send(true).expect("shutdown");
    let _ = tokio::time::timeout(Duration::from_secs(2), client_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;

    let _ = std::fs::remove_dir_all(primary_wal_dir);
    let _ = std::fs::remove_dir_all(replica_wal_dir);
}

/// Realistic bootstrap simulation: fresh replica fetches the WAL dir via
/// `BASE_BACKUP`, then opens a `WalReceiver` against the materialised dir
/// and streams further changes. Validates that the two protocols cooperate
/// end-to-end against the real pgwire connection state machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulate_base_backup_bootstrap_then_streaming_catchup() {
    use aiondb_replication::client::{
        fetch_base_backup, run as run_client, ConnInfo, ReplicaClientConfig,
    };
    use aiondb_wal::replication::WalReceiver;
    use aiondb_wal::{WalConfig, WalLsnMode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let primary_wal_dir = unique_wal_dir("sim_bootstrap_primary");
    let _ = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(7),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(7),
                commit_ts: 70,
            },
        ],
    );
    // BASE_BACKUP currently streams the WAL dir as-is. The replica must be
    // bootstrapped on a system id matching the primary's IDENTIFY_SYSTEM.
    std::fs::create_dir_all(primary_wal_dir.join("replication"))
        .expect("create replication metadata");
    std::fs::write(
        primary_wal_dir.join("replication").join("system_id"),
        b"sim-base-backup",
    )
    .expect("write system id");
    std::fs::write(primary_wal_dir.join("replication").join("timeline"), b"1")
        .expect("write timeline");

    let primary_engine = Arc::new(ReplicationTestEngine::new(
        primary_wal_dir.clone(),
        "sim-base-backup",
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let primary_engine_for_task = Arc::clone(&primary_engine);
    let primary_task = tokio::spawn(async move {
        // Serve two consecutive replication sessions: first BASE_BACKUP,
        // then START_REPLICATION. The pgwire `Connection` is one-shot
        // per accept, so we drive two accepts in sequence.
        for _ in 0..2 {
            let Ok((stream, _peer)) = listener.accept().await else {
                return;
            };
            stream.set_nodelay(true).ok();
            let (reader, writer) = tokio::io::split(stream);
            let mut conn = Connection::new(
                Arc::clone(&primary_engine_for_task),
                reader,
                writer,
                1,
                42,
                crate::server::CancelRegistry::new(),
            );
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.run()).await;
        }
    });

    let replica_target = unique_wal_dir("sim_bootstrap_replica");
    let _ = std::fs::remove_dir_all(&replica_target);

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    // Step 1: BASE_BACKUP populates the replica's WAL dir.
    let header = tokio::time::timeout(
        Duration::from_secs(5),
        fetch_base_backup(conninfo.clone(), replica_target.clone()),
    )
    .await
    .expect("base backup did not stall")
    .expect("base backup succeeds");
    assert_eq!(header.system_identifier, "sim-base-backup");
    assert!(replica_target
        .join("replication")
        .join("system_id")
        .exists());

    // Step 2: open WalReceiver and stream the same WAL again.
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: replica_target.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open replica receiver"),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let receiver_for_client = Arc::clone(&receiver);
    let client_task = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(80),
                expected_system_identifier: Some("sim-base-backup".to_owned()),
                expected_timeline: Some(1),
            },
            receiver_for_client,
            shutdown_rx,
        )
        .await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if receiver.write_lsn() >= Lsn::new(2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= Lsn::new(2),
        "replica did not ingest WAL after base backup: write_lsn = {}",
        receiver.write_lsn().get()
    );

    shutdown_tx.send(true).expect("shutdown");
    let _ = tokio::time::timeout(Duration::from_secs(2), client_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;

    let _ = std::fs::remove_dir_all(primary_wal_dir);
    let _ = std::fs::remove_dir_all(replica_target);
}

/// Realistic feedback simulation: the replica must report its flush_lsn
/// back to the primary as `StandbyStatusUpdate`. After convergence, the
/// primary's `ReplicaRegistry` snapshot must reflect that progress so
/// future WAL retention decisions on the primary use the right floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulate_replica_status_update_reaches_primary_registry() {
    use aiondb_replication::client::{run_with_metrics, ConnInfo, ReplicaClientConfig};
    use aiondb_replication::ReplicaMetrics;
    use aiondb_wal::replication::WalReceiver;
    use aiondb_wal::{WalConfig, WalLsnMode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let primary_wal_dir = unique_wal_dir("sim_status_primary");
    let last_lsn = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(301),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(301),
                commit_ts: 71,
            },
        ],
    );

    let primary_engine = Arc::new(ReplicationTestEngine::new(
        primary_wal_dir.clone(),
        "7777777777",
    ));
    primary_engine
        .manager
        .state()
        .wal_notifier()
        .notify_new_wal(last_lsn);
    let primary_registry = Arc::clone(primary_engine.manager.state().replica_registry());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let primary_engine_for_task = Arc::clone(&primary_engine);
    let primary_task = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await.expect("accept");
        stream.set_nodelay(true).ok();
        let (reader, writer) = tokio::io::split(stream);
        let mut conn = Connection::new(
            primary_engine_for_task,
            reader,
            writer,
            1,
            42,
            crate::server::CancelRegistry::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.run()).await;
    });

    let replica_wal_dir = unique_wal_dir("sim_status_replica");
    std::fs::create_dir_all(replica_wal_dir.join("replication")).ok();
    std::fs::write(
        replica_wal_dir.join("replication").join("system_id"),
        b"7777777777",
    )
    .ok();
    std::fs::write(replica_wal_dir.join("replication").join("timeline"), b"1").ok();

    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: replica_wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open replica receiver"),
    );

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("conninfo");

    let metrics = ReplicaMetrics::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client_metrics = metrics.clone();
    let client_task = tokio::spawn(async move {
        run_with_metrics(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("7777777777".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
            client_metrics,
        )
        .await;
    });

    // Wait for the primary to (a) register the replica and (b) observe
    // its flush_lsn climb past 0 via incoming StandbyStatusUpdate frames.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut observed_flush_lsn = aiondb_wal::Lsn::ZERO;
    while tokio::time::Instant::now() < deadline {
        let snapshot = primary_registry.snapshot();
        if let Some(state) = snapshot.first() {
            observed_flush_lsn = state.flush_lsn;
            if observed_flush_lsn >= last_lsn {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let final_snapshot = primary_registry.snapshot();
    assert!(
        !final_snapshot.is_empty() || receiver.write_lsn() >= last_lsn,
        "neither primary registry nor replica advanced; receiver write_lsn={}",
        receiver.write_lsn().get()
    );
    // The replica should have shown up in the registry at some point, even
    // if it has already gracefully disconnected by now (see graceful
    // shutdown assertion below). What matters is that *while connected*
    // its progress reached the primary -- captured via observed_flush_lsn
    // sampled in the loop above.
    assert!(
        observed_flush_lsn >= last_lsn,
        "primary's registry must reflect replica flush_lsn (got {}, expected ≥ {}); receiver write_lsn={}, registry now has {} replicas",
        observed_flush_lsn.get(),
        last_lsn.get(),
        receiver.write_lsn().get(),
        final_snapshot.len()
    );
    assert!(
        metrics.snapshot().standby_status_updates_sent >= 1,
        "replica must have sent at least one StandbyStatusUpdate"
    );

    shutdown_tx.send(true).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), client_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;

    // Graceful shutdown should unregister the replica.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if primary_registry.count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        primary_registry.count(),
        0,
        "primary must unregister the replica after graceful shutdown"
    );

    let _ = std::fs::remove_dir_all(primary_wal_dir);
    let _ = std::fs::remove_dir_all(replica_wal_dir);
}

/// Realistic mid-session simulation: primary first streams a small WAL
/// batch, then while the replica is still connected the primary appends
/// more records and signals the notifier. The replica must pick the new
/// records up without reconnecting.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulate_replica_picks_up_wal_appended_mid_session() {
    use aiondb_replication::client::{run as run_client, ConnInfo, ReplicaClientConfig};
    use aiondb_wal::replication::WalReceiver;
    use aiondb_wal::{WalConfig, WalLsnMode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let primary_wal_dir = unique_wal_dir("sim_midsession_primary");
    // Phase 1: seed an initial batch so the replica converges before we
    // start appending more records.
    let phase1_lsn = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(401),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(401),
                commit_ts: 41,
            },
        ],
    );

    let primary_engine = Arc::new(ReplicationTestEngine::new(
        primary_wal_dir.clone(),
        "8888888888",
    ));
    primary_engine
        .manager
        .state()
        .wal_notifier()
        .notify_new_wal(phase1_lsn);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let primary_engine_for_task = Arc::clone(&primary_engine);
    let primary_task = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await.expect("accept");
        stream.set_nodelay(true).ok();
        let (reader, writer) = tokio::io::split(stream);
        let mut conn = Connection::new(
            primary_engine_for_task,
            reader,
            writer,
            1,
            42,
            crate::server::CancelRegistry::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(10), conn.run()).await;
    });

    let replica_wal_dir = unique_wal_dir("sim_midsession_replica");
    std::fs::create_dir_all(replica_wal_dir.join("replication")).ok();
    std::fs::write(
        replica_wal_dir.join("replication").join("system_id"),
        b"8888888888",
    )
    .ok();
    std::fs::write(replica_wal_dir.join("replication").join("timeline"), b"1").ok();

    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: replica_wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open replica receiver"),
    );

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client_task = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(60),
                expected_system_identifier: Some("8888888888".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    // Wait until phase-1 has converged on the replica.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if receiver.write_lsn() >= phase1_lsn {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= phase1_lsn,
        "phase-1 did not converge; receiver write_lsn={}",
        receiver.write_lsn().get()
    );

    // Phase 2: append more records to the primary's WAL dir AFTER the
    // replica is already connected and ingesting. The notifier wake-up
    // must let the sender notice the new bytes on disk.
    let phase2_lsn = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(402),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(402),
                commit_ts: 42,
            },
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(403),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(403),
                commit_ts: 43,
            },
        ],
    );
    primary_engine
        .manager
        .state()
        .wal_notifier()
        .notify_new_wal(phase2_lsn);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if receiver.write_lsn() >= phase2_lsn {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= phase2_lsn,
        "phase-2 (mid-session append) did not converge; receiver write_lsn={}, expected ≥ {}",
        receiver.write_lsn().get(),
        phase2_lsn.get()
    );

    shutdown_tx.send(true).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), client_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;

    let _ = std::fs::remove_dir_all(primary_wal_dir);
    let _ = std::fs::remove_dir_all(replica_wal_dir);
}

/// Realistic slot retention simulation: replica connects via a named slot,
/// streams some WAL, then disconnects gracefully. The slot's
/// `restart_lsn` must still pin retention -- this is what guarantees the
/// primary holds WAL for a (temporarily) offline replica.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulate_slot_pins_retention_across_disconnect() {
    use aiondb_replication::client::{run as run_client, ConnInfo, ReplicaClientConfig};
    use aiondb_wal::replication::WalReceiver;
    use aiondb_wal::{WalConfig, WalLsnMode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::watch;

    let primary_wal_dir = unique_wal_dir("sim_slot_primary");
    let last_lsn = seed_wal_records(
        &primary_wal_dir,
        &[
            WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(501),
                isolation: IsolationLevel::ReadCommitted,
            },
            WalRecord::CommitTxn {
                txn_id: aiondb_core::TxnId::new(501),
                commit_ts: 51,
            },
        ],
    );

    let primary_engine = Arc::new(ReplicationTestEngine::new(
        primary_wal_dir.clone(),
        "5555555555",
    ));
    primary_engine
        .manager
        .state()
        .wal_notifier()
        .notify_new_wal(last_lsn);
    primary_engine
        .manager
        .create_physical_slot("sim_slot_alice", false)
        .expect("create slot");

    let primary_registry = Arc::clone(primary_engine.manager.state().replica_registry());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let primary_engine_for_task = Arc::clone(&primary_engine);
    let primary_task = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await.expect("accept");
        stream.set_nodelay(true).ok();
        let (reader, writer) = tokio::io::split(stream);
        let mut conn = Connection::new(
            primary_engine_for_task,
            reader,
            writer,
            1,
            42,
            crate::server::CancelRegistry::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.run()).await;
    });

    let replica_wal_dir = unique_wal_dir("sim_slot_replica");
    std::fs::create_dir_all(replica_wal_dir.join("replication")).ok();
    std::fs::write(
        replica_wal_dir.join("replication").join("system_id"),
        b"5555555555",
    )
    .ok();
    std::fs::write(replica_wal_dir.join("replication").join("timeline"), b"1").ok();

    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: replica_wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open replica receiver"),
    );

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator replication_slot=sim_slot_alice sslmode=disable",
        addr.port()
    ))
    .expect("conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client_task = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("5555555555".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    // Wait until the replica has flushed and reported its progress.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut observed_slot_lsn = aiondb_wal::Lsn::ZERO;
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(slot)) = primary_engine.manager.read_physical_slot("sim_slot_alice") {
            if let Some(lsn) = slot.restart_lsn {
                if lsn >= last_lsn {
                    observed_slot_lsn = lsn;
                    break;
                }
                observed_slot_lsn = lsn;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        observed_slot_lsn >= last_lsn,
        "slot restart_lsn must advance with replica progress (got {}, expected ≥ {})",
        observed_slot_lsn.get(),
        last_lsn.get()
    );
    assert_eq!(
        primary_registry.count(),
        1,
        "primary must have a single connected replica"
    );

    // Disconnect gracefully and confirm the slot keeps holding the
    // retention floor even though no replica is currently connected.
    shutdown_tx.send(true).ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), client_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), primary_task).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if primary_registry.count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        primary_registry.count(),
        0,
        "replica should be unregistered after graceful shutdown"
    );

    let slot = primary_engine
        .manager
        .read_physical_slot("sim_slot_alice")
        .expect("read slot")
        .expect("slot must still exist");
    assert!(
        slot.restart_lsn.unwrap_or(aiondb_wal::Lsn::ZERO) >= last_lsn,
        "slot restart_lsn must survive disconnect (got {:?}, expected ≥ {})",
        slot.restart_lsn,
        last_lsn.get()
    );
    let min_retention = primary_registry.min_retention_lsn();
    assert!(
        min_retention.is_some(),
        "min_retention_lsn must be pinned by slot even without active replicas"
    );

    let _ = std::fs::remove_dir_all(primary_wal_dir);
    let _ = std::fs::remove_dir_all(replica_wal_dir);
}
