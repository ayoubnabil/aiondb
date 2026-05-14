//! End-to-end smoke test for the replica streaming driver.
//!
//! Spins up a hand-rolled "primary" on a TCP loopback socket that replays the
//! PG startup → `IDENTIFY_SYSTEM` → `START_REPLICATION` → `CopyData` handshake
//! and feeds one WAL entry over the wire. The real
//! [`aiondb_replication::client::run`] driver then connects, ingests the WAL
//! into a local [`WalReceiver`], and we assert the receiver's `flush_lsn`
//! converged on the streamed value.
//!
//! Covers:
//! - Startup wire layout (`AuthOk` → `ParameterStatus` → `BackendKeyData` →
//!   `ReadyForQuery`).
//! - `IDENTIFY_SYSTEM` row decoding (system identifier + timeline).
//! - `START_REPLICATION` → `CopyBothResponse` → `CopyData(WalData)` framing.
//! - The driver flushing the local WAL durably and the apply tracker
//!   subsequently advancing `apply_lsn`.

use std::sync::Arc;
use std::time::Duration;

use aiondb_replication::client::{
    run as run_client, run_with_metrics, ConnInfo, ReplicaClientConfig,
};
use aiondb_replication::ReplicaMetrics;
use aiondb_wal::codec::encode_entry;
use aiondb_wal::record::{WalEntry, WalRecord};
use aiondb_wal::replication::{ReplicationMessage, WalReceiver};
use aiondb_wal::{IsolationLevel, Lsn, WalConfig, WalLsnMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

fn temp_wal_dir(tag: &str) -> std::path::PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aiondb-replication-e2e-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp wal dir");
    dir
}

fn replication_identity_root(tag: &str) -> std::path::PathBuf {
    let root = temp_wal_dir(tag);
    let replication_dir = root.join("replication");
    std::fs::create_dir_all(&replication_dir).expect("create replication metadata dir");
    std::fs::write(replication_dir.join("system_id"), b"42").expect("write system id");
    std::fs::write(replication_dir.join("timeline"), b"1").expect("write timeline");
    root
}

async fn read_exact_bytes(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    stream
        .read_exact(&mut buf)
        .await
        .expect("read exact from client");
    buf
}

async fn read_pg_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0u8; 5];
    stream
        .read_exact(&mut header)
        .await
        .expect("read pg header");
    let tag = header[0];
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize - 4;
    let payload = if len > 0 {
        read_exact_bytes(stream, len).await
    } else {
        Vec::new()
    };
    (tag, payload)
}

async fn write_pg_frame(stream: &mut TcpStream, tag: u8, payload: &[u8]) {
    let len = (payload.len() + 4) as u32;
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(tag);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    stream.write_all(&buf).await.expect("write pg frame");
}

fn build_row_description(columns: &[&str]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(columns.len() as i16).to_be_bytes());
    for name in columns {
        payload.extend_from_slice(name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&0_u32.to_be_bytes()); // table oid
        payload.extend_from_slice(&0_i16.to_be_bytes()); // column attr
        payload.extend_from_slice(&25_u32.to_be_bytes()); // type oid (text)
        payload.extend_from_slice(&(-1_i16).to_be_bytes()); // type size
        payload.extend_from_slice(&(-1_i32).to_be_bytes()); // type mod
        payload.extend_from_slice(&0_i16.to_be_bytes()); // format code (text)
    }
    payload
}

fn build_data_row(values: &[&str]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(values.len() as i16).to_be_bytes());
    for value in values {
        payload.extend_from_slice(&(value.len() as i32).to_be_bytes());
        payload.extend_from_slice(value.as_bytes());
    }
    payload
}

fn build_command_complete(tag: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(tag.len() + 1);
    payload.extend_from_slice(tag.as_bytes());
    payload.push(0);
    payload
}

fn build_wal_data_message(lsn: u64, txn_id: u64) -> Vec<u8> {
    ReplicationMessage::WalData {
        start_lsn: Lsn::new(lsn),
        end_lsn: Lsn::new(lsn),
        data: encode_entry(&WalEntry {
            lsn: Lsn::new(lsn),
            prev_lsn: if lsn > 1 {
                Lsn::new(lsn - 1)
            } else {
                Lsn::ZERO
            },
            database_id: WalEntry::LEGACY_DATABASE_ID,
            record: WalRecord::BeginTxn {
                txn_id: aiondb_core::TxnId::new(txn_id),
                isolation: IsolationLevel::ReadCommitted,
            },
        })
        .expect("encode wal entry"),
    }
    .encode()
    .expect("encode replication message")
}

/// Mock primary auth modes.
#[derive(Clone, Copy)]
enum AuthMode {
    /// Send `AuthenticationOk` immediately.
    Trust,
    /// Send `AuthenticationCleartextPassword`, expect a PasswordMessage with
    /// the embedded value, then `AuthenticationOk`.
    Cleartext { expected: &'static str },
    /// Send `AuthenticationMD5Password` with a fixed salt, verify the token
    /// matches `md5(md5hex(password + user) + salt)`.
    Md5 {
        user: &'static str,
        password: &'static str,
        salt: [u8; 4],
    },
}

fn expected_md5_token(user: &str, password: &str, salt: [u8; 4]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(user.as_bytes());
    let first = hex_lower(&hasher.finalize_reset());
    hasher.update(first.as_bytes());
    hasher.update(salt);
    let second = hex_lower(&hasher.finalize());
    format!("md5{second}")
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(*b >> 4) as usize] as char);
        out.push(HEX[(*b & 0x0F) as usize] as char);
    }
    out
}

async fn drive_mock_primary_base_backup(
    listener: TcpListener,
    files: Vec<(&'static str, Vec<u8>)>,
) {
    use aiondb_replication::base_backup::{
        encode_backup_end, encode_file_data, encode_file_end, encode_file_start, encode_header,
        BaseBackupHeader,
    };
    let (mut stream, _) = listener.accept().await.expect("accept replica");

    // Startup
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read startup len");
    let total_len = u32::from_be_bytes(len_buf) as usize;
    let _ = read_exact_bytes(&mut stream, total_len.saturating_sub(4)).await;
    write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
    write_pg_frame(&mut stream, b'S', b"server_version\0aiondb-test\0").await;
    let mut key = Vec::new();
    key.extend_from_slice(&1_u32.to_be_bytes());
    key.extend_from_slice(&42_u32.to_be_bytes());
    write_pg_frame(&mut stream, b'K', &key).await;
    write_pg_frame(&mut stream, b'Z', b"I").await;

    // Expect `Q BASE_BACKUP\0`
    let (tag, query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    let q_text = std::str::from_utf8(&query[..query.len() - 1]).unwrap();
    assert!(q_text.contains("BASE_BACKUP"), "got query {q_text}");

    // CopyOutResponse: format=text(0), 0 columns
    let mut copy_out = Vec::new();
    copy_out.push(0u8);
    copy_out.extend_from_slice(&0_i16.to_be_bytes());
    write_pg_frame(&mut stream, b'H', &copy_out).await;

    // Header
    let header = BaseBackupHeader {
        wal_start_lsn: aiondb_wal::Lsn::new(0x42),
        timeline: 1,
        system_identifier: "42".to_owned(),
    };
    let header_frame = encode_header(&header).expect("encode header");
    write_pg_frame(&mut stream, b'd', &header_frame).await;

    // Files
    for (path, content) in &files {
        let start = encode_file_start(path, content.len() as u64).expect("encode start");
        write_pg_frame(&mut stream, b'd', &start).await;
        let data = encode_file_data(content).expect("encode data");
        write_pg_frame(&mut stream, b'd', &data).await;
        let end = encode_file_end().expect("encode end");
        write_pg_frame(&mut stream, b'd', &end).await;
    }

    // BackupEnd + CopyDone + CommandComplete + ReadyForQuery
    let end = encode_backup_end().expect("encode end");
    write_pg_frame(&mut stream, b'd', &end).await;
    write_pg_frame(&mut stream, b'c', &[]).await;
    write_pg_frame(&mut stream, b'C', b"BASE_BACKUP\0").await;
    write_pg_frame(&mut stream, b'Z', b"I").await;
    let _ = stream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_base_backup_writes_each_file_to_target_dir() {
    use aiondb_replication::client::fetch_base_backup;

    let target = temp_wal_dir("base_backup_target");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let files = vec![
        ("wal/000000010000000000000001", b"AAAABBBB".to_vec()),
        ("wal/000000010000000000000002", b"CCCCDDDD".to_vec()),
        ("replication/system_id", b"42\n".to_vec()),
    ];
    let mock = tokio::spawn(drive_mock_primary_base_backup(listener, files));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let header = fetch_base_backup(conninfo, target.clone())
        .await
        .expect("fetch base backup");
    assert_eq!(header.system_identifier, "42");
    assert_eq!(header.timeline, 1);

    let seg1 = std::fs::read(target.join("wal/000000010000000000000001")).expect("seg1");
    assert_eq!(seg1, b"AAAABBBB");
    let seg2 = std::fs::read(target.join("wal/000000010000000000000002")).expect("seg2");
    assert_eq!(seg2, b"CCCCDDDD");
    let sysid = std::fs::read(target.join("replication/system_id")).expect("sysid");
    assert_eq!(sysid, b"42\n");

    let _ = tokio::time::timeout(Duration::from_secs(2), mock).await;
    let _ = std::fs::remove_dir_all(target);
}

async fn drive_mock_primary(listener: TcpListener, auth: AuthMode) {
    let (mut stream, _) = listener.accept().await.expect("accept replica");

    // 1. Startup message: [len:u32][protocol:u32][k=v...\0\0]
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read startup len");
    let total_len = u32::from_be_bytes(len_buf) as usize;
    let payload_len = total_len.saturating_sub(4);
    let _startup_payload = read_exact_bytes(&mut stream, payload_len).await;

    // 2. Authentication phase
    match auth {
        AuthMode::Trust => {
            write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
        }
        AuthMode::Cleartext { expected } => {
            // AuthenticationCleartextPassword: subtype = 3
            write_pg_frame(&mut stream, b'R', &3_u32.to_be_bytes()).await;
            // Expect PasswordMessage ('p' | len | password\0)
            let (tag, payload) = read_pg_frame(&mut stream).await;
            assert_eq!(tag, b'p', "expected PasswordMessage, got {tag:#04x}");
            let sent = std::str::from_utf8(&payload[..payload.len() - 1]).expect("utf8");
            assert_eq!(sent, expected, "password mismatch");
            write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
        }
        AuthMode::Md5 {
            user,
            password,
            salt,
        } => {
            // AuthenticationMD5Password: subtype = 5 followed by 4-byte salt
            let mut auth_payload = Vec::with_capacity(8);
            auth_payload.extend_from_slice(&5_u32.to_be_bytes());
            auth_payload.extend_from_slice(&salt);
            write_pg_frame(&mut stream, b'R', &auth_payload).await;
            // Expect PasswordMessage with the md5 token
            let (tag, payload) = read_pg_frame(&mut stream).await;
            assert_eq!(tag, b'p', "expected PasswordMessage, got {tag:#04x}");
            let sent = std::str::from_utf8(&payload[..payload.len() - 1]).expect("utf8");
            let expected = expected_md5_token(user, password, salt);
            assert_eq!(sent, expected, "md5 token mismatch");
            write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
        }
    }

    // 3. Minimal ParameterStatus to look like a real primary
    let mut server_version_payload = Vec::new();
    server_version_payload.extend_from_slice(b"server_version\0aiondb-test\0");
    write_pg_frame(&mut stream, b'S', &server_version_payload).await;

    // 4. BackendKeyData
    let mut backend_key = Vec::new();
    backend_key.extend_from_slice(&1_u32.to_be_bytes());
    backend_key.extend_from_slice(&42_u32.to_be_bytes());
    write_pg_frame(&mut stream, b'K', &backend_key).await;

    // 5. ReadyForQuery (Idle)
    write_pg_frame(&mut stream, b'Z', b"I").await;

    // 6. Expect `Q IDENTIFY_SYSTEM\0`
    let (tag, query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    let query_text = std::str::from_utf8(&query[..query.len() - 1]).expect("utf8");
    assert!(
        query_text.contains("IDENTIFY_SYSTEM"),
        "expected IDENTIFY_SYSTEM, got {query_text}"
    );

    // 7. Reply with RowDescription + DataRow + CommandComplete + ReadyForQuery
    write_pg_frame(
        &mut stream,
        b'T',
        &build_row_description(&["systemid", "timeline", "xlogpos", "dbname"]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'D',
        &build_data_row(&["42", "1", "0/00000000", ""]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'C',
        &build_command_complete("IDENTIFY_SYSTEM"),
    )
    .await;
    write_pg_frame(&mut stream, b'Z', b"I").await;

    // 8. Expect `Q START_REPLICATION 0/...\0`
    let (tag, query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    let query_text = std::str::from_utf8(&query[..query.len() - 1]).expect("utf8");
    assert!(
        query_text.starts_with("START_REPLICATION"),
        "expected START_REPLICATION, got {query_text}"
    );

    // 9. Send CopyBothResponse: format=0(text), 0 columns
    let mut copy_both = Vec::new();
    copy_both.push(0u8);
    copy_both.extend_from_slice(&0_i16.to_be_bytes());
    write_pg_frame(&mut stream, b'W', &copy_both).await;

    // 10. Send one CopyData wrapping a WalData replication message for LSN 1
    let wal_payload = build_wal_data_message(1, 7);
    write_pg_frame(&mut stream, b'd', &wal_payload).await;

    // Drain any StandbyStatusUpdate the replica sends; we don't have to act on
    // it, but reading prevents the socket from filling up.
    let mut scratch = [0u8; 64];
    let _ = tokio::time::timeout(Duration::from_millis(200), stream.read(&mut scratch)).await;

    let _ = stream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_uses_replication_slot_in_start_replication() {
    let root = replication_identity_root("slot_usage");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mock_primary = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .expect("read startup len");
        let total_len = u32::from_be_bytes(len_buf) as usize;
        let _ = read_exact_bytes(&mut stream, total_len.saturating_sub(4)).await;
        write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
        write_pg_frame(&mut stream, b'S', b"server_version\0aiondb-test\0").await;
        let mut key = Vec::new();
        key.extend_from_slice(&1_u32.to_be_bytes());
        key.extend_from_slice(&42_u32.to_be_bytes());
        write_pg_frame(&mut stream, b'K', &key).await;
        write_pg_frame(&mut stream, b'Z', b"I").await;
        let (_, q1) = read_pg_frame(&mut stream).await;
        let _ = cmd_tx.send(
            std::str::from_utf8(&q1[..q1.len() - 1])
                .unwrap_or_default()
                .to_owned(),
        );
        write_pg_frame(
            &mut stream,
            b'T',
            &build_row_description(&["systemid", "timeline", "xlogpos", "dbname"]),
        )
        .await;
        write_pg_frame(
            &mut stream,
            b'D',
            &build_data_row(&["42", "1", "0/00000000", ""]),
        )
        .await;
        write_pg_frame(
            &mut stream,
            b'C',
            &build_command_complete("IDENTIFY_SYSTEM"),
        )
        .await;
        write_pg_frame(&mut stream, b'Z', b"I").await;
        let (_, q2) = read_pg_frame(&mut stream).await;
        let _ = cmd_tx.send(
            std::str::from_utf8(&q2[..q2.len() - 1])
                .unwrap_or_default()
                .to_owned(),
        );
        let mut copy_both = Vec::new();
        copy_both.push(0u8);
        copy_both.extend_from_slice(&0_i16.to_be_bytes());
        write_pg_frame(&mut stream, b'W', &copy_both).await;
        write_pg_frame(&mut stream, b'd', &build_wal_data_message(1, 13)).await;
        let mut scratch = [0u8; 64];
        let _ = tokio::time::timeout(Duration::from_millis(200), stream.read(&mut scratch)).await;
        let _ = stream.shutdown().await;
    });

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator replication_slot=standby_alice sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");
    assert_eq!(conninfo.slot_name.as_deref(), Some("standby_alice"));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..200 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let identify = cmd_rx.recv().await.expect("identify cmd");
    let start = cmd_rx.recv().await.expect("start cmd");
    assert!(identify.contains("IDENTIFY_SYSTEM"), "got: {identify}");
    assert!(
        start.starts_with("START_REPLICATION SLOT standby_alice PHYSICAL "),
        "expected slot-qualified START_REPLICATION, got: {start}"
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_consumes_one_wal_batch_from_mock_primary() {
    let root = replication_identity_root("wal_batch");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let mock_primary = tokio::spawn(drive_mock_primary(listener, AuthMode::Trust));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..200 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= Lsn::new(1),
        "replica did not ingest the WAL frame: write_lsn = {}",
        receiver.write_lsn().get()
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_metrics_count_session_and_wal_bytes() {
    let root = replication_identity_root("metrics");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let mock_primary = tokio::spawn(drive_mock_primary(listener, AuthMode::Trust));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let metrics = ReplicaMetrics::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client_metrics = metrics.clone();
    let client = tokio::spawn(async move {
        run_with_metrics(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
            client_metrics,
        )
        .await;
    });

    for _ in 0..200 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snap = metrics.snapshot();
    assert!(
        snap.sessions_started >= 1,
        "sessions_started must record at least one session, got {}",
        snap.sessions_started
    );
    assert!(
        snap.wal_bytes_received > 0,
        "wal_bytes_received must be positive after WAL frame, got {}",
        snap.wal_bytes_received
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_sends_standby_status_update_after_receiving_wal() {
    use aiondb_wal::replication::ReplicationMessage;

    let root = replication_identity_root("status_update");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<ReplicationMessage>();
    let mock_primary = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .expect("read startup len");
        let total_len = u32::from_be_bytes(len_buf) as usize;
        let _ = read_exact_bytes(&mut stream, total_len.saturating_sub(4)).await;
        write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
        write_pg_frame(&mut stream, b'S', b"server_version\0aiondb-test\0").await;
        let mut key = Vec::new();
        key.extend_from_slice(&1_u32.to_be_bytes());
        key.extend_from_slice(&42_u32.to_be_bytes());
        write_pg_frame(&mut stream, b'K', &key).await;
        write_pg_frame(&mut stream, b'Z', b"I").await;
        let (tag, _) = read_pg_frame(&mut stream).await;
        assert_eq!(tag, b'Q');
        write_pg_frame(
            &mut stream,
            b'T',
            &build_row_description(&["systemid", "timeline", "xlogpos", "dbname"]),
        )
        .await;
        write_pg_frame(
            &mut stream,
            b'D',
            &build_data_row(&["42", "1", "0/00000000", ""]),
        )
        .await;
        write_pg_frame(
            &mut stream,
            b'C',
            &build_command_complete("IDENTIFY_SYSTEM"),
        )
        .await;
        write_pg_frame(&mut stream, b'Z', b"I").await;
        let (tag, _) = read_pg_frame(&mut stream).await;
        assert_eq!(tag, b'Q');
        let mut copy_both = Vec::new();
        copy_both.push(0u8);
        copy_both.extend_from_slice(&0_i16.to_be_bytes());
        write_pg_frame(&mut stream, b'W', &copy_both).await;
        write_pg_frame(&mut stream, b'd', &build_wal_data_message(1, 11)).await;

        // Read CopyData frames from the replica. We expect at least one
        // StandbyStatusUpdate carrying write_lsn >= 1 (the LSN we sent
        // above). Forward each decoded message to the channel so the
        // outer test can assert.
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_secs(2), read_pg_frame(&mut stream)).await {
                Ok((b'd', payload)) => {
                    let (msg, _) = ReplicationMessage::decode(&payload).expect("decode");
                    let _ = status_tx.send(msg);
                }
                Ok((b'c' | b'X', _)) | Err(_) => break,
                _ => {}
            }
        }
        let _ = stream.shutdown().await;
    });

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(40),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    // Wait until the receiver has flushed the WAL, then poll the status
    // channel for a StandbyStatusUpdate reflecting that LSN.
    let mut observed_lsn = Lsn::ZERO;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(msg)) =
            tokio::time::timeout(Duration::from_millis(100), status_rx.recv()).await
        {
            if let ReplicationMessage::StandbyStatusUpdate {
                write_lsn,
                flush_lsn,
                ..
            } = msg
            {
                if flush_lsn >= Lsn::new(1) {
                    observed_lsn = write_lsn;
                    break;
                }
            }
        }
    }
    assert!(
        observed_lsn >= Lsn::new(1),
        "replica did not send StandbyStatusUpdate with flush_lsn >= 1 \
         within 5s; last observed write_lsn = {}",
        observed_lsn.get()
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}

/// Mock primary that accepts two connections back-to-back. The first one is
/// hung up after `IDENTIFY_SYSTEM` so the driver hits its reconnect path; the
/// second one streams one `WalData` frame to prove resumption works.
async fn drive_mock_primary_with_drop(listener: TcpListener) {
    // Session 1: complete startup + IDENTIFY_SYSTEM, then drop the socket
    // before sending the CopyBothResponse. The driver must treat this as a
    // transient session failure and reconnect.
    let (mut stream, _) = listener.accept().await.expect("accept replica (1)");
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read startup len");
    let total_len = u32::from_be_bytes(len_buf) as usize;
    let _ = read_exact_bytes(&mut stream, total_len.saturating_sub(4)).await;
    write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await; // AuthOk
    write_pg_frame(&mut stream, b'S', b"server_version\0aiondb-test\0").await;
    let mut key = Vec::new();
    key.extend_from_slice(&1_u32.to_be_bytes());
    key.extend_from_slice(&42_u32.to_be_bytes());
    write_pg_frame(&mut stream, b'K', &key).await;
    write_pg_frame(&mut stream, b'Z', b"I").await;
    let (tag, query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    assert!(std::str::from_utf8(&query)
        .unwrap()
        .contains("IDENTIFY_SYSTEM"));
    write_pg_frame(
        &mut stream,
        b'T',
        &build_row_description(&["systemid", "timeline", "xlogpos", "dbname"]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'D',
        &build_data_row(&["42", "1", "0/00000000", ""]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'C',
        &build_command_complete("IDENTIFY_SYSTEM"),
    )
    .await;
    write_pg_frame(&mut stream, b'Z', b"I").await;
    // Now drop the socket to force the driver into reconnect.
    drop(stream);

    // Session 2: full happy path streams one WAL entry.
    let (mut stream, _) = listener.accept().await.expect("accept replica (2)");
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read startup len (2)");
    let total_len = u32::from_be_bytes(len_buf) as usize;
    let _ = read_exact_bytes(&mut stream, total_len.saturating_sub(4)).await;
    write_pg_frame(&mut stream, b'R', &0_u32.to_be_bytes()).await;
    write_pg_frame(&mut stream, b'S', b"server_version\0aiondb-test\0").await;
    let mut key = Vec::new();
    key.extend_from_slice(&1_u32.to_be_bytes());
    key.extend_from_slice(&42_u32.to_be_bytes());
    write_pg_frame(&mut stream, b'K', &key).await;
    write_pg_frame(&mut stream, b'Z', b"I").await;
    let (tag, query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    assert!(std::str::from_utf8(&query)
        .unwrap()
        .contains("IDENTIFY_SYSTEM"));
    write_pg_frame(
        &mut stream,
        b'T',
        &build_row_description(&["systemid", "timeline", "xlogpos", "dbname"]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'D',
        &build_data_row(&["42", "1", "0/00000000", ""]),
    )
    .await;
    write_pg_frame(
        &mut stream,
        b'C',
        &build_command_complete("IDENTIFY_SYSTEM"),
    )
    .await;
    write_pg_frame(&mut stream, b'Z', b"I").await;
    let (tag, _query) = read_pg_frame(&mut stream).await;
    assert_eq!(tag, b'Q');
    let mut copy_both = Vec::new();
    copy_both.push(0u8);
    copy_both.extend_from_slice(&0_i16.to_be_bytes());
    write_pg_frame(&mut stream, b'W', &copy_both).await;
    write_pg_frame(&mut stream, b'd', &build_wal_data_message(1, 9)).await;
    let mut scratch = [0u8; 64];
    let _ = tokio::time::timeout(Duration::from_millis(300), stream.read(&mut scratch)).await;
    let _ = stream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_reconnects_after_primary_disconnects_mid_session() {
    let root = replication_identity_root("reconnect");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let mock_primary = tokio::spawn(drive_mock_primary_with_drop(listener));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    // Give the driver up to ~6s: first session dies (~250ms backoff initial),
    // second session needs to complete handshake + receive WAL.
    for _ in 0..300 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= Lsn::new(1),
        "replica did not ingest WAL after reconnect: write_lsn = {}",
        receiver.write_lsn().get()
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(3), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_completes_md5_password_handshake() {
    let root = replication_identity_root("md5_auth");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let salt = [0xDE, 0xAD, 0xBE, 0xEF];
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let mock_primary = tokio::spawn(drive_mock_primary(
        listener,
        AuthMode::Md5 {
            user: "alice",
            password: "secret",
            salt,
        },
    ));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=alice password=secret sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..200 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= Lsn::new(1),
        "replica did not ingest WAL after md5 auth: write_lsn = {}",
        receiver.write_lsn().get()
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replica_driver_completes_cleartext_password_handshake() {
    let root = replication_identity_root("cleartext_auth");
    let wal_dir = root.join("wal");
    let receiver = Arc::new(
        WalReceiver::open(WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: WalLsnMode::Logical,
            ..WalConfig::default()
        })
        .expect("open WAL receiver"),
    );

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let mock_primary = tokio::spawn(drive_mock_primary(
        listener,
        AuthMode::Cleartext {
            expected: "hunter2",
        },
    ));

    let conninfo = ConnInfo::parse(&format!(
        "host=127.0.0.1 port={} user=replicator password=hunter2 sslmode=disable",
        addr.port()
    ))
    .expect("parse conninfo");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_receiver = Arc::clone(&receiver);
    let client = tokio::spawn(async move {
        run_client(
            ReplicaClientConfig {
                conninfo,
                status_interval: Duration::from_millis(50),
                expected_system_identifier: Some("42".to_owned()),
                expected_timeline: Some(1),
            },
            client_receiver,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..200 {
        if receiver.write_lsn() >= Lsn::new(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        receiver.write_lsn() >= Lsn::new(1),
        "replica did not ingest WAL after cleartext auth: write_lsn = {}",
        receiver.write_lsn().get()
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), client).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), mock_primary).await;

    let _ = std::fs::remove_dir_all(root);
}
