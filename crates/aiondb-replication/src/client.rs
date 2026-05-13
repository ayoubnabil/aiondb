//! Replica-side streaming replication driver.
//!
//! Opens a TCP connection to a primary `AionDB` server, runs the PG startup
//! handshake with `replication=true`, issues `IDENTIFY_SYSTEM` to confirm the
//! upstream identity, then `START_REPLICATION` to enter `CopyBoth` mode and
//! forward incoming WAL frames to a local [`WalReceiver`]. Periodic
//! `StandbyStatusUpdate` messages are sent back to the primary so it can
//! release WAL segments the replica has flushed.
//!
//! The driver reconnects on transient errors with exponential backoff capped
//! at a configurable maximum.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use aiondb_wal::replication::{ReplicationMessage, WalReceiver};
use aiondb_wal::Lsn;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn};

use crate::metrics::ReplicaMetrics;
use crate::tls::maybe_upgrade_to_tls;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const BACKOFF_INITIAL: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// PostgreSQL v3 startup protocol version (`0x0003_0000`).
const STARTUP_PROTOCOL_VERSION: u32 = 196_608;
/// Hard cap on backend frame size (mirrors `aiondb-pgwire`'s codec limit).
const MAX_BACKEND_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Hard cap on frontend frames emitted by this replica driver.
const MAX_FRONTEND_MESSAGE_BYTES: usize = MAX_BACKEND_MESSAGE_BYTES;
/// `IDENTIFY_SYSTEM` only needs a handful of text fields. Keep a generous cap
/// so a malicious upstream cannot force unbounded per-row allocations.
const MAX_BACKEND_DATA_ROW_COLUMNS: usize = 1024;
const PG_FRAME_LEN_BYTES: usize = 4;
const PG_FRAME_TAG_BYTES: usize = 1;

/// TLS posture requested by the operator. Mirrors libpq's `sslmode`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SslMode {
    Disable,
    Allow,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    /// Returns true when the driver should attempt the PG SSL preamble
    /// (`SSLRequest`) on every connect, with or without certificate
    /// validation.
    pub fn attempts_tls(self) -> bool {
        !matches!(self, SslMode::Disable)
    }

    /// Returns true when the operator demands a successful TLS handshake.
    /// `disable` / `allow` / `prefer` are happy with plaintext fallback.
    pub fn requires_tls(self) -> bool {
        matches!(
            self,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
        )
    }

    /// Returns true when server certificate identity must be validated
    /// against `ssl_root_cert`. `Require` accepts any server certificate;
    /// `VerifyCa` and `VerifyFull` enforce the trust chain.
    pub fn verifies_certificate(self) -> bool {
        matches!(self, SslMode::VerifyCa | SslMode::VerifyFull)
    }
}

/// Parsed connection info string in libpq keyword=value form.
#[derive(Clone, Eq, PartialEq)]
pub struct ConnInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: Option<String>,
    pub application_name: Option<String>,
    /// Name of a pre-created physical replication slot on the primary. When
    /// set the driver issues `START_REPLICATION SLOT <slot_name> PHYSICAL`,
    /// which keeps the primary's WAL retention pinned to this replica's
    /// `restart_lsn` even across disconnects.
    pub slot_name: Option<String>,
    /// Transport-level posture for the connection (see [`SslMode`]).
    pub sslmode: SslMode,
    /// PEM-encoded root certificate bundle path; required for
    /// `sslmode=verify-ca` and `sslmode=verify-full`.
    pub ssl_root_cert: Option<String>,
}

impl fmt::Debug for ConnInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnInfo")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("database", &self.database)
            .field("application_name", &self.application_name)
            .field("slot_name", &self.slot_name)
            .field("sslmode", &self.sslmode)
            .field("ssl_root_cert", &self.ssl_root_cert)
            .finish()
    }
}

impl ConnInfo {
    /// Parse a libpq-style connection string. Recognized keys: `host`,
    /// `port`, `user`, `password`, `dbname`, `application_name`.
    pub fn parse(conninfo: &str) -> DbResult<Self> {
        let mut host = None;
        let mut port = None;
        let mut user = None;
        let mut password = None;
        let mut database = None;
        let mut application_name = None;
        let mut slot_name = None;
        let mut sslmode = SslMode::default();
        let mut ssl_root_cert = None;
        for (key, value) in parse_conninfo_tokens(conninfo)? {
            match key.as_str() {
                "host" => {
                    validate_conninfo_value(&key, &value)?;
                    host = Some(value);
                }
                "port" => {
                    validate_conninfo_value(&key, &value)?;
                    port = Some(value.parse::<u16>().map_err(|err| {
                        DbError::invalid_input_syntax(
                            "primary_conninfo",
                            &format!("invalid port \"{value}\": {err}"),
                        )
                    })?);
                }
                "user" => {
                    validate_conninfo_value(&key, &value)?;
                    user = Some(value);
                }
                "password" => {
                    validate_conninfo_value(&key, &value)?;
                    password = Some(value);
                }
                "dbname" => {
                    validate_conninfo_value(&key, &value)?;
                    database = Some(value);
                }
                "application_name" => {
                    validate_conninfo_value(&key, &value)?;
                    application_name = Some(value);
                }
                "replication_slot" | "slot_name" => {
                    validate_conninfo_value(&key, &value)?;
                    validate_slot_name(&value)?;
                    slot_name = Some(value);
                }
                "sslmode" => {
                    validate_conninfo_value(&key, &value)?;
                    sslmode = parse_sslmode(&value)?;
                }
                "sslrootcert" | "ssl_root_cert" => {
                    validate_conninfo_value(&key, &value)?;
                    ssl_root_cert = Some(value);
                }
                _ => {
                    return Err(DbError::invalid_input_syntax(
                        "primary_conninfo",
                        &format!("unknown keyword \"{key}\""),
                    ));
                }
            }
        }
        if sslmode.verifies_certificate() && ssl_root_cert.is_none() {
            return Err(DbError::invalid_input_syntax(
                "primary_conninfo",
                "sslmode=verify-ca/verify-full requires sslrootcert= to be set",
            ));
        }
        Ok(Self {
            host: host.unwrap_or_else(|| "127.0.0.1".to_owned()),
            port: port.unwrap_or(5432),
            user: user.unwrap_or_else(|| "replicator".to_owned()),
            password,
            database,
            application_name,
            slot_name,
            sslmode,
            ssl_root_cert,
        })
    }

    fn socket_addr(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn parse_conninfo_tokens(conninfo: &str) -> DbResult<Vec<(String, String)>> {
    let mut chars = conninfo.chars().peekable();
    let mut tokens = Vec::new();

    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Ok(tokens);
        }

        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if key.is_empty() {
            return Err(DbError::invalid_input_syntax(
                "primary_conninfo",
                "expected keyword before '='",
            ));
        }

        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some('=') {
            return Err(DbError::invalid_input_syntax(
                "primary_conninfo",
                &format!("token for keyword \"{key}\" is not key=value"),
            ));
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}

        let value = if chars.next_if_eq(&'\'').is_some() {
            let mut value = String::new();
            loop {
                match chars.next() {
                    Some('\'') => break value,
                    Some('\\') => match chars.next() {
                        Some(escaped) => value.push(escaped),
                        None => {
                            return Err(DbError::invalid_input_syntax(
                                "primary_conninfo",
                                &format!(
                                    "unterminated backslash escape in quoted value for \"{key}\""
                                ),
                            ));
                        }
                    },
                    Some(c) => value.push(c),
                    None => {
                        return Err(DbError::invalid_input_syntax(
                            "primary_conninfo",
                            &format!("unterminated quoted value for \"{key}\""),
                        ));
                    }
                }
            }
        } else {
            let mut value = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
            value
        };
        tokens.push((key, value));
    }
}

fn validate_conninfo_value(keyword: &str, value: &str) -> DbResult<()> {
    if value.contains('\0') {
        return Err(DbError::invalid_input_syntax(
            "primary_conninfo",
            &format!("keyword \"{keyword}\" contains NUL byte"),
        ));
    }
    Ok(())
}

fn validate_slot_name(name: &str) -> DbResult<()> {
    if name.is_empty() || name.len() > 63 {
        return Err(DbError::invalid_input_syntax(
            "primary_conninfo",
            "replication_slot must be 1..=63 bytes",
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(DbError::invalid_input_syntax(
            "primary_conninfo",
            "replication_slot must contain only [a-z0-9_]",
        ));
    }
    Ok(())
}

fn parse_sslmode(value: &str) -> DbResult<SslMode> {
    match value {
        "disable" => Ok(SslMode::Disable),
        "allow" => Ok(SslMode::Allow),
        "prefer" => Ok(SslMode::Prefer),
        "require" => Ok(SslMode::Require),
        "verify-ca" => Ok(SslMode::VerifyCa),
        "verify-full" => Ok(SslMode::VerifyFull),
        other => Err(DbError::invalid_input_syntax(
            "primary_conninfo",
            &format!(
                "invalid sslmode \"{other}\" (expected one of: disable, allow, prefer, require, verify-ca, verify-full)"
            ),
        )),
    }
}

/// Static configuration for the replica streaming driver.
#[derive(Clone, Debug)]
pub struct ReplicaClientConfig {
    pub conninfo: ConnInfo,
    pub status_interval: Duration,
    pub expected_system_identifier: Option<String>,
    pub expected_timeline: Option<u32>,
}

/// Run the replica streaming loop until shutdown is signalled.
///
/// Reconnects with exponential backoff on any error short of explicit
/// shutdown. The receiver's local WAL is the single source of truth for
/// resume LSN: each reconnect picks up at `receiver.flush_lsn()`.
pub async fn run(
    config: ReplicaClientConfig,
    receiver: Arc<WalReceiver>,
    shutdown_rx: watch::Receiver<bool>,
) {
    run_with_metrics(config, receiver, shutdown_rx, ReplicaMetrics::new()).await;
}

/// Connect to `primary_conninfo`, run the PG startup handshake (auth +
/// SSL), issue `BASE_BACKUP`, and materialise every streamed file under
/// `target_dir`. Returns the `BaseBackupHeader` announced by the primary so
/// the caller can seed its replica metadata (`system_id`, `timeline`).
///
/// Unlike [`run`], this function performs a single one-shot exchange and
/// closes the connection -- it is meant to bootstrap a fresh replica before
/// `run` takes over the streaming session.
pub async fn fetch_base_backup(
    conninfo: ConnInfo,
    target_dir: std::path::PathBuf,
) -> DbResult<crate::base_backup::BaseBackupHeader> {
    let addr = conninfo.socket_addr();
    debug!(addr = %addr, target = %target_dir.display(), "fetching BASE_BACKUP");

    let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| DbError::internal(format!("primary connect timed out: {addr}")))
        .and_then(|res| {
            res.map_err(|err| DbError::internal(format!("primary connect failed ({addr}): {err}")))
        })?;
    stream
        .set_nodelay(true)
        .map_err(|err| DbError::internal(format!("set_nodelay failed: {err}")))?;
    let stream = crate::tls::maybe_upgrade_to_tls(stream, &conninfo).await?;
    let (mut reader, mut writer) = tokio::io::split(stream);

    send_startup(&mut writer, &conninfo).await?;
    handshake_until_ready(&mut reader, &mut writer, &conninfo).await?;

    send_simple_query(&mut writer, "BASE_BACKUP").await?;
    // Expect CopyOutResponse 'H' before CopyData frames.
    loop {
        let raw = read_backend_message(&mut reader).await?;
        match raw.tag {
            b'H' => break,
            b'N' => {}
            b'E' => {
                return Err(DbError::internal(format!(
                    "BASE_BACKUP rejected by primary: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected backend tag {other:#04x} before BASE_BACKUP CopyOutResponse"
                )));
            }
        }
    }

    let mut writer_state = crate::base_backup::BaseBackupWriter::new(target_dir)?;
    let mut header_observed: Option<crate::base_backup::BaseBackupHeader> = None;
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let raw = read_backend_message(&mut reader).await?;
        match raw.tag {
            b'd' => {
                buffer.extend_from_slice(&raw.payload);
                loop {
                    if buffer.is_empty() {
                        break;
                    }
                    match crate::base_backup::decode_frame(&buffer) {
                        Ok((frame, consumed)) => {
                            if let crate::base_backup::BackupFrame::Header(ref hdr) = frame {
                                header_observed = Some(hdr.clone());
                            }
                            let terminal = writer_state.apply(frame)?;
                            buffer.drain(..consumed);
                            if terminal {
                                break;
                            }
                        }
                        Err(err) => {
                            // Frame truncated -- need more bytes; bail to
                            // outer loop which reads another CopyData.
                            if err.to_string().contains("truncated") {
                                break;
                            }
                            return Err(err);
                        }
                    }
                }
            }
            b'c' => break,
            b'C' => {}
            b'Z' => break,
            b'E' => {
                return Err(DbError::internal(format!(
                    "BASE_BACKUP stream errored mid-flight: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected backend tag {other:#04x} during BASE_BACKUP stream"
                )));
            }
        }
    }

    if !writer_state.completed() {
        return Err(DbError::protocol(
            "BASE_BACKUP stream ended before BackupEnd frame",
        ));
    }
    let _ = writer.shutdown().await;

    header_observed
        .ok_or_else(|| DbError::protocol("BASE_BACKUP stream completed without a Header frame"))
}

/// Variant of [`run`] that lets the caller observe driver-level counters.
pub async fn run_with_metrics(
    config: ReplicaClientConfig,
    receiver: Arc<WalReceiver>,
    mut shutdown_rx: watch::Receiver<bool>,
    metrics: ReplicaMetrics,
) {
    let mut backoff = BACKOFF_INITIAL;
    let mut first_session = true;
    loop {
        if *shutdown_rx.borrow() {
            info!("replica streaming driver shutdown requested");
            return;
        }
        if !first_session {
            metrics.note_reconnect();
        }
        first_session = false;
        metrics.note_session_started();
        let session_result =
            run_session(&config, Arc::clone(&receiver), &mut shutdown_rx, &metrics).await;
        if *shutdown_rx.borrow() {
            info!("replica streaming driver shutdown requested after session");
            metrics.note_session_succeeded();
            return;
        }
        match session_result {
            Ok(()) => {
                metrics.note_session_succeeded();
                info!("replica streaming driver session ended cleanly; reconnecting");
                backoff = BACKOFF_INITIAL;
            }
            Err(err) => {
                metrics.note_session_failed();
                error!(error = %err, "replica streaming driver session failed; will reconnect");
                let jitter = Duration::from_millis(fastrand_jitter_ms());
                tokio::select! {
                    () = sleep(backoff + jitter) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

async fn run_session(
    config: &ReplicaClientConfig,
    receiver: Arc<WalReceiver>,
    shutdown_rx: &mut watch::Receiver<bool>,
    metrics: &ReplicaMetrics,
) -> DbResult<()> {
    let addr = config.conninfo.socket_addr();
    debug!(addr = %addr, "connecting to replication primary");
    let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| DbError::internal(format!("primary connect timed out: {addr}")))
        .and_then(|res| {
            res.map_err(|err| DbError::internal(format!("primary connect failed ({addr}): {err}")))
        })?;
    stream
        .set_nodelay(true)
        .map_err(|err| DbError::internal(format!("set_nodelay failed: {err}")))?;
    let stream = maybe_upgrade_to_tls(stream, &config.conninfo).await?;
    let (mut reader, mut writer) = tokio::io::split(stream);

    send_startup(&mut writer, &config.conninfo).await?;
    handshake_until_ready(&mut reader, &mut writer, &config.conninfo).await?;

    let identity = issue_identify_system(&mut writer, &mut reader).await?;
    if let Some(expected) = config.expected_system_identifier.as_deref() {
        if expected != identity.system_identifier {
            return Err(DbError::internal(format!(
                "primary system identifier mismatch: expected {}, primary reports {}",
                expected, identity.system_identifier
            )));
        }
    }
    let timeline = identity.timeline;
    if let Some(expected_timeline) = config.expected_timeline {
        if expected_timeline != timeline {
            return Err(DbError::internal(format!(
                "primary timeline mismatch: expected {expected_timeline}, primary reports {timeline}"
            )));
        }
    }
    let parsed_sysid = identity.system_identifier.parse::<u64>().map_err(|err| {
        DbError::internal(format!(
            "primary IDENTIFY_SYSTEM returned non-numeric systemid \"{}\": {err}",
            identity.system_identifier
        ))
    })?;
    // Try the cheap path first: identities already match.
    if let Err(initial_err) = receiver.validate_upstream_identity(parsed_sysid, timeline) {
        // If only the timeline differs and the replica hasn't replayed past
        // the fork point, accept the new timeline. Otherwise propagate the
        // error -- we cannot keep streaming in a divergent history.
        let local_timeline = receiver.local_timeline();
        let local_flush = receiver.flush_lsn();
        if local_timeline.map_or(false, |local| local >= timeline) {
            return Err(initial_err);
        }
        let history = fetch_timeline_history(&mut writer, &mut reader, timeline).await?;
        let fork_lsn = timeline_fork_lsn(&history, timeline);
        if let Some(fork) = fork_lsn {
            if local_flush > fork {
                return Err(DbError::internal(format!(
                    "replica diverged from primary: flushed past timeline {timeline} fork at {} (replica flush_lsn = {})",
                    fork.get(),
                    local_flush.get()
                )));
            }
        }
        receiver.set_local_timeline(timeline)?;
        info!(
            previous_timeline = local_timeline,
            new_timeline = timeline,
            fork_lsn = fork_lsn.map(Lsn::get),
            "accepted primary's new timeline"
        );
        receiver.validate_upstream_identity(parsed_sysid, timeline)?;
    }

    let resume_lsn = receiver.flush_lsn().max(Lsn::new(1));
    let pg_lsn = format_pg_lsn(resume_lsn);
    let start_cmd = match config.conninfo.slot_name.as_deref() {
        Some(slot) => format!("START_REPLICATION SLOT {slot} PHYSICAL {pg_lsn}"),
        None => format!("START_REPLICATION {pg_lsn}"),
    };
    debug!(start_cmd = %start_cmd, "issuing START_REPLICATION");
    send_simple_query(&mut writer, &start_cmd).await?;
    expect_copy_both(&mut reader).await?;

    let mut status_timer =
        tokio::time::interval(config.status_interval.max(Duration::from_secs(1)));
    status_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    status_timer.tick().await; // consume immediate tick; send initial status explicitly.
    send_standby_status_update(&mut writer, receiver.as_ref(), metrics).await?;

    loop {
        tokio::select! {
            biased;
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    // Best-effort graceful close: send CopyDone followed by
                    // Terminate so the primary unregisters the WAL sender
                    // promptly instead of waiting on its TCP keepalive.
                    let _ = send_copy_done(&mut writer).await;
                    let _ = send_terminate(&mut writer).await;
                    let _ = writer.shutdown().await;
                    return Ok(());
                }
            }
            _ = status_timer.tick() => {
                send_standby_status_update(&mut writer, receiver.as_ref(), metrics).await?;
            }
            res = timeout(READ_TIMEOUT, read_backend_message(&mut reader)) => {
                let raw = res
                    .map_err(|_| DbError::internal("primary read timeout"))?
                    ?;
                match raw.tag {
                    b'd' => {
                        metrics.note_wal_bytes(raw.payload.len() as u64);
                        let (message, consumed) = ReplicationMessage::decode(&raw.payload)?;
                        if consumed != raw.payload.len() {
                            return Err(DbError::protocol(
                                "CopyData frame contains trailing bytes after replication message",
                            ));
                        }
                        match message {
                            ReplicationMessage::WalData { .. } => {
                                receiver.receive_message(&message)?;
                                receiver.flush_durable()?;
                            }
                            ReplicationMessage::Keepalive {
                                wal_end,
                                timestamp_us,
                                reply_requested,
                            } => {
                                receiver.receive_message(&ReplicationMessage::Keepalive {
                                    wal_end,
                                    timestamp_us,
                                    reply_requested,
                                })?;
                                if reply_requested {
                                    send_standby_status_update(
                                        &mut writer,
                                        receiver.as_ref(),
                                        metrics,
                                    )
                                    .await?;
                                }
                            }
                            ReplicationMessage::StandbyStatusUpdate { .. }
                            | ReplicationMessage::KeepaliveReply { .. } => {
                                return Err(DbError::protocol(
                                    "primary sent a standby-only replication message",
                                ));
                            }
                        }
                    }
                    b'c' => {
                        // CopyDone from primary -- treat as graceful close.
                        return Ok(());
                    }
                    b'E' => {
                        return Err(DbError::internal(format!(
                            "primary sent ErrorResponse: {}",
                            decode_error_response(&raw.payload)
                        )));
                    }
                    b'N' => {
                        // NoticeResponse -- log and continue.
                        let notice = decode_error_response(&raw.payload);
                        warn!(notice = %notice, "primary notice");
                    }
                    other => {
                        return Err(DbError::protocol(format!(
                            "unexpected backend tag {other:#04x} during replication stream"
                        )));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

async fn send_startup<W>(writer: &mut W, conn: &ConnInfo) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    params.insert("user", conn.user.clone());
    params.insert("replication", "true".to_owned());
    if let Some(db) = &conn.database {
        params.insert("database", db.clone());
    }
    if let Some(app) = &conn.application_name {
        params.insert("application_name", app.clone());
    }

    let mut payload = Vec::new();
    payload.extend_from_slice(&STARTUP_PROTOCOL_VERSION.to_be_bytes());
    for (k, v) in &params {
        payload.extend_from_slice(k.as_bytes());
        payload.push(0);
        payload.extend_from_slice(v.as_bytes());
        payload.push(0);
    }
    payload.push(0);
    let total_len = u32::try_from(payload.len() + 4)
        .map_err(|_| DbError::internal("startup message too large"))?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&total_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    writer
        .write_all(&frame)
        .await
        .map_err(|err| DbError::internal(format!("startup write failed: {err}")))?;
    Ok(())
}

async fn handshake_until_ready<R, W>(
    reader: &mut R,
    writer: &mut W,
    conn: &ConnInfo,
) -> DbResult<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut scram_client: Option<aiondb_security::scram::ScramClient> = None;
    loop {
        let raw = read_backend_message(reader).await?;
        match raw.tag {
            b'R' => {
                if raw.payload.len() < 4 {
                    return Err(DbError::protocol("AuthenticationRequest payload too short"));
                }
                let subtype = u32::from_be_bytes([
                    raw.payload[0],
                    raw.payload[1],
                    raw.payload[2],
                    raw.payload[3],
                ]);
                match subtype {
                    0 => { /* AuthenticationOk */ }
                    3 => {
                        // AuthenticationCleartextPassword
                        let password = conn.password.as_deref().ok_or_else(|| {
                            DbError::feature_not_supported(
                                "primary requested cleartext password but \
                                 primary_conninfo did not include password=",
                            )
                        })?;
                        send_password_message(writer, password).await?;
                    }
                    5 => {
                        // AuthenticationMD5Password: payload is `subtype:u32 | salt[4]`
                        if raw.payload.len() != 8 {
                            return Err(DbError::protocol(
                                "AuthenticationMD5Password payload must be 8 bytes",
                            ));
                        }
                        let password = conn.password.as_deref().ok_or_else(|| {
                            DbError::feature_not_supported(
                                "primary requested MD5 password but \
                                 primary_conninfo did not include password=",
                            )
                        })?;
                        let salt = [
                            raw.payload[4],
                            raw.payload[5],
                            raw.payload[6],
                            raw.payload[7],
                        ];
                        let token = compute_md5_password_token(&conn.user, password, salt);
                        send_password_message(writer, &token).await?;
                    }
                    10 => {
                        // AuthenticationSASL: payload is the mechanism list,
                        // each entry NUL-terminated, ending with an empty
                        // entry.
                        let password = conn.password.as_deref().ok_or_else(|| {
                            DbError::feature_not_supported(
                                "primary requested SASL auth but primary_conninfo \
                                 did not include password=",
                            )
                        })?;
                        if !sasl_payload_advertises_scram_sha_256(&raw.payload[4..]) {
                            return Err(DbError::feature_not_supported(
                                "primary advertised SASL mechanism that is not \
                                 SCRAM-SHA-256; replication driver only supports \
                                 SCRAM-SHA-256",
                            ));
                        }
                        let mut client =
                            aiondb_security::scram::ScramClient::new(&conn.user, password);
                        let first = client.client_first_message()?.clone();
                        send_sasl_initial_response(
                            writer,
                            aiondb_security::scram::SCRAM_SHA_256_MECHANISM,
                            first.message.as_bytes(),
                        )
                        .await?;
                        scram_client = Some(client);
                    }
                    11 => {
                        // AuthenticationSASLContinue: server-first-message
                        let server_first =
                            std::str::from_utf8(&raw.payload[4..]).map_err(|err| {
                                DbError::protocol(format!(
                                    "SCRAM server-first-message is not UTF-8: {err}"
                                ))
                            })?;
                        let client = scram_client.as_mut().ok_or_else(|| {
                            DbError::protocol(
                                "primary sent AuthenticationSASLContinue without prior \
                                 AuthenticationSASL",
                            )
                        })?;
                        let final_msg = client.process_server_first(server_first)?;
                        send_sasl_response(writer, final_msg.message.as_bytes()).await?;
                    }
                    12 => {
                        // AuthenticationSASLFinal: server-final-message
                        let server_final =
                            std::str::from_utf8(&raw.payload[4..]).map_err(|err| {
                                DbError::protocol(format!(
                                    "SCRAM server-final-message is not UTF-8: {err}"
                                ))
                            })?;
                        let client = scram_client.as_ref().ok_or_else(|| {
                            DbError::protocol(
                                "primary sent AuthenticationSASLFinal without prior \
                                 AuthenticationSASL exchange",
                            )
                        })?;
                        client.verify_server_final(server_final)?;
                    }
                    other => {
                        return Err(DbError::feature_not_supported(format!(
                            "replication driver supports only trust, cleartext, MD5 \
                             or SCRAM-SHA-256 auth; primary requested method {other}"
                        )));
                    }
                }
            }
            b'S' | b'K' | b'N' => {
                // ParameterStatus, BackendKeyData, NoticeResponse -- accept.
            }
            b'Z' => {
                return Ok(());
            }
            b'E' => {
                return Err(DbError::internal(format!(
                    "primary returned ErrorResponse during startup: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected startup tag {other:#04x}"
                )));
            }
        }
    }
}

fn sasl_payload_advertises_scram_sha_256(payload: &[u8]) -> bool {
    let mechanism = aiondb_security::scram::SCRAM_SHA_256_MECHANISM.as_bytes();
    payload.split(|b| *b == 0).any(|name| name == mechanism)
}

async fn send_sasl_initial_response<W>(
    writer: &mut W,
    mechanism: &str,
    initial_response: &[u8],
) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    // PasswordMessage frame: tag 'p' | len | <mechanism>\0 | i32 len | bytes
    let body_len = mechanism.len() + 1 + 4 + initial_response.len();
    let total = u32::try_from(body_len + 4)
        .map_err(|_| DbError::internal("SASL initial response too large"))?;
    let mut buf = Vec::with_capacity(body_len + 5);
    buf.push(b'p');
    buf.extend_from_slice(&total.to_be_bytes());
    buf.extend_from_slice(mechanism.as_bytes());
    buf.push(0);
    let resp_len = i32::try_from(initial_response.len())
        .map_err(|_| DbError::internal("SASL initial response too large"))?;
    buf.extend_from_slice(&resp_len.to_be_bytes());
    buf.extend_from_slice(initial_response);
    writer
        .write_all(&buf)
        .await
        .map_err(|err| DbError::internal(format!("SASL initial response write failed: {err}")))
}

async fn send_sasl_response<W>(writer: &mut W, body: &[u8]) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let total =
        u32::try_from(body.len() + 4).map_err(|_| DbError::internal("SASL response too large"))?;
    let mut buf = Vec::with_capacity(body.len() + 5);
    buf.push(b'p');
    buf.extend_from_slice(&total.to_be_bytes());
    buf.extend_from_slice(body);
    writer
        .write_all(&buf)
        .await
        .map_err(|err| DbError::internal(format!("SASL response write failed: {err}")))
}

struct IdentifySystemResult {
    system_identifier: String,
    timeline: u32,
}

async fn issue_identify_system<W, R>(
    writer: &mut W,
    reader: &mut R,
) -> DbResult<IdentifySystemResult>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncReadExt + Unpin,
{
    send_simple_query(writer, "IDENTIFY_SYSTEM").await?;
    let mut system_identifier = None;
    let mut timeline = None;
    loop {
        let raw = read_backend_message(reader).await?;
        match raw.tag {
            b'T' => { /* RowDescription -- ignore column metadata */ }
            b'D' => {
                let values = decode_data_row(&raw.payload)?;
                if let Some(Some(sysid)) = values.first() {
                    system_identifier = Some(sysid.clone());
                }
                if let Some(Some(tli)) = values.get(1) {
                    timeline = Some(tli.parse::<u32>().map_err(|err| {
                        DbError::internal(format!(
                            "IDENTIFY_SYSTEM returned non-numeric timeline \"{tli}\": {err}"
                        ))
                    })?);
                }
            }
            b'C' => { /* CommandComplete */ }
            b'Z' => break,
            b'E' => {
                return Err(DbError::internal(format!(
                    "IDENTIFY_SYSTEM failed: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected backend tag {other:#04x} during IDENTIFY_SYSTEM"
                )));
            }
        }
    }
    let system_identifier = system_identifier.ok_or_else(|| {
        DbError::internal("primary IDENTIFY_SYSTEM response missing systemid column")
    })?;
    let timeline = timeline.ok_or_else(|| {
        DbError::internal("primary IDENTIFY_SYSTEM response missing timeline column")
    })?;
    Ok(IdentifySystemResult {
        system_identifier,
        timeline,
    })
}

async fn expect_copy_both<R>(reader: &mut R) -> DbResult<()>
where
    R: AsyncReadExt + Unpin,
{
    loop {
        let raw = read_backend_message(reader).await?;
        match raw.tag {
            b'W' => return Ok(()),
            b'N' => {}
            b'E' => {
                return Err(DbError::internal(format!(
                    "START_REPLICATION rejected: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected backend tag {other:#04x} after START_REPLICATION"
                )));
            }
        }
    }
}

async fn send_simple_query<W>(writer: &mut W, sql: &str) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let body_len = sql
        .len()
        .checked_add(1)
        .ok_or_else(|| DbError::internal("simple query too large"))?;
    let payload_len = pg_frame_len(body_len, "simple query")?;
    let mut buf = Vec::with_capacity(pg_tagged_frame_capacity(payload_len)?);
    buf.push(b'Q');
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.extend_from_slice(sql.as_bytes());
    buf.push(0);
    writer
        .write_all(&buf)
        .await
        .map_err(|err| DbError::internal(format!("simple query write failed: {err}")))
}

/// Issue `TIMELINE_HISTORY <tli>` against the primary and return the body
/// of the history file (raw, may be empty if the requested timeline is the
/// initial one with no fork yet).
async fn fetch_timeline_history<W, R>(
    writer: &mut W,
    reader: &mut R,
    timeline: u32,
) -> DbResult<String>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncReadExt + Unpin,
{
    send_simple_query(writer, &format!("TIMELINE_HISTORY {timeline}")).await?;
    let mut content = String::new();
    loop {
        let raw = read_backend_message(reader).await?;
        match raw.tag {
            b'T' => { /* RowDescription */ }
            b'D' => {
                let row = decode_data_row(&raw.payload)?;
                // PG returns (filename, content) for TIMELINE_HISTORY; the
                // history is the second column.
                if let Some(Some(body)) = row.get(1) {
                    content = body.clone();
                }
            }
            b'C' => { /* CommandComplete */ }
            b'Z' => break,
            b'N' => {} // Notice
            b'E' => {
                return Err(DbError::internal(format!(
                    "TIMELINE_HISTORY {timeline} failed: {}",
                    decode_error_response(&raw.payload)
                )));
            }
            other => {
                return Err(DbError::protocol(format!(
                    "unexpected backend tag {other:#04x} during TIMELINE_HISTORY"
                )));
            }
        }
    }
    Ok(content)
}

/// Parse a PG timeline history file and return the LSN at which `timeline`
/// forked from its parent. Returns `None` if the requested timeline is the
/// initial one (no entry in the history).
///
/// PG format (one entry per line, tab-separated):
/// `<parent_tli>\t<switch_lsn>\t<reason>`
fn timeline_fork_lsn(history: &str, timeline: u32) -> Option<Lsn> {
    for line in history.lines() {
        let mut parts = line.split_whitespace();
        let Some(parent_str) = parts.next() else {
            continue;
        };
        let Ok(parent) = parent_str.parse::<u32>() else {
            continue;
        };
        let Some(lsn_token) = parts.next() else {
            continue;
        };
        // The fork LSN sits on the line that introduces `timeline` -- i.e.
        // the parent + 1 == timeline.
        if parent + 1 == timeline {
            return parse_pg_lsn(lsn_token);
        }
    }
    None
}

fn parse_pg_lsn(token: &str) -> Option<Lsn> {
    let (high, low) = token.split_once('/')?;
    let high_part = u32::from_str_radix(high.trim(), 16).ok()?;
    let low_part = u32::from_str_radix(low.trim(), 16).ok()?;
    let raw = (u64::from(high_part) << 32) | u64::from(low_part);
    Some(Lsn::new(raw))
}

async fn send_copy_done<W>(writer: &mut W) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    // CopyDone: 'c' | len(u32)=4
    let frame = [b'c', 0, 0, 0, 4];
    writer
        .write_all(&frame)
        .await
        .map_err(|err| DbError::internal(format!("CopyDone write failed: {err}")))
}

async fn send_terminate<W>(writer: &mut W) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    // Terminate: 'X' | len(u32)=4
    let frame = [b'X', 0, 0, 0, 4];
    writer
        .write_all(&frame)
        .await
        .map_err(|err| DbError::internal(format!("Terminate write failed: {err}")))
}

async fn send_password_message<W>(writer: &mut W, password: &str) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    // PasswordMessage: 'p' | len(u32) | password\0
    let body_len = password
        .len()
        .checked_add(1)
        .ok_or_else(|| DbError::internal("password message too large"))?;
    let payload_len = pg_frame_len(body_len, "password message")?;
    let mut buf = Vec::with_capacity(pg_tagged_frame_capacity(payload_len)?);
    buf.push(b'p');
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.extend_from_slice(password.as_bytes());
    buf.push(0);
    writer
        .write_all(&buf)
        .await
        .map_err(|err| DbError::internal(format!("password write failed: {err}")))
}

async fn send_copy_data<W>(writer: &mut W, payload: &[u8]) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let total = pg_frame_len(payload.len(), "CopyData payload")?;
    let mut frame = Vec::with_capacity(pg_tagged_frame_capacity(total)?);
    frame.push(b'd');
    frame.extend_from_slice(&total.to_be_bytes());
    frame.extend_from_slice(payload);
    writer
        .write_all(&frame)
        .await
        .map_err(|err| DbError::internal(format!("CopyData write failed: {err}")))
}

async fn send_standby_status_update<W>(
    writer: &mut W,
    receiver: &WalReceiver,
    metrics: &ReplicaMetrics,
) -> DbResult<()>
where
    W: AsyncWriteExt + Unpin,
{
    let update = receiver.status_update();
    if let ReplicationMessage::StandbyStatusUpdate { .. } = update {
        let payload = update.encode()?;
        send_copy_data(writer, &payload).await?;
        metrics.note_status_update_sent();
    }
    Ok(())
}

#[derive(Debug)]
struct RawBackendMessage {
    tag: u8,
    payload: Vec<u8>,
}

async fn read_backend_message<R>(reader: &mut R) -> DbResult<RawBackendMessage>
where
    R: AsyncReadExt + Unpin,
{
    let mut header = [0u8; 5];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|err| DbError::internal(format!("read header failed: {err}")))?;
    let tag = header[0];
    let total_len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if total_len < 4 {
        return Err(DbError::protocol(format!(
            "backend message length {total_len} below minimum"
        )));
    }
    let payload_len = total_len - 4;
    if payload_len > MAX_BACKEND_MESSAGE_BYTES {
        return Err(DbError::protocol(format!(
            "backend message length {total_len} exceeds maximum"
        )));
    }
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader
            .read_exact(&mut payload)
            .await
            .map_err(|err| DbError::internal(format!("read payload failed: {err}")))?;
    }
    Ok(RawBackendMessage { tag, payload })
}

fn pg_frame_len(body_len: usize, label: &str) -> DbResult<u32> {
    let total = body_len
        .checked_add(PG_FRAME_LEN_BYTES)
        .ok_or_else(|| DbError::internal(format!("{label} too large")))?;
    if total > MAX_FRONTEND_MESSAGE_BYTES {
        return Err(DbError::internal(format!("{label} exceeds maximum")));
    }
    u32::try_from(total).map_err(|_| DbError::internal(format!("{label} too large")))
}

fn pg_tagged_frame_capacity(total_len: u32) -> DbResult<usize> {
    let total_len = usize::try_from(total_len)
        .map_err(|_| DbError::internal("frontend frame too large for this platform"))?;
    if total_len > MAX_FRONTEND_MESSAGE_BYTES {
        return Err(DbError::internal("frontend frame exceeds maximum"));
    }
    total_len
        .checked_add(PG_FRAME_TAG_BYTES)
        .ok_or_else(|| DbError::internal("frontend frame too large for this platform"))
}

fn decode_data_row(payload: &[u8]) -> DbResult<Vec<Option<String>>> {
    if payload.len() < 2 {
        return Err(DbError::protocol("DataRow payload too short"));
    }
    let column_count = i16::from_be_bytes([payload[0], payload[1]]);
    if column_count < 0 {
        return Err(DbError::protocol("DataRow column count is negative"));
    }
    let column_count = usize::try_from(column_count)
        .map_err(|_| DbError::protocol("DataRow column count is negative"))?;
    if column_count > MAX_BACKEND_DATA_ROW_COLUMNS {
        return Err(DbError::protocol(format!(
            "DataRow column count {column_count} exceeds maximum {MAX_BACKEND_DATA_ROW_COLUMNS}"
        )));
    }
    let mut columns = Vec::with_capacity(column_count);
    let mut offset = 2;
    for _ in 0..column_count {
        if offset + 4 > payload.len() {
            return Err(DbError::protocol("DataRow payload truncated"));
        }
        let len = i32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        offset += 4;
        if len == -1 {
            columns.push(None);
            continue;
        }
        let len = usize::try_from(len)
            .map_err(|_| DbError::protocol("DataRow value length is negative"))?;
        if offset + len > payload.len() {
            return Err(DbError::protocol("DataRow value extends past payload"));
        }
        let text = String::from_utf8(payload[offset..offset + len].to_vec())
            .map_err(|err| DbError::protocol(format!("DataRow value is not UTF-8: {err}")))?;
        columns.push(Some(text));
        offset += len;
    }
    if offset != payload.len() {
        return Err(DbError::protocol("DataRow payload contains trailing bytes"));
    }
    Ok(columns)
}

fn decode_error_response(payload: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0;
    while offset < payload.len() {
        let field_tag = payload[offset];
        offset += 1;
        if field_tag == 0 {
            break;
        }
        let start = offset;
        while offset < payload.len() && payload[offset] != 0 {
            offset += 1;
        }
        let value = String::from_utf8_lossy(&payload[start..offset]).into_owned();
        if matches!(field_tag, b'M' | b'D' | b'H') {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&value);
        }
        if offset < payload.len() {
            offset += 1; // skip nul terminator
        }
    }
    if out.is_empty() {
        "<no message>".to_owned()
    } else {
        out
    }
}

fn format_pg_lsn(lsn: Lsn) -> String {
    let raw = lsn.get();
    let lower = u32::try_from(raw & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    format!("{:X}/{:08X}", raw >> 32, lower)
}

/// PostgreSQL MD5 password token: `md5` + hex(md5(hex(md5(password + user)) + salt)).
fn compute_md5_password_token(user: &str, password: &str, salt: [u8; 4]) -> String {
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

fn fastrand_jitter_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    u64::from(nanos % 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_wal_dir(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("aiondb-replica-client-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp wal dir");
        dir
    }

    #[test]
    fn conninfo_parse_minimal() {
        let info = ConnInfo::parse("host=10.0.0.1 port=6432 user=replicator").expect("parse");
        assert_eq!(info.host, "10.0.0.1");
        assert_eq!(info.port, 6432);
        assert_eq!(info.user, "replicator");
        assert!(info.database.is_none());
    }

    #[test]
    fn conninfo_parse_full() {
        let info = ConnInfo::parse(
            "host=primary port=5432 user=alice dbname=aiondb application_name=replica1",
        )
        .expect("parse");
        assert_eq!(info.host, "primary");
        assert_eq!(info.port, 5432);
        assert_eq!(info.user, "alice");
        assert_eq!(info.database.as_deref(), Some("aiondb"));
        assert_eq!(info.application_name.as_deref(), Some("replica1"));
    }

    #[test]
    fn conninfo_parse_rejects_unknown_keyword() {
        let err = ConnInfo::parse("host=x foo=bar").expect_err("foo is not allowed");
        assert!(err.to_string().contains("unknown keyword"));
    }

    #[test]
    fn conninfo_parse_rejects_malformed_token() {
        let err = ConnInfo::parse("host x").expect_err("missing equals");
        assert!(err.to_string().contains("not key=value"));
    }

    #[test]
    fn conninfo_parse_captures_password() {
        let info = ConnInfo::parse("host=primary user=alice password=hunter2").expect("parse");
        assert_eq!(info.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn conninfo_parse_accepts_quoted_values() {
        let info = ConnInfo::parse(
            "host = primary user=alice password='hun ter' application_name='replica one'",
        )
        .expect("parse");
        assert_eq!(info.password.as_deref(), Some("hun ter"));
        assert_eq!(info.application_name.as_deref(), Some("replica one"));
    }

    #[test]
    fn conninfo_parse_accepts_backslash_escaped_quotes() {
        let info = ConnInfo::parse("host=primary user=alice password='hun\\'ter'")
            .expect("parse escaped password");
        assert_eq!(info.password.as_deref(), Some("hun'ter"));
    }

    #[test]
    fn conninfo_parse_rejects_unterminated_quoted_value() {
        let err = ConnInfo::parse("host=primary user=alice password='unterminated")
            .expect_err("unterminated quoted value should fail");
        assert!(
            err.to_string().contains("unterminated quoted value"),
            "{err}"
        );
    }

    #[test]
    fn conninfo_parse_rejects_nul_values() {
        let err = ConnInfo::parse("host=primary user=ali\0ce")
            .expect_err("NUL bytes must not reach startup parameters");
        assert!(err.to_string().contains("contains NUL byte"), "{err}");

        let err = ConnInfo::parse("host=primary user=alice password=hun\0ter2")
            .expect_err("NUL bytes must not reach password messages");
        assert!(err.to_string().contains("contains NUL byte"), "{err}");
    }

    #[test]
    fn conninfo_debug_redacts_password() {
        let info = ConnInfo::parse("host=primary user=alice password=hunter2").expect("parse");
        let debug = format!("{info:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("hunter2"));
    }

    #[test]
    fn conninfo_parse_accepts_plaintext_sslmodes() {
        let info = ConnInfo::parse("host=primary user=alice sslmode=prefer").expect("parse");
        assert_eq!(info.host, "primary");
        ConnInfo::parse("host=primary user=alice sslmode=disable").expect("disable");
        ConnInfo::parse("host=primary user=alice sslmode=allow").expect("allow");
    }

    #[test]
    fn conninfo_parse_accepts_strict_tls_sslmodes() {
        let info =
            ConnInfo::parse("host=primary user=alice sslmode=require").expect("require parses");
        assert_eq!(info.sslmode, SslMode::Require);

        let err = ConnInfo::parse("host=primary user=alice sslmode=verify-ca")
            .expect_err("verify-ca without sslrootcert must be rejected");
        assert!(err.to_string().contains("sslrootcert"), "{err}");

        let err = ConnInfo::parse("host=primary user=alice sslmode=verify-full")
            .expect_err("verify-full without sslrootcert must be rejected");
        assert!(err.to_string().contains("sslrootcert"), "{err}");

        // With sslrootcert, verify-ca succeeds.
        let info = ConnInfo::parse(
            "host=primary user=alice sslmode=verify-ca sslrootcert=/etc/aiondb/ca.pem",
        )
        .expect("verify-ca with sslrootcert");
        assert_eq!(info.sslmode, SslMode::VerifyCa);
        assert_eq!(info.ssl_root_cert.as_deref(), Some("/etc/aiondb/ca.pem"));
    }

    #[test]
    fn conninfo_parse_rejects_unknown_sslmode() {
        let err = ConnInfo::parse("host=primary user=alice sslmode=maybe")
            .expect_err("invalid sslmode should be rejected");
        assert!(err.to_string().contains("invalid sslmode"), "{err}");
    }

    #[test]
    fn conninfo_parse_captures_replication_slot() {
        let info = ConnInfo::parse("host=primary user=alice replication_slot=standby_alice")
            .expect("parse");
        assert_eq!(info.slot_name.as_deref(), Some("standby_alice"));
    }

    #[test]
    fn conninfo_parse_rejects_invalid_slot_name() {
        let err = ConnInfo::parse("host=primary user=alice replication_slot=BAD-NAME")
            .expect_err("slot name with hyphen must be rejected");
        assert!(err.to_string().contains("replication_slot"), "{err}");
    }

    #[test]
    fn parse_pg_lsn_decodes_two_part_hex_form() {
        assert_eq!(parse_pg_lsn("0/00000010"), Some(Lsn::new(0x10)));
        assert_eq!(parse_pg_lsn("1/FFFFFFFF"), Some(Lsn::new(0x1_FFFF_FFFF)));
        assert_eq!(parse_pg_lsn("not/a/lsn"), None);
        assert_eq!(parse_pg_lsn("0"), None);
    }

    #[test]
    fn timeline_fork_lsn_finds_matching_branch() {
        let history = "1\t0/000000F0\tno recovery target specified\n\
                       2\t0/00000200\tno recovery target specified\n";
        assert_eq!(timeline_fork_lsn(history, 2), Some(Lsn::new(0xF0)));
        assert_eq!(timeline_fork_lsn(history, 3), Some(Lsn::new(0x200)));
        assert_eq!(timeline_fork_lsn(history, 1), None);
    }

    #[test]
    fn timeline_fork_lsn_skips_malformed_lines() {
        let history = "garbage\n1\t0/00000010\treason\n";
        assert_eq!(timeline_fork_lsn(history, 2), Some(Lsn::new(0x10)));
    }

    #[test]
    fn md5_token_matches_pg_reference_value() {
        // PostgreSQL md5 auth wire token (src/backend/libpq/auth.c):
        // md5_token = "md5" + md5hex(md5hex(password + user) + salt).
        // Reference inputs ("alice" / "secret" / [1,2,3,4]) cross-checked
        // against Python's hashlib.md5.
        let salt = [0x01u8, 0x02, 0x03, 0x04];
        let token = compute_md5_password_token("alice", "secret", salt);
        assert_eq!(token, "md598a0412b9c31436fc53776e863350083");
    }

    #[tokio::test]
    async fn password_message_uses_pg_p_frame() {
        let (mut client_side, mut server_side) = tokio::io::duplex(64);
        send_password_message(&mut client_side, "hunter2")
            .await
            .expect("send");
        let mut header = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut server_side, &mut header)
            .await
            .expect("read header");
        assert_eq!(header[0], b'p');
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; len - 4];
        tokio::io::AsyncReadExt::read_exact(&mut server_side, &mut payload)
            .await
            .expect("read payload");
        assert_eq!(payload, b"hunter2\0");
    }

    #[tokio::test]
    async fn standby_status_update_sends_copy_data_and_updates_metrics() {
        let wal_dir = temp_wal_dir("status");
        let receiver = WalReceiver::open(aiondb_wal::WalConfig {
            dir: wal_dir.clone(),
            wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
            ..aiondb_wal::WalConfig::default()
        })
        .expect("open receiver");
        let metrics = ReplicaMetrics::new();
        let (mut client_side, mut server_side) = tokio::io::duplex(128);

        send_standby_status_update(&mut client_side, &receiver, &metrics)
            .await
            .expect("send status");

        let mut header = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut server_side, &mut header)
            .await
            .expect("read header");
        assert_eq!(header[0], b'd');
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; len - 4];
        tokio::io::AsyncReadExt::read_exact(&mut server_side, &mut payload)
            .await
            .expect("read payload");
        let (message, consumed) = ReplicationMessage::decode(&payload).expect("decode status");
        assert_eq!(consumed, payload.len());
        assert!(matches!(
            message,
            ReplicationMessage::StandbyStatusUpdate { .. }
        ));
        assert_eq!(metrics.snapshot().standby_status_updates_sent, 1);

        let _ = std::fs::remove_dir_all(wal_dir);
    }

    #[test]
    fn pg_lsn_formatter_matches_postgres_convention() {
        assert_eq!(format_pg_lsn(Lsn::new(0)), "0/00000000");
        assert_eq!(format_pg_lsn(Lsn::new(0x12345678)), "0/12345678");
        assert_eq!(format_pg_lsn(Lsn::new(0x0000_0001_FFFF_FFFF)), "1/FFFFFFFF");
    }

    #[test]
    fn decode_data_row_extracts_text_columns() {
        // 2 columns: "424242", "1"
        let mut payload = Vec::new();
        payload.extend_from_slice(&2_i16.to_be_bytes());
        let v1 = b"424242";
        payload.extend_from_slice(&(v1.len() as i32).to_be_bytes());
        payload.extend_from_slice(v1);
        let v2 = b"1";
        payload.extend_from_slice(&(v2.len() as i32).to_be_bytes());
        payload.extend_from_slice(v2);
        let row = decode_data_row(&payload).expect("decode");
        assert_eq!(row, vec![Some("424242".to_owned()), Some("1".to_owned())]);
    }

    #[test]
    fn decode_data_row_handles_null_column() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1_i16.to_be_bytes());
        payload.extend_from_slice(&(-1_i32).to_be_bytes());
        let row = decode_data_row(&payload).expect("decode null");
        assert_eq!(row, vec![None]);
    }

    #[test]
    fn decode_data_row_rejects_trailing_bytes() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1_i16.to_be_bytes());
        let value = b"1";
        payload.extend_from_slice(&(value.len() as i32).to_be_bytes());
        payload.extend_from_slice(value);
        payload.push(0);

        let err = decode_data_row(&payload).expect_err("trailing bytes should be rejected");
        assert!(err.to_string().contains("trailing bytes"), "{err}");
    }

    #[test]
    fn decode_data_row_rejects_excessive_column_count() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&((MAX_BACKEND_DATA_ROW_COLUMNS as i16) + 1).to_be_bytes());

        let err = decode_data_row(&payload).expect_err("excessive column count");
        assert!(err.to_string().contains("exceeds maximum"), "{err}");
    }

    #[test]
    fn frontend_frame_len_rejects_oversized_or_overflowing_body() {
        pg_frame_len(MAX_FRONTEND_MESSAGE_BYTES - PG_FRAME_LEN_BYTES, "query").expect("at cap");

        let err = pg_frame_len(MAX_FRONTEND_MESSAGE_BYTES - PG_FRAME_LEN_BYTES + 1, "query")
            .expect_err("over cap");
        assert!(err.to_string().contains("exceeds maximum"), "{err}");

        let err = pg_frame_len(usize::MAX, "query").expect_err("integer overflow");
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[tokio::test]
    async fn read_backend_message_rejects_oversized_frame_before_payload() {
        let (mut client_side, mut server_side) = tokio::io::duplex(16);
        let total_len = u32::try_from(MAX_BACKEND_MESSAGE_BYTES + 5).expect("fits in u32");
        client_side.write_all(b"D").await.expect("write tag");
        client_side
            .write_all(&total_len.to_be_bytes())
            .await
            .expect("write length");

        let err = read_backend_message(&mut server_side)
            .await
            .expect_err("oversized backend frame");
        assert!(err.to_string().contains("exceeds maximum"), "{err}");
    }
}
