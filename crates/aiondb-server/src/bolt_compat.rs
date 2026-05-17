use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiondb_engine::{
    Credential, DataType, Engine, PortalBatch, QueryEngine, QueryExtendedProtocol,
    IsolationLevel, SecretString, SessionHandle, StartupParams, StatementResult, TransportInfo,
    TransportKind,
};
use aiondb_parser::{parse_sql, Statement};
use serde_json::{Map as JsonMap, Value as JsonValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::query_api::rewrite_named_parameters;

const BOLT_MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];
const BOLT_MANIFEST_V1: u32 = 0x0000_01FF;
const BOLT_SUPPORTED_VERSIONS: [u32; 8] = [
    0x0000_0006,
    0x0000_0805,
    0x0000_0705,
    0x0000_0605,
    0x0000_0505,
    0x0000_0205,
    0x0000_0105,
    0x0000_0404,
];
const BOLT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const BOLT_CHUNK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const BOLT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const BOLT_SUCCESS_SIGNATURE: u8 = 0x70;
const BOLT_FAILURE_SIGNATURE: u8 = 0x7F;
const BOLT_IGNORED_SIGNATURE: u8 = 0x7E;
const BOLT_HELLO_SIGNATURE: u8 = 0x01;
const BOLT_GOODBYE_SIGNATURE: u8 = 0x02;
const BOLT_RECORD_SIGNATURE: u8 = 0x71;
const BOLT_RUN_SIGNATURE: u8 = 0x10;
const BOLT_BEGIN_SIGNATURE: u8 = 0x11;
const BOLT_COMMIT_SIGNATURE: u8 = 0x12;
const BOLT_ROLLBACK_SIGNATURE: u8 = 0x13;
const BOLT_ROUTE_SIGNATURE: u8 = 0x66;
const BOLT_PULL_SIGNATURE: u8 = 0x3F;
const BOLT_RESET_SIGNATURE: u8 = 0x0F;
const BOLT_DISCARD_SIGNATURE: u8 = 0x2F;
const BOLT_LOGON_SIGNATURE: u8 = 0x6A;
const BOLT_LOGOFF_SIGNATURE: u8 = 0x6B;
const BOLT_TELEMETRY_SIGNATURE: u8 = 0x54;
const BOLT_DEFAULT_DATABASE: &str = "default";
const BOLT_APPLICATION_NAME: &str = "aiondb-bolt-compat";
const BOLT_CONNECTION_RECV_TIMEOUT_SECONDS: i64 = 120;
const BOLT_AUTH_FAILURE_CODE: &str = "Neo.ClientError.Security.Unauthorized";
const BOLT_REQUEST_FAILURE_CODE: &str = "Neo.ClientError.Request.Invalid";
const BOLT_FORBIDDEN_FAILURE_CODE: &str = "Neo.ClientError.Security.Forbidden";
const BOLT_DATABASE_FAILURE_CODE: &str = "Neo.ClientError.Database.DatabaseNotFound";
const BOLT_MAX_ENCODE_DEPTH: usize = 256;
const MICROS_PER_SECOND: i64 = 1_000_000;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
static NEXT_BOLT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct BoltHelloMetadata {
    principal: Option<String>,
    credentials: Option<String>,
    scheme: Option<String>,
    user_agent: Option<String>,
    extra_metadata: JsonMap<String, JsonValue>,
}

#[derive(Default)]
struct BoltConnectionState {
    session: Option<SessionHandle>,
    pending: Option<BoltPendingQuery>,
    failed: bool,
    next_bookmark_id: u64,
    last_bookmark: Option<String>,
    connection_id: String,
}

struct BoltPendingQuery {
    fields: Vec<String>,
    records: Vec<Vec<u8>>,
    next_record: usize,
    qid: i64,
    bookmark: Option<String>,
    result_available_after_ms: u64,
    status: BoltStatusKind,
    summary: Vec<(&'static str, String)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoltStatusKind {
    Success,
    OmittedResult,
    NoData,
}

struct BoltRunMessage {
    statement: String,
    params: JsonMap<String, JsonValue>,
    database: Option<String>,
}

struct BoltStreamControl {
    n: Option<usize>,
    qid: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoltCompatConfig {
    pub bind_address: String,
    pub port: u16,
}

impl Default for BoltCompatConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_owned(),
            port: 7687,
        }
    }
}

pub(crate) async fn spawn_bolt_compat_server(
    config: BoltCompatConfig,
    engine: Arc<Engine>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<tokio::task::JoinHandle<()>, io::Error> {
    let addr = format!("{}:{}", config.bind_address, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(address = %addr, "bolt compatibility server ready");

    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = wait_for_shutdown(shutdown_rx.clone()) => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer_addr)) => {
                            let engine = Arc::clone(&engine);
                            tokio::spawn(async move {
                                if let Err(err) = handle_bolt_connection(stream, engine).await {
                                    warn!(%peer_addr, %err, "bolt compatibility connection failed");
                                }
                            });
                        }
                        Err(err) => {
                            error!(%err, "bolt compatibility accept failed");
                            break;
                        }
                    }
                }
            }
        }
    }))
}

pub(crate) async fn handle_bolt_connection(
    mut stream: tokio::net::TcpStream,
    engine: Arc<Engine>,
) -> Result<(), io::Error> {
    let handshake = tokio::time::timeout(BOLT_HANDSHAKE_TIMEOUT, async {
        let mut preface = [0u8; 20];
        stream.read_exact(&mut preface).await?;
        Ok::<[u8; 20], io::Error>(preface)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "bolt handshake timed out"))??;

    if handshake[..4] != BOLT_MAGIC {
        let _ = stream.write_all(&0u32.to_be_bytes()).await;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid bolt magic preface",
        ));
    }

    let mut offered = [0u32; 4];
    for (index, slot) in offered.iter_mut().enumerate() {
        let offset = 4 + (index * 4);
        *slot = u32::from_be_bytes([
            handshake[offset],
            handshake[offset + 1],
            handshake[offset + 2],
            handshake[offset + 3],
        ]);
    }

    let negotiated = negotiate_version(&offered).unwrap_or(0);
    stream.write_all(&negotiated.to_be_bytes()).await?;
    stream.flush().await?;
    if negotiated == 0 {
        return Ok(());
    }

    let mut state = BoltConnectionState {
        connection_id: format!(
            "{BOLT_APPLICATION_NAME}:{}",
            NEXT_BOLT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed)
        ),
        ..Default::default()
    };
    loop {
        let message = match tokio::time::timeout(
            BOLT_CHUNK_READ_TIMEOUT,
            read_chunked_message(&mut stream),
        )
        .await
        {
            Ok(Ok(message)) => message,
            Ok(Err(err))
                if matches!(
                    err.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) =>
            {
                break;
            }
            Ok(Err(err)) => return Err(err),
            Err(_) => break,
        };
        if message.is_empty() {
            break;
        }
        let should_close = handle_bolt_message(&mut stream, &message, &engine, &mut state).await?;
        if should_close {
            break;
        }
    }
    if let Some(session) = state.session.take() {
        let _ = engine.terminate(session);
    }

    Ok(())
}

fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) -> impl std::future::Future<Output = ()> {
    async move {
        let _ = shutdown_rx.changed().await;
    }
}

fn negotiate_version(offered: &[u32; 4]) -> Option<u32> {
    for version in offered {
        if *version == 0 || *version == BOLT_MANIFEST_V1 {
            continue;
        }
        if let Some(chosen) = match_version_offer(*version) {
            return Some(chosen);
        }
    }
    None
}

fn match_version_offer(offer: u32) -> Option<u32> {
    let [reserved, range, minor, major] = offer.to_be_bytes();
    if reserved != 0 {
        return None;
    }

    if range == 0 {
        return BOLT_SUPPORTED_VERSIONS
            .iter()
            .copied()
            .find(|supported| *supported == offer);
    }

    let highest_minor = minor;
    let lowest_minor = highest_minor.saturating_sub(range);
    BOLT_SUPPORTED_VERSIONS.iter().copied().find(|supported| {
        let [supported_reserved, _, supported_minor, supported_major] = supported.to_be_bytes();
        supported_reserved == 0
            && supported_major == major
            && supported_minor >= lowest_minor
            && supported_minor <= highest_minor
    })
}

async fn handle_bolt_message(
    stream: &mut tokio::net::TcpStream,
    payload: &[u8],
    engine: &Arc<Engine>,
    state: &mut BoltConnectionState,
) -> Result<bool, io::Error> {
    let signature = decode_top_level_message_signature(payload)?;
    if state.failed
        && !matches!(
            signature,
            BOLT_RESET_SIGNATURE | BOLT_GOODBYE_SIGNATURE | BOLT_TELEMETRY_SIGNATURE
        )
    {
        write_chunked_message(stream, &encode_ignored_message()).await?;
        return Ok(false);
    }
    match signature {
        BOLT_HELLO_SIGNATURE => {
            let hello = match decode_hello_metadata(payload) {
                Ok(hello) => hello,
                Err(_err) => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "invalid HELLO message"),
                    )
                    .await?;
                    return Ok(false);
                }
            };
            let maybe_session = match authenticate_hello(engine, &hello) {
                Ok(session) => session,
                Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_AUTH_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    state.failed = true;
                    return Ok(false);
                }
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    state.failed = true;
                    return Ok(false);
                }
                Err(err) => return Err(err),
            };
            write_chunked_message(
                stream,
                &encode_hello_success_message(&state.connection_id),
            )
            .await?;
            if let Some(session) = maybe_session {
                if let Some(existing) = state.session.replace(session) {
                    let _ = engine.terminate(existing);
                }
                state.last_bookmark = None;
            }
            state.pending = None;
            state.failed = false;
            Ok(false)
        }
        BOLT_LOGON_SIGNATURE => {
            let auth = match decode_auth_metadata(payload, BOLT_LOGON_SIGNATURE) {
                Ok(auth) => auth,
                Err(_err) => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "invalid LOGON message"),
                    )
                    .await?;
                    return Ok(false);
                }
            };
            let session = if let Some(session) = match authenticate_hello(engine, &auth) {
                Ok(session) => session,
                Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_AUTH_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    state.failed = true;
                    return Ok(false);
                }
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    if let Some(existing) = state.session.take() {
                        let _ = engine.terminate(existing);
                    }
                    state.pending = None;
                    state.last_bookmark = None;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    state.failed = true;
                    return Ok(false);
                }
                Err(err) => return Err(err),
            } {
                session
            } else {
                if let Some(existing) = state.session.take() {
                    let _ = engine.terminate(existing);
                }
                state.pending = None;
                state.last_bookmark = None;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            };
            if let Some(existing) = state.session.replace(session) {
                let _ = engine.terminate(existing);
            }
            state.pending = None;
            state.last_bookmark = None;
            state.failed = false;
            write_chunked_message(stream, &encode_success_message(&[])).await?;
            Ok(false)
        }
        BOLT_LOGOFF_SIGNATURE => {
            if let Err(err) = ensure_zero_field_message(payload, BOLT_LOGOFF_SIGNATURE, "LOGOFF") {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Some(existing) = state.session.take() {
                let _ = engine.terminate(existing);
            }
            state.pending = None;
            state.last_bookmark = None;
            state.failed = false;
            write_chunked_message(stream, &encode_success_message(&[])).await?;
            Ok(false)
        }
        BOLT_TELEMETRY_SIGNATURE => {
            if decode_telemetry_api(payload).is_err() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "invalid TELEMETRY message"),
                )
                .await?;
                state.failed = true;
                return Ok(false);
            }
            write_chunked_message(stream, &encode_success_message(&[])).await?;
            Ok(false)
        }
        BOLT_RUN_SIGNATURE => {
            if state.session.is_none() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            }
            let session = state.session.as_ref().expect("checked session");
            match execute_run_message(engine, session, payload) {
                Ok(pending) => {
                    let fields = pending.fields.clone();
                    let qid = pending.qid;
                    let t_first = pending.result_available_after_ms;
                    let database = pending
                        .summary
                        .iter()
                        .find(|(key, _)| *key == "db")
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(|| BOLT_DEFAULT_DATABASE.to_owned());
                    let in_explicit_txn =
                        QueryEngine::has_active_transaction(engine.as_ref(), session)
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
                    let mut pending = pending;
                    if !in_explicit_txn {
                        state.next_bookmark_id = state.next_bookmark_id.saturating_add(1);
                        let bookmark = format!("aiondb:bookmark:{}", state.next_bookmark_id);
                        state.last_bookmark = Some(bookmark.clone());
                        pending.bookmark = Some(bookmark);
                    }
                    state.pending = Some(pending);
                    state.failed = false;
                    write_chunked_message(
                        stream,
                        &encode_run_success_message(&fields, qid, &database, t_first),
                    )
                    .await?;
                    Ok(false)
                }
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_DATABASE_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    Ok(false)
                }
                Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(
                            BOLT_FORBIDDEN_FAILURE_CODE,
                            &err.to_string(),
                        ),
                    )
                    .await?;
                    Ok(false)
                }
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    Ok(false)
                }
            }
        }
        BOLT_ROUTE_SIGNATURE => {
            if state.session.is_none() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            }
            let route_metadata = match decode_route_metadata(payload) {
                Ok(metadata) => metadata,
                Err(err) => {
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    return Ok(false);
                }
            };
            if let Err(err) = ensure_only_supported_operation_metadata(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_impersonation(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_FORBIDDEN_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_supported_bookmarks(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_unsupported_transaction_metadata(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_unsupported_session_auth_metadata(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_string_metadata(&route_metadata, "db") {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_notification_filtering_metadata_types(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_supported_access_mode(&route_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_FORBIDDEN_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            let requested_database = route_metadata
                .get("db")
                .and_then(JsonValue::as_str)
                .map(ToOwned::to_owned);
            if let Err(err) = ensure_supported_database(requested_database.as_deref()) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_DATABASE_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            state.pending = None;
            state.failed = false;
            let address = stream
                .local_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|_| "127.0.0.1:7687".to_owned());
            write_chunked_message(
                stream,
                &encode_route_success_message(
                    &address,
                    requested_database.as_deref().unwrap_or(BOLT_DEFAULT_DATABASE),
                ),
            )
            .await?;
            Ok(false)
        }
        BOLT_BEGIN_SIGNATURE => {
            if state.session.is_none() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            }
            let begin_metadata = decode_begin_metadata(payload)?;
            if let Err(err) = ensure_only_supported_operation_metadata(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_impersonation(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_FORBIDDEN_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_supported_bookmarks(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_unsupported_transaction_metadata(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_no_unsupported_session_auth_metadata(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_string_metadata(&begin_metadata, "db") {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_notification_filtering_metadata_types(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Err(err) = ensure_supported_access_mode(&begin_metadata) {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_FORBIDDEN_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            ensure_supported_database(begin_metadata.get("db").and_then(JsonValue::as_str))?;
            let session = state.session.as_ref().expect("checked session");
            match QueryEngine::begin_transaction(engine.as_ref(), session, IsolationLevel::ReadCommitted) {
                Ok(()) => {
                    state.pending = None;
                    state.failed = false;
                    state.last_bookmark = None;
                    write_chunked_message(stream, &encode_success_message(&[])).await?;
                    Ok(false)
                }
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    Ok(false)
                }
            }
        }
        BOLT_COMMIT_SIGNATURE => {
            if let Err(err) = ensure_zero_field_message(payload, BOLT_COMMIT_SIGNATURE, "COMMIT") {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if state.session.is_none() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            }
            let session = state.session.as_ref().expect("checked session");
            match QueryEngine::commit_transaction(engine.as_ref(), session) {
                Ok(()) => {
                    state.pending = None;
                    state.failed = false;
                    state.next_bookmark_id = state.next_bookmark_id.saturating_add(1);
                    let bookmark = format!("aiondb:bookmark:{}", state.next_bookmark_id);
                    state.last_bookmark = Some(bookmark.clone());
                    write_chunked_message(
                        stream,
                        &encode_success_message(&[
                            ("bookmark", &bookmark),
                            ("db", BOLT_DEFAULT_DATABASE),
                        ]),
                    )
                    .await?;
                    Ok(false)
                }
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    Ok(false)
                }
            }
        }
        BOLT_ROLLBACK_SIGNATURE => {
            if let Err(err) =
                ensure_zero_field_message(payload, BOLT_ROLLBACK_SIGNATURE, "ROLLBACK")
            {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if state.session.is_none() {
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required"),
                )
                .await?;
                return Ok(false);
            }
            let session = state.session.as_ref().expect("checked session");
            match QueryEngine::rollback_transaction(engine.as_ref(), session) {
                Ok(()) => {
                    state.pending = None;
                    state.failed = false;
                    state.last_bookmark = None;
                    write_chunked_message(stream, &encode_success_message(&[])).await?;
                    Ok(false)
                }
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    Ok(false)
                }
            }
        }
        BOLT_PULL_SIGNATURE => {
            let pull = match decode_pull_metadata(payload, BOLT_PULL_SIGNATURE) {
                Ok(pull) => pull,
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    return Ok(false);
                }
            };
            if let Err(err) = ensure_supported_qid(pull.qid) {
                state.pending = None;
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            let Some(mut pending) = state.pending.take() else {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "no pending result to pull"),
                )
                .await?;
                return Ok(false);
            };
            let pull_started = Instant::now();
            let remaining = pending.records.len().saturating_sub(pending.next_record);
            let batch_size = pull.n.map(|n| n.min(remaining)).unwrap_or(remaining);
            let end = pending.next_record.saturating_add(batch_size);
            for record in &pending.records[pending.next_record..end] {
                write_chunked_message(stream, record).await?;
            }
            pending.next_record = end;
            let has_more = pending.next_record < pending.records.len();
            let t_last = u64::try_from(pull_started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let summary_pairs: Vec<(&str, &str)> = pending
                .summary
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();
            let summary_with_bookmark = if !has_more {
                pending.bookmark.as_deref().map(|bookmark| {
                    let mut pairs = summary_pairs.clone();
                    pairs.push(("bookmark", bookmark));
                    pairs
                })
            } else {
                None
            };
            write_chunked_message(
                stream,
                &encode_pull_success_message_with_status(
                    summary_with_bookmark
                        .as_deref()
                        .unwrap_or(summary_pairs.as_slice()),
                    pending.qid,
                    has_more,
                    t_last,
                    pending.status,
                ),
            )
            .await?;
            if has_more {
                state.pending = Some(pending);
                state.failed = false;
            } else {
                state.failed = false;
            }
            Ok(false)
        }
        BOLT_DISCARD_SIGNATURE => {
            let discard = match decode_pull_metadata(payload, BOLT_DISCARD_SIGNATURE) {
                Ok(discard) => discard,
                Err(err) => {
                    state.pending = None;
                    state.failed = true;
                    write_chunked_message(
                        stream,
                        &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                    )
                    .await?;
                    return Ok(false);
                }
            };
            if let Err(err) = ensure_supported_qid(discard.qid) {
                state.pending = None;
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            let pending = state.pending.take();
            state.failed = false;
            let summary_pairs: Vec<(&str, &str)> = pending
                .as_ref()
                .map(|pending| {
                    let mut pairs: Vec<(&str, &str)> = pending
                        .summary
                        .iter()
                        .map(|(k, v)| (*k, v.as_str()))
                        .collect();
                    if let Some(bookmark) = pending.bookmark.as_deref() {
                        pairs.push(("bookmark", bookmark));
                    }
                    pairs
                })
                .unwrap_or_else(|| vec![("type", "r")]);
            write_chunked_message(
                stream,
                &encode_pull_success_message_with_status(
                    &summary_pairs,
                    pending.as_ref().map(|pending| pending.qid).unwrap_or(0),
                    false,
                    0,
                    BoltStatusKind::OmittedResult,
                ),
            )
            .await?;
            Ok(false)
        }
        BOLT_RESET_SIGNATURE => {
            if let Err(err) = ensure_zero_field_message(payload, BOLT_RESET_SIGNATURE, "RESET") {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            if let Some(session) = state.session.as_ref() {
                let has_active_txn =
                    QueryEngine::has_active_transaction(engine.as_ref(), session).map_err(|err| {
                        io::Error::new(io::ErrorKind::InvalidData, err.to_string())
                    })?;
                if has_active_txn {
                    QueryEngine::rollback_transaction(engine.as_ref(), session).map_err(|err| {
                        io::Error::new(io::ErrorKind::InvalidData, err.to_string())
                    })?;
                    state.last_bookmark = None;
                }
            }
            state.pending = None;
            state.failed = false;
            write_chunked_message(stream, &encode_success_message(&[])).await?;
            Ok(false)
        }
        BOLT_GOODBYE_SIGNATURE => {
            if let Err(err) = ensure_zero_field_message(payload, BOLT_GOODBYE_SIGNATURE, "GOODBYE")
            {
                state.failed = true;
                write_chunked_message(
                    stream,
                    &encode_failure_message(BOLT_REQUEST_FAILURE_CODE, &err.to_string()),
                )
                .await?;
                return Ok(false);
            }
            Ok(true)
        }
        _ => {
            write_chunked_message(
                stream,
                &encode_failure_message(
                    BOLT_REQUEST_FAILURE_CODE,
                    "unsupported Bolt message",
                ),
            )
            .await?;
            Ok(false)
        }
    }
}

fn decode_route_metadata(payload: &[u8]) -> Result<JsonMap<String, JsonValue>, io::Error> {
    if payload.len() < 3 || payload[0] != 0xB3 || payload[1] != BOLT_ROUTE_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt ROUTE must be a three-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let _routing_context = decode_string_keyed_string_map(&mut remaining)?;
    let _bookmarks = decode_string_list(&mut remaining)?;
    let metadata = decode_string_keyed_json_map(&mut remaining)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after Bolt ROUTE metadata",
        ));
    }
    Ok(metadata)
}

fn ensure_zero_field_message(
    payload: &[u8],
    expected_signature: u8,
    name: &str,
) -> Result<(), io::Error> {
    if payload.len() != 2 || payload[0] != 0xB0 || payload[1] != expected_signature {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {name} must be a zero-field struct"),
        ));
    }
    Ok(())
}

fn decode_begin_metadata(payload: &[u8]) -> Result<JsonMap<String, JsonValue>, io::Error> {
    if payload.len() < 3 || payload[0] != 0xB1 || payload[1] != BOLT_BEGIN_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt BEGIN must be a single-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let map = decode_string_keyed_json_map(&mut remaining)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after Bolt BEGIN metadata",
        ));
    }
    Ok(map)
}

fn decode_string_keyed_string_map<'a>(
    input: &mut &'a [u8],
) -> Result<std::collections::HashMap<String, String>, io::Error> {
    let map_len = decode_map_len(input)?;
    let mut map = std::collections::HashMap::with_capacity(map_len);
    for _ in 0..map_len {
        let key = decode_string(input)?;
        let value = decode_string(input)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn decode_string_list<'a>(input: &mut &'a [u8]) -> Result<Vec<String>, io::Error> {
    let len = decode_list_len(input)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(decode_string(input)?);
    }
    Ok(values)
}

fn decode_top_level_message_signature(payload: &[u8]) -> Result<u8, io::Error> {
    if payload.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt message too short",
        ));
    }
    let marker = payload[0];
    let field_count = match marker {
        0xB0..=0xBF => (marker & 0x0F) as usize,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt message must start with a tiny struct",
            ))
        }
    };
    let signature = payload[1];
    let mut remaining = &payload[2..];
    for _ in 0..field_count {
        remaining = skip_packstream_value(remaining, 0)?;
    }
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after bolt message",
        ));
    }
    Ok(signature)
}

fn decode_hello_metadata(payload: &[u8]) -> Result<BoltHelloMetadata, io::Error> {
    decode_auth_metadata(payload, BOLT_HELLO_SIGNATURE)
}

fn decode_auth_metadata(payload: &[u8], signature: u8) -> Result<BoltHelloMetadata, io::Error> {
    if payload.len() < 3 || payload[0] != 0xB1 || payload[1] != signature {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt auth message must be a single-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let map_len = decode_map_len(&mut remaining)?;

    let mut principal = None;
    let mut credentials = None;
    let mut user_agent = None;
    let mut scheme = None;
    let mut extra_metadata = JsonMap::new();
    for _ in 0..map_len {
        let key = decode_string(&mut remaining)?;
        match key.as_str() {
            "principal" => principal = Some(decode_string(&mut remaining)?),
            "credentials" => credentials = Some(decode_string(&mut remaining)?),
            "user_agent" => user_agent = Some(decode_string(&mut remaining)?),
            "scheme" => scheme = Some(decode_string(&mut remaining)?),
            _ => {
                let value = decode_json_value(&mut remaining, 0)?;
                extra_metadata.insert(key, value);
            }
        }
    }
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after bolt auth metadata",
        ));
    }
    Ok(BoltHelloMetadata {
        principal,
        credentials,
        scheme,
        user_agent,
        extra_metadata,
    })
}

fn decode_map_len<'a>(input: &mut &'a [u8]) -> Result<usize, io::Error> {
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt map",
        ));
    };
    *input = rest;
    match marker {
        0xA0..=0xAF => Ok((marker & 0x0F) as usize),
        0xD8 => {
            let (len, tail) = decode_sized_len(*input, 1)?;
            *input = tail;
            Ok(len)
        }
        0xD9 => {
            let (len, tail) = decode_sized_len(*input, 2)?;
            *input = tail;
            Ok(len)
        }
        0xDA => {
            let (len, tail) = decode_sized_len(*input, 4)?;
            *input = tail;
            Ok(len)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt hello metadata must be a map",
        )),
    }
}

fn decode_list_len<'a>(input: &mut &'a [u8]) -> Result<usize, io::Error> {
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt list",
        ));
    };
    *input = rest;
    match marker {
        0x90..=0x9F => Ok((marker & 0x0F) as usize),
        0xD4 => {
            let (len, tail) = decode_sized_len(*input, 1)?;
            *input = tail;
            Ok(len)
        }
        0xD5 => {
            let (len, tail) = decode_sized_len(*input, 2)?;
            *input = tail;
            Ok(len)
        }
        0xD6 => {
            let (len, tail) = decode_sized_len(*input, 4)?;
            *input = tail;
            Ok(len)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt value must be a list",
        )),
    }
}

fn decode_string<'a>(input: &mut &'a [u8]) -> Result<String, io::Error> {
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt string",
        ));
    };
    *input = rest;
    let (len, tail) = match marker {
        0x80..=0x8F => ((marker & 0x0F) as usize, *input),
        0xD0 => decode_sized_len(*input, 1)?,
        0xD1 => decode_sized_len(*input, 2)?,
        0xD2 => decode_sized_len(*input, 4)?,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt value must be a string",
            ))
        }
    };
    if tail.len() < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt string payload",
        ));
    }
    let value = std::str::from_utf8(&tail[..len])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bolt string is not utf-8"))?;
    *input = &tail[len..];
    Ok(value.to_owned())
}

fn decode_pull_metadata(payload: &[u8], expected_signature: u8) -> Result<BoltStreamControl, io::Error> {
    if payload.len() < 3 || payload[0] != 0xB1 || payload[1] != expected_signature {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt stream control must be a single-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let map_len = decode_map_len(&mut remaining)?;
    let mut pull_n = None;
    let mut qid = None;
    for _ in 0..map_len {
        let key = decode_string(&mut remaining)?;
        if key == "n" {
            pull_n = Some(decode_optional_usize(&mut remaining)?);
        } else if key == "qid" {
            qid = decode_optional_i64(&mut remaining)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported Bolt stream control metadata key \"{key}\""),
            ));
        }
    }
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after Bolt stream control metadata",
        ));
    }
    Ok(BoltStreamControl {
        n: pull_n.flatten(),
        qid,
    })
}

fn decode_telemetry_api(payload: &[u8]) -> Result<i64, io::Error> {
    if payload.len() < 3 || payload[0] != 0xB1 || payload[1] != BOLT_TELEMETRY_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt TELEMETRY message must be a single-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let api = decode_optional_i64(&mut remaining)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt TELEMETRY api must be an integer",
        )
    })?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected trailing data in TELEMETRY message",
        ));
    }
    Ok(api)
}

fn decode_optional_usize<'a>(input: &mut &'a [u8]) -> Result<Option<usize>, io::Error> {
    let value = decode_optional_i64(input)?;
    match value {
        None => Ok(None),
        Some(value) if value < 0 => Ok(None),
        Some(value) => usize::try_from(value).map(Some).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt integer must fit in usize",
            )
        }),
    }
}

fn decode_optional_i64<'a>(input: &mut &'a [u8]) -> Result<Option<i64>, io::Error> {
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt integer",
        ));
    };
    *input = rest;
    let value = match marker {
        0xC0 => return Ok(None),
        0x00..=0x7F => i64::from(marker),
        0xF0..=0xFF => i64::from(i8::from_be_bytes([marker])),
        0xC8 => {
            let bytes = take_exact(input, 1)?;
            i64::from(i8::from_be_bytes([bytes[0]]))
        }
        0xC9 => {
            let bytes = take_exact(input, 2)?;
            i64::from(i16::from_be_bytes([bytes[0], bytes[1]]))
        }
        0xCA => {
            let bytes = take_exact(input, 4)?;
            i64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        0xCB => {
            let bytes = take_exact(input, 8)?;
            i64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt value must be an integer",
            ))
        }
    };
    Ok(Some(value))
}

fn take_exact<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], io::Error> {
    if input.len() < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt integer payload",
        ));
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Ok(head)
}

fn authenticate_hello(
    engine: &Arc<Engine>,
    hello: &BoltHelloMetadata,
) -> Result<Option<SessionHandle>, io::Error> {
    let has_any_auth_field =
        hello.scheme.is_some() || hello.principal.is_some() || hello.credentials.is_some();
    ensure_only_supported_handshake_metadata(&hello.extra_metadata)?;
    ensure_no_unsupported_handshake_metadata(&hello.extra_metadata)?;
    ensure_no_unsupported_session_auth_metadata(&hello.extra_metadata)?;
    ensure_no_impersonation(&hello.extra_metadata)?;
    ensure_notification_filtering_metadata_types(&hello.extra_metadata)?;
    if has_any_auth_field && hello.scheme.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bolt authentication metadata must include scheme",
        ));
    }
    if let Some(scheme) = hello.scheme.as_deref() {
        if scheme != "basic" {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Bolt compatibility only supports auth scheme \"basic\", got {scheme:?}"),
            ));
        }
    }
    if hello.principal.is_none() || hello.credentials.is_none() {
        if has_any_auth_field {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Bolt authentication metadata must include both principal and credentials",
            ));
        }
        return Ok(None);
    }
    let params = StartupParams {
        database: BOLT_DEFAULT_DATABASE.to_owned(),
        application_name: hello
            .user_agent
            .clone()
            .or_else(|| Some(BOLT_APPLICATION_NAME.to_owned())),
        options: Default::default(),
        credential: Credential::CleartextPassword {
            user: hello.principal.clone().expect("checked principal"),
            password: SecretString::new(hello.credentials.clone().expect("checked credentials")),
        },
        transport: TransportInfo {
            kind: TransportKind::Network {
                tls: true,
                peer_addr: Some("127.0.0.1:7687".to_owned()),
            },
        },
    };
    match engine.startup(params) {
        Ok((session, _)) => Ok(Some(session)),
        Err(err) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("authentication failed: {err}"),
        )),
    }
}

fn skip_packstream_value(input: &[u8], depth: usize) -> Result<&[u8], io::Error> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt packstream nesting too deep",
        ));
    }
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt packstream value",
        ));
    };
    match marker {
        0x00..=0x7F | 0xF0..=0xFF | 0xC0 | 0xC2 | 0xC3 => Ok(rest),
        0xC1 => skip_bytes(rest, 8),
        0xC8 => skip_bytes(rest, 1),
        0xC9 => skip_bytes(rest, 2),
        0xCA => skip_bytes(rest, 4),
        0xCB => skip_bytes(rest, 8),
        0xCC => skip_sized_bytes(rest, 1),
        0xCD => skip_sized_bytes(rest, 2),
        0xCE => skip_sized_bytes(rest, 4),
        0x80..=0x8F => skip_bytes(rest, (marker & 0x0F) as usize),
        0xD0 => skip_sized_bytes(rest, 1),
        0xD1 => skip_sized_bytes(rest, 2),
        0xD2 => skip_sized_bytes(rest, 4),
        0x90..=0x9F => skip_sequence(rest, (marker & 0x0F) as usize, depth + 1),
        0xD4 => skip_sized_sequence(rest, 1, depth + 1),
        0xD5 => skip_sized_sequence(rest, 2, depth + 1),
        0xD6 => skip_sized_sequence(rest, 4, depth + 1),
        0xA0..=0xAF => skip_map(rest, (marker & 0x0F) as usize, depth + 1),
        0xD8 => skip_sized_map(rest, 1, depth + 1),
        0xD9 => skip_sized_map(rest, 2, depth + 1),
        0xDA => skip_sized_map(rest, 4, depth + 1),
        0xB0..=0xBF => skip_struct(rest, (marker & 0x0F) as usize, depth + 1),
        0xDC => skip_sized_struct(rest, 1, depth + 1),
        0xDD => skip_sized_struct(rest, 2, depth + 1),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported bolt packstream marker",
        )),
    }
}

fn skip_bytes(input: &[u8], len: usize) -> Result<&[u8], io::Error> {
    if input.len() < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt bytes payload",
        ));
    }
    Ok(&input[len..])
}

fn decode_sized_len(input: &[u8], width: usize) -> Result<(usize, &[u8]), io::Error> {
    if input.len() < width {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt size prefix",
        ));
    }
    let len = match width {
        1 => input[0] as usize,
        2 => u16::from_be_bytes([input[0], input[1]]) as usize,
        4 => u32::from_be_bytes([input[0], input[1], input[2], input[3]]) as usize,
        _ => unreachable!("invalid size width"),
    };
    Ok((len, &input[width..]))
}

fn skip_sized_bytes(input: &[u8], width: usize) -> Result<&[u8], io::Error> {
    let (len, rest) = decode_sized_len(input, width)?;
    skip_bytes(rest, len)
}

fn skip_sequence(mut input: &[u8], len: usize, depth: usize) -> Result<&[u8], io::Error> {
    for _ in 0..len {
        input = skip_packstream_value(input, depth)?;
    }
    Ok(input)
}

fn skip_sized_sequence(input: &[u8], width: usize, depth: usize) -> Result<&[u8], io::Error> {
    let (len, rest) = decode_sized_len(input, width)?;
    skip_sequence(rest, len, depth)
}

fn skip_map(mut input: &[u8], len: usize, depth: usize) -> Result<&[u8], io::Error> {
    for _ in 0..len {
        input = skip_packstream_value(input, depth)?;
        input = skip_packstream_value(input, depth)?;
    }
    Ok(input)
}

fn skip_sized_map(input: &[u8], width: usize, depth: usize) -> Result<&[u8], io::Error> {
    let (len, rest) = decode_sized_len(input, width)?;
    skip_map(rest, len, depth)
}

fn skip_struct(input: &[u8], len: usize, depth: usize) -> Result<&[u8], io::Error> {
    let Some((_, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt struct signature",
        ));
    };
    skip_sequence(rest, len, depth)
}

fn skip_sized_struct(input: &[u8], width: usize, depth: usize) -> Result<&[u8], io::Error> {
    let (len, rest) = decode_sized_len(input, width)?;
    skip_struct(rest, len, depth)
}

fn encode_success_message(metadata: &[(&str, &str)]) -> Vec<u8> {
    let mut payload = vec![0xB1, BOLT_SUCCESS_SIGNATURE];
    encode_map_header_into(&mut payload, metadata.len());
    for (key, value) in metadata {
        encode_string_into(&mut payload, key);
        encode_string_into(&mut payload, value);
    }
    payload
}

fn encode_hello_success_message(connection_id: &str) -> Vec<u8> {
    let mut payload = vec![0xB1, BOLT_SUCCESS_SIGNATURE];
    encode_map_header_into(&mut payload, 3);
    encode_string_into(&mut payload, "server");
    encode_string_into(&mut payload, "AionDB/bolt-compat");
    encode_string_into(&mut payload, "connection_id");
    encode_string_into(&mut payload, connection_id);
    encode_string_into(&mut payload, "hints");
    encode_map_header_into(&mut payload, 2);
    encode_string_into(&mut payload, "telemetry.enabled");
    payload.push(0xC2);
    encode_string_into(&mut payload, "connection.recv_timeout_seconds");
    encode_i64_into(&mut payload, BOLT_CONNECTION_RECV_TIMEOUT_SECONDS);
    payload
}

fn encode_run_success_message(fields: &[String], qid: i64, database: &str, t_first: u64) -> Vec<u8> {
    let mut payload = vec![0xB1, BOLT_SUCCESS_SIGNATURE];
    encode_map_header_into(&mut payload, 5);
    encode_string_into(&mut payload, "fields");
    encode_list_header_into(&mut payload, fields.len());
    for field in fields {
        encode_string_into(&mut payload, field);
    }
    encode_string_into(&mut payload, "qid");
    encode_i64_into(&mut payload, qid);
    encode_string_into(&mut payload, "db");
    encode_string_into(&mut payload, database);
    encode_string_into(&mut payload, "t_first");
    encode_i64_into(&mut payload, i64::try_from(t_first).unwrap_or(i64::MAX));
    encode_string_into(&mut payload, "result_available_after");
    encode_i64_into(&mut payload, i64::try_from(t_first).unwrap_or(i64::MAX));
    payload
}

fn encode_pull_success_message_with_status(
    metadata: &[(&str, &str)],
    qid: i64,
    has_more: bool,
    t_last: u64,
    status: BoltStatusKind,
) -> Vec<u8> {
    let map_len = metadata.len() + usize::from(has_more) + 5;
    let mut payload = vec![0xB1, BOLT_SUCCESS_SIGNATURE];
    encode_map_header_into(&mut payload, map_len);
    for (key, value) in metadata {
        encode_string_into(&mut payload, key);
        encode_string_into(&mut payload, value);
    }
    encode_string_into(&mut payload, "qid");
    encode_i64_into(&mut payload, qid);
    if has_more {
        encode_string_into(&mut payload, "has_more");
        payload.push(0xC3);
    }
    encode_string_into(&mut payload, "stats");
    encode_empty_stats_into(&mut payload);
    encode_string_into(&mut payload, "statuses");
    encode_statuses_into(&mut payload, status);
    encode_string_into(&mut payload, "t_last");
    encode_i64_into(&mut payload, i64::try_from(t_last).unwrap_or(i64::MAX));
    encode_string_into(&mut payload, "result_consumed_after");
    encode_i64_into(&mut payload, i64::try_from(t_last).unwrap_or(i64::MAX));
    payload
}

fn encode_statuses_into(buf: &mut Vec<u8>, status: BoltStatusKind) {
    encode_list_header_into(buf, 1);
    encode_map_header_into(buf, 5);
    let (gql_status, status_description, title, description) = match status {
        BoltStatusKind::Success => (
            "00000",
            "note: successful completion",
            "Successful completion",
            "note: successful completion",
        ),
        BoltStatusKind::OmittedResult => (
            "00001",
            "note: successful completion - omitted result",
            "Successful completion - omitted result",
            "note: successful completion - omitted result",
        ),
        BoltStatusKind::NoData => (
            "02000",
            "note: no data",
            "No data",
            "note: no data",
        ),
    };
    encode_string_into(buf, "gql_status");
    encode_string_into(buf, gql_status);
    encode_string_into(buf, "status_description");
    encode_string_into(buf, status_description);
    encode_string_into(buf, "title");
    encode_string_into(buf, title);
    encode_string_into(buf, "description");
    encode_string_into(buf, description);
    encode_string_into(buf, "diagnostic_record");
    encode_map_header_into(buf, 3);
    encode_string_into(buf, "OPERATION");
    encode_string_into(buf, "");
    encode_string_into(buf, "OPERATION_CODE");
    encode_string_into(buf, "0");
    encode_string_into(buf, "CURRENT_SCHEMA");
    encode_string_into(buf, "/");
}

fn encode_empty_stats_into(buf: &mut Vec<u8>) {
    const ZERO_I64_STATS: &[&str] = &[
        "nodes-created",
        "nodes-deleted",
        "relationships-created",
        "relationships-deleted",
        "properties-set",
        "labels-added",
        "labels-removed",
        "indexes-added",
        "indexes-removed",
        "constraints-added",
        "constraints-removed",
        "system-updates",
    ];
    const FALSE_BOOL_STATS: &[&str] = &["contains-updates", "contains-system-updates"];

    encode_map_header_into(buf, ZERO_I64_STATS.len() + FALSE_BOOL_STATS.len());
    for key in ZERO_I64_STATS {
        encode_string_into(buf, key);
        encode_i64_into(buf, 0);
    }
    for key in FALSE_BOOL_STATS {
        encode_string_into(buf, key);
        buf.push(0xC2);
    }
}

fn encode_route_success_message(address: &str, database: &str) -> Vec<u8> {
    let mut payload = vec![0xB1, BOLT_SUCCESS_SIGNATURE];
    encode_map_header_into(&mut payload, 2);
    encode_string_into(&mut payload, "rt");
    encode_map_header_into(&mut payload, 2);
    encode_string_into(&mut payload, "ttl");
    encode_i64_into(&mut payload, 300);
    encode_string_into(&mut payload, "servers");
    encode_list_header_into(&mut payload, 3);
    for role in ["ROUTE", "READ"] {
        encode_map_header_into(&mut payload, 2);
        encode_string_into(&mut payload, "role");
        encode_string_into(&mut payload, role);
        encode_string_into(&mut payload, "addresses");
        encode_list_header_into(&mut payload, 1);
        encode_string_into(&mut payload, address);
    }
    encode_map_header_into(&mut payload, 2);
    encode_string_into(&mut payload, "role");
    encode_string_into(&mut payload, "WRITE");
    encode_string_into(&mut payload, "addresses");
    encode_list_header_into(&mut payload, 0);
    encode_string_into(&mut payload, "db");
    encode_string_into(&mut payload, database);
    payload
}

fn encode_failure_message(code: &str, message: &str) -> Vec<u8> {
    let (gql_status, description) = match code {
        BOLT_AUTH_FAILURE_CODE | BOLT_FORBIDDEN_FAILURE_CODE => {
            ("28000", "error: invalid authorization specification")
        }
        BOLT_DATABASE_FAILURE_CODE => ("3D000", "error: invalid catalog name"),
        BOLT_REQUEST_FAILURE_CODE => ("42000", "error: syntax error or access rule violation"),
        _ => ("58000", "error: system error"),
    };
    let mut payload = vec![0xB1, BOLT_FAILURE_SIGNATURE];
    encode_map_header_into(&mut payload, 6);
    encode_string_into(&mut payload, "code");
    encode_string_into(&mut payload, code);
    encode_string_into(&mut payload, "neo4j_code");
    encode_string_into(&mut payload, code);
    encode_string_into(&mut payload, "message");
    encode_string_into(&mut payload, message);
    encode_string_into(&mut payload, "gql_status");
    encode_string_into(&mut payload, gql_status);
    encode_string_into(&mut payload, "description");
    encode_string_into(&mut payload, description);
    encode_string_into(&mut payload, "diagnostic_record");
    encode_map_header_into(&mut payload, 0);
    payload
}

fn encode_ignored_message() -> Vec<u8> {
    vec![0xB0, BOLT_IGNORED_SIGNATURE]
}

fn encode_string_into(buf: &mut Vec<u8>, value: &str) {
    let len = value.len();
    if len <= 15 {
        buf.push(0x80 | (len as u8));
    } else if u8::try_from(len).is_ok() {
        buf.push(0xD0);
        buf.push(len as u8);
    } else if u16::try_from(len).is_ok() {
        buf.push(0xD1);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xD2);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(value.as_bytes());
}

fn encode_map_header_into(buf: &mut Vec<u8>, len: usize) {
    if len <= 15 {
        buf.push(0xA0 | (len as u8));
    } else if u8::try_from(len).is_ok() {
        buf.push(0xD8);
        buf.push(len as u8);
    } else if u16::try_from(len).is_ok() {
        buf.push(0xD9);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xDA);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

fn execute_run_message(
    engine: &Arc<Engine>,
    session: &SessionHandle,
    payload: &[u8],
) -> Result<BoltPendingQuery, io::Error> {
    let started = Instant::now();
    let BoltRunMessage {
        statement,
        params,
        database,
    } = decode_run_statement(payload)?;
    let extra_metadata = database_metadata_from_payload(payload)?;
    ensure_only_supported_operation_metadata(&extra_metadata)?;
    ensure_no_impersonation(&extra_metadata)?;
    ensure_supported_bookmarks(&extra_metadata)?;
    ensure_no_unsupported_transaction_metadata(&extra_metadata)?;
    ensure_no_unsupported_session_auth_metadata(&extra_metadata)?;
    ensure_string_metadata(&extra_metadata, "db")?;
    ensure_notification_filtering_metadata_types(&extra_metadata)?;
    ensure_supported_access_mode(&extra_metadata)?;
    ensure_supported_database(database.as_deref())?;
    let (statement, params) = rewrite_named_parameters(statement, &params)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
    ensure_read_only_statement(&statement)?;
    let results = if params.is_empty() {
        engine
            .execute_sql(session, &statement)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?
    } else {
        let stmt_name = String::new();
        let param_hints = infer_param_hints(&params);
        QueryExtendedProtocol::prepare_with_param_hints(
            engine.as_ref(),
            session,
            stmt_name.clone(),
            statement,
            param_hints,
        )
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        let (batch, notices) = QueryExtendedProtocol::execute_prepared_statement_with_notices(
            engine.as_ref(),
            session,
            stmt_name,
            params,
            0,
        )
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        portal_batch_to_statement_results(batch, notices)
    };
    let mut pending = build_pending_query(results)?;
    pending.result_available_after_ms =
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    pending
        .summary
        .push(("db", database.unwrap_or_else(|| BOLT_DEFAULT_DATABASE.to_owned())));
    Ok(pending)
}

fn database_metadata_from_payload(payload: &[u8]) -> Result<JsonMap<String, JsonValue>, io::Error> {
    if payload.len() < 4 || payload[0] != 0xB3 || payload[1] != BOLT_RUN_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt RUN must be a three-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let _statement = decode_string(&mut remaining)?;
    let _params = decode_string_keyed_json_map(&mut remaining)?;
    let extra = decode_string_keyed_json_map(&mut remaining)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after Bolt RUN",
        ));
    }
    Ok(extra)
}

fn decode_run_statement(payload: &[u8]) -> Result<BoltRunMessage, io::Error> {
    if payload.len() < 4 || payload[0] != 0xB3 || payload[1] != BOLT_RUN_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt RUN must be a three-field struct",
        ));
    }
    let mut remaining = &payload[2..];
    let statement = decode_string(&mut remaining)?;
    let params = decode_string_keyed_json_map(&mut remaining)?;
    let extra = decode_string_keyed_json_map(&mut remaining)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extra trailing bytes after Bolt RUN",
        ));
    }
    Ok(BoltRunMessage {
        statement,
        params,
        database: extra
            .get("db")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned),
    })
}

fn ensure_supported_database(database: Option<&str>) -> Result<(), io::Error> {
    match database {
        None => Ok(()),
        Some(database) if database == BOLT_DEFAULT_DATABASE => Ok(()),
        Some(database) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Bolt compatibility only supports database {BOLT_DEFAULT_DATABASE:?}, got {database:?}"),
        )),
    }
}

fn ensure_only_supported_operation_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    const SUPPORTED_KEYS: &[&str] = &[
        "db",
        "mode",
        "bookmarks",
        "tx_timeout",
        "tx_metadata",
        "auth",
        "session_auth",
        "imp_user",
        "impersonated_user",
        "notifications_minimum_severity",
        "notifications_disabled_classifications",
        "notifications_disabled_categories",
    ];
    for key in metadata.keys() {
        if !SUPPORTED_KEYS.contains(&key.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported Bolt metadata key \"{key}\""),
            ));
        }
    }
    Ok(())
}

fn ensure_supported_bookmarks(metadata: &JsonMap<String, JsonValue>) -> Result<(), io::Error> {
    let Some(bookmarks) = metadata.get("bookmarks") else {
        return Ok(());
    };
    let JsonValue::Array(bookmarks) = bookmarks else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt bookmarks metadata must be a list of strings",
        ));
    };
    if bookmarks.iter().all(JsonValue::is_string) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt bookmarks metadata must be a list of strings",
        ))
    }
}

fn ensure_no_unsupported_transaction_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    if metadata.contains_key("tx_timeout") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility does not support tx_timeout metadata",
        ));
    }
    if metadata.contains_key("tx_metadata") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility does not support tx_metadata metadata",
        ));
    }
    Ok(())
}

fn ensure_no_unsupported_session_auth_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    if metadata.contains_key("auth") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility does not support auth metadata",
        ));
    }
    if metadata.contains_key("session_auth") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility does not support session_auth metadata",
        ));
    }
    Ok(())
}

fn ensure_no_unsupported_handshake_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    for key in ["db", "mode", "bookmarks", "tx_timeout", "tx_metadata"] {
        if metadata.contains_key(key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Bolt compatibility does not support {key} metadata in HELLO/LOGON"),
            ));
        }
    }
    Ok(())
}

fn ensure_only_supported_handshake_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    const SUPPORTED_KEYS: &[&str] = &[
        "auth",
        "session_auth",
        "imp_user",
        "impersonated_user",
        "db",
        "mode",
        "bookmarks",
        "tx_timeout",
        "tx_metadata",
        "notifications_minimum_severity",
        "notifications_disabled_classifications",
        "notifications_disabled_categories",
    ];
    for key in metadata.keys() {
        if !SUPPORTED_KEYS.contains(&key.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported Bolt metadata key \"{key}\" in HELLO/LOGON"),
            ));
        }
    }
    Ok(())
}

fn ensure_string_metadata(
    metadata: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<(), io::Error> {
    match metadata.get(key) {
        None | Some(JsonValue::String(_)) => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {key} metadata must be a string"),
        )),
    }
}

fn ensure_string_list_metadata(
    metadata: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<(), io::Error> {
    match metadata.get(key) {
        None => Ok(()),
        Some(JsonValue::Array(values)) if values.iter().all(JsonValue::is_string) => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {key} metadata must be a list of strings"),
        )),
    }
}

fn ensure_notification_filtering_metadata_types(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    ensure_string_metadata(metadata, "notifications_minimum_severity")?;
    ensure_string_list_metadata(metadata, "notifications_disabled_classifications")?;
    ensure_string_list_metadata(metadata, "notifications_disabled_categories")?;
    Ok(())
}

fn ensure_supported_access_mode(metadata: &JsonMap<String, JsonValue>) -> Result<(), io::Error> {
    ensure_string_metadata(metadata, "mode")?;
    let Some(mode) = metadata.get("mode").and_then(JsonValue::as_str) else {
        return Ok(());
    };
    match mode {
        "r" | "read" | "READ" => Ok(()),
        "w" | "write" | "WRITE" => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bolt compatibility is read-only and does not support write access mode",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Bolt access mode {mode:?}"),
        )),
    }
}

fn ensure_no_impersonation(metadata: &JsonMap<String, JsonValue>) -> Result<(), io::Error> {
    match metadata.get("imp_user") {
        Some(JsonValue::String(imp_user)) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Bolt compatibility does not support impersonation ({imp_user:?})"),
            ));
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Bolt imp_user metadata must be a string",
            ));
        }
        None => {}
    }
    match metadata.get("impersonated_user") {
        Some(JsonValue::String(imp_user)) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Bolt compatibility does not support impersonation ({imp_user:?})"),
            ));
        }
        Some(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Bolt impersonated_user metadata must be a string",
            ));
        }
        None => {}
    }
    Ok(())
}

fn ensure_supported_qid(qid: Option<i64>) -> Result<(), io::Error> {
    match qid {
        None | Some(-1) | Some(0) => Ok(()),
        Some(qid) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Bolt qid {qid}"),
        )),
    }
}

fn ensure_read_only_statement(sql: &str) -> Result<(), io::Error> {
    let statements = parse_sql(sql)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    if statements.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bolt compatibility only supports one read-only statement per RUN",
        ));
    }
    if !statement_is_read_only(&statements[0]) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bolt compatibility only supports read-only statements",
        ));
    }
    Ok(())
}

fn statement_is_read_only(statement: &Statement) -> bool {
    match statement {
        Statement::Select(_) | Statement::SetOperation(_) | Statement::Analyze { .. } => true,
        Statement::Explain { statement, .. } => statement_is_read_only(statement),
        Statement::Cypher(statement) => cypher_is_read_only(statement),
        _ => false,
    }
}

fn cypher_is_read_only(statement: &aiondb_parser::cypher_ast::CypherStatement) -> bool {
    statement.clauses.iter().all(cypher_clause_is_read_only)
        && statement
            .union
            .as_ref()
            .map(|union| cypher_is_read_only(&union.right))
            .unwrap_or(true)
}

fn cypher_clause_is_read_only(clause: &aiondb_parser::cypher_ast::CypherClause) -> bool {
    matches!(
        clause,
        aiondb_parser::cypher_ast::CypherClause::Match(_)
            | aiondb_parser::cypher_ast::CypherClause::Unwind(_)
            | aiondb_parser::cypher_ast::CypherClause::With(_)
            | aiondb_parser::cypher_ast::CypherClause::Return(_)
    )
}

fn build_pending_query(results: Vec<StatementResult>) -> Result<BoltPendingQuery, io::Error> {
    let mut query_result = None;
    for result in results {
        if let StatementResult::Query { columns, rows } = result {
            if query_result.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Bolt compatibility only supports one query result",
                ));
            }
            let fields = columns.into_iter().map(|column| column.name).collect::<Vec<_>>();
            let records = rows
                .into_iter()
                .map(|row| encode_record_message(&row.values))
                .collect::<Result<Vec<_>, _>>()?;
            let status = if records.is_empty() {
                BoltStatusKind::NoData
            } else {
                BoltStatusKind::Success
            };
            query_result = Some(BoltPendingQuery {
                fields,
                qid: 0,
                bookmark: None,
                result_available_after_ms: 0,
                status,
                summary: vec![("type", "r".to_owned())],
                records,
                next_record: 0,
            });
        }
    }
    query_result.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility expected a query result",
        )
    })
}

fn portal_batch_to_statement_results(
    batch: PortalBatch,
    notices: Vec<String>,
) -> Vec<StatementResult> {
    let mut out = Vec::with_capacity(1 + notices.len());
    if batch.columns.is_empty() && batch.rows.is_empty() {
        out.push(StatementResult::Command {
            tag: batch.tag,
            rows_affected: batch.rows_affected,
        });
    } else {
        out.push(StatementResult::Query {
            columns: batch.columns,
            rows: batch.rows,
        });
    }
    out.extend(
        notices
            .into_iter()
            .map(|message| StatementResult::Notice { message }),
    );
    out
}

fn infer_param_hints(params: &[aiondb_engine::Value]) -> Vec<Option<DataType>> {
    params.iter().map(infer_param_hint).collect()
}

fn infer_param_hint(value: &aiondb_engine::Value) -> Option<DataType> {
    match value {
        aiondb_engine::Value::Null => None,
        aiondb_engine::Value::Int(_) => Some(DataType::Int),
        aiondb_engine::Value::BigInt(_) => Some(DataType::BigInt),
        aiondb_engine::Value::Real(_) => Some(DataType::Real),
        aiondb_engine::Value::Double(_) => Some(DataType::Double),
        aiondb_engine::Value::Numeric(_) => Some(DataType::Numeric),
        aiondb_engine::Value::Money(_) => Some(DataType::Money),
        aiondb_engine::Value::Text(_) => Some(DataType::Text),
        aiondb_engine::Value::Boolean(_) => Some(DataType::Boolean),
        aiondb_engine::Value::Blob(_) => Some(DataType::Blob),
        aiondb_engine::Value::Timestamp(_) => Some(DataType::Timestamp),
        aiondb_engine::Value::Date(_) | aiondb_engine::Value::LargeDate(_) => Some(DataType::Date),
        aiondb_engine::Value::Time(_) => Some(DataType::Time),
        aiondb_engine::Value::TimeTz(_, _) => Some(DataType::TimeTz),
        aiondb_engine::Value::Interval(_) => Some(DataType::Interval),
        aiondb_engine::Value::Tid(_) => Some(DataType::Tid),
        aiondb_engine::Value::Uuid(_) => Some(DataType::Uuid),
        aiondb_engine::Value::TimestampTz(_) => Some(DataType::TimestampTz),
        aiondb_engine::Value::PgLsn(_) => Some(DataType::PgLsn),
        aiondb_engine::Value::Jsonb(_) => Some(DataType::Jsonb),
        aiondb_engine::Value::MacAddr(_) => Some(DataType::MacAddr),
        aiondb_engine::Value::MacAddr8(_) => Some(DataType::MacAddr8),
        aiondb_engine::Value::Vector(_vector) => None,
        aiondb_engine::Value::Array(values) => values
            .iter()
            .find_map(infer_param_hint)
            .map(|inner| DataType::Array(Box::new(inner))),
    }
}

fn encode_record_message(values: &[aiondb_engine::Value]) -> Result<Vec<u8>, io::Error> {
    let mut payload = vec![0xB1, BOLT_RECORD_SIGNATURE];
    encode_list_header_into(&mut payload, values.len());
    for value in values {
        encode_value_into(&mut payload, value)?;
    }
    Ok(payload)
}

fn decode_string_keyed_json_map<'a>(
    input: &mut &'a [u8],
) -> Result<JsonMap<String, JsonValue>, io::Error> {
    let map_len = decode_map_len(input)?;
    let mut map = JsonMap::with_capacity(map_len);
    for _ in 0..map_len {
        let key = decode_string(input)?;
        let value = decode_json_value(input, 0)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn decode_json_value<'a>(input: &mut &'a [u8], depth: usize) -> Result<JsonValue, io::Error> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt parameter nesting too deep",
        ));
    }
    let Some((&marker, rest)) = input.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof in bolt parameter value",
        ));
    };
    *input = rest;
    match marker {
        0xC0 => Ok(JsonValue::Null),
        0xC2 => Ok(JsonValue::Bool(false)),
        0xC3 => Ok(JsonValue::Bool(true)),
        0x00..=0x7F => Ok(JsonValue::from(i64::from(marker))),
        0xF0..=0xFF => Ok(JsonValue::from(i64::from(i8::from_be_bytes([marker])))),
        0xC8 => {
            let bytes = take_exact(input, 1)?;
            Ok(JsonValue::from(i64::from(i8::from_be_bytes([bytes[0]]))))
        }
        0xC9 => {
            let bytes = take_exact(input, 2)?;
            Ok(JsonValue::from(i64::from(i16::from_be_bytes([bytes[0], bytes[1]]))))
        }
        0xCA => {
            let bytes = take_exact(input, 4)?;
            Ok(JsonValue::from(i64::from(i32::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ]))))
        }
        0xCB => {
            let bytes = take_exact(input, 8)?;
            let value = i64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            Ok(JsonValue::from(value))
        }
        0xC1 => {
            let bytes = take_exact(input, 8)?;
            let value = f64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            serde_json::Number::from_f64(value)
                .map(JsonValue::Number)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-finite float"))
        }
        0x80..=0x8F | 0xD0 | 0xD1 | 0xD2 => {
            let len = decode_string_len_from_marker(marker, input)?;
            let bytes = take_exact(input, len)?;
            let value = std::str::from_utf8(bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "bolt string is not utf-8")
            })?;
            Ok(JsonValue::String(value.to_owned()))
        }
        0x90..=0x9F | 0xD4 | 0xD5 | 0xD6 => {
            let len = decode_list_len_from_marker(marker, input)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(decode_json_value(input, depth + 1)?);
            }
            Ok(JsonValue::Array(items))
        }
        0xA0..=0xAF | 0xD8 | 0xD9 | 0xDA => {
            let len = decode_map_len_from_marker(marker, input)?;
            let mut map = JsonMap::with_capacity(len);
            for _ in 0..len {
                let key = decode_string(input)?;
                let value = decode_json_value(input, depth + 1)?;
                map.insert(key, value);
            }
            Ok(JsonValue::Object(map))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported bolt parameter type",
        )),
    }
}

fn decode_string_len_from_marker<'a>(marker: u8, input: &mut &'a [u8]) -> Result<usize, io::Error> {
    match marker {
        0x80..=0x8F => Ok((marker & 0x0F) as usize),
        0xD0 => {
            let (len, tail) = decode_sized_len(*input, 1)?;
            *input = tail;
            Ok(len)
        }
        0xD1 => {
            let (len, tail) = decode_sized_len(*input, 2)?;
            *input = tail;
            Ok(len)
        }
        0xD2 => {
            let (len, tail) = decode_sized_len(*input, 4)?;
            *input = tail;
            Ok(len)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt parameter must be a string",
        )),
    }
}

fn decode_list_len_from_marker<'a>(marker: u8, input: &mut &'a [u8]) -> Result<usize, io::Error> {
    match marker {
        0x90..=0x9F => Ok((marker & 0x0F) as usize),
        0xD4 => {
            let (len, tail) = decode_sized_len(*input, 1)?;
            *input = tail;
            Ok(len)
        }
        0xD5 => {
            let (len, tail) = decode_sized_len(*input, 2)?;
            *input = tail;
            Ok(len)
        }
        0xD6 => {
            let (len, tail) = decode_sized_len(*input, 4)?;
            *input = tail;
            Ok(len)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt parameter must be a list",
        )),
    }
}

fn decode_map_len_from_marker<'a>(marker: u8, input: &mut &'a [u8]) -> Result<usize, io::Error> {
    match marker {
        0xA0..=0xAF => Ok((marker & 0x0F) as usize),
        0xD8 => {
            let (len, tail) = decode_sized_len(*input, 1)?;
            *input = tail;
            Ok(len)
        }
        0xD9 => {
            let (len, tail) = decode_sized_len(*input, 2)?;
            *input = tail;
            Ok(len)
        }
        0xDA => {
            let (len, tail) = decode_sized_len(*input, 4)?;
            *input = tail;
            Ok(len)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt parameter must be a map",
        )),
    }
}

fn encode_list_header_into(buf: &mut Vec<u8>, len: usize) {
    if len <= 15 {
        buf.push(0x90 | (len as u8));
    } else if u8::try_from(len).is_ok() {
        buf.push(0xD4);
        buf.push(len as u8);
    } else if u16::try_from(len).is_ok() {
        buf.push(0xD5);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xD6);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

fn encode_value_into(buf: &mut Vec<u8>, value: &aiondb_engine::Value) -> Result<(), io::Error> {
    encode_value_into_with_depth(buf, value, 0)
}

fn encode_value_into_with_depth(
    buf: &mut Vec<u8>,
    value: &aiondb_engine::Value,
    depth: usize,
) -> Result<(), io::Error> {
    if depth >= BOLT_MAX_ENCODE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt value nesting too deep",
        ));
    }
    match value {
        aiondb_engine::Value::Null => buf.push(0xC0),
        aiondb_engine::Value::Boolean(false) => buf.push(0xC2),
        aiondb_engine::Value::Boolean(true) => buf.push(0xC3),
        aiondb_engine::Value::Int(number) => encode_i64_into(buf, i64::from(*number)),
        aiondb_engine::Value::BigInt(number) => encode_i64_into(buf, *number),
        aiondb_engine::Value::Real(number) => {
            buf.push(0xC1);
            buf.extend_from_slice(&(f64::from(*number)).to_be_bytes());
        }
        aiondb_engine::Value::Double(number) => {
            buf.push(0xC1);
            buf.extend_from_slice(&number.to_be_bytes());
        }
        aiondb_engine::Value::Date(date) => encode_bolt_date(buf, *date),
        aiondb_engine::Value::Time(time) => encode_bolt_local_time(buf, *time),
        aiondb_engine::Value::TimeTz(time, offset) => encode_bolt_time(buf, *time, *offset),
        aiondb_engine::Value::Timestamp(datetime) => encode_bolt_local_datetime(buf, *datetime),
        aiondb_engine::Value::TimestampTz(datetime) => encode_bolt_datetime(buf, *datetime),
        aiondb_engine::Value::Interval(interval) => {
            encode_bolt_duration(buf, interval.months, interval.days, interval.micros)
        }
        aiondb_engine::Value::Jsonb(value) => encode_json_into(buf, value, depth + 1)?,
        aiondb_engine::Value::Array(values) => {
            encode_list_header_into(buf, values.len());
            for value in values {
                encode_value_into_with_depth(buf, value, depth + 1)?;
            }
        }
        aiondb_engine::Value::Text(text) => encode_string_into(buf, text),
        aiondb_engine::Value::Blob(bytes) => encode_bytes_into(buf, bytes),
        other => encode_string_into(buf, &other.to_string()),
    }
    Ok(())
}

fn encode_bolt_date(buf: &mut Vec<u8>, date: time::Date) {
    let days = time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp()
        / 86_400;
    encode_struct_header_into(buf, 1, b'D');
    encode_i64_into(buf, days);
}

fn encode_bolt_local_time(buf: &mut Vec<u8>, time: time::Time) {
    encode_struct_header_into(buf, 1, b't');
    encode_i64_into(buf, nanos_since_midnight(time));
}

fn encode_bolt_time(buf: &mut Vec<u8>, time: time::Time, offset: time::UtcOffset) {
    encode_struct_header_into(buf, 2, b'T');
    encode_i64_into(buf, nanos_since_midnight(time));
    encode_i64_into(buf, i64::from(offset.whole_seconds()));
}

fn encode_bolt_local_datetime(buf: &mut Vec<u8>, datetime: time::PrimitiveDateTime) {
    encode_struct_header_into(buf, 2, b'd');
    encode_i64_into(buf, datetime.assume_utc().unix_timestamp());
    encode_i64_into(buf, i64::from(datetime.nanosecond()));
}

fn encode_bolt_datetime(buf: &mut Vec<u8>, datetime: time::OffsetDateTime) {
    encode_struct_header_into(buf, 3, b'I');
    encode_i64_into(buf, datetime.unix_timestamp());
    encode_i64_into(buf, i64::from(datetime.nanosecond()));
    encode_i64_into(buf, i64::from(datetime.offset().whole_seconds()));
}

fn encode_bolt_duration(buf: &mut Vec<u8>, months: i32, days: i32, micros: i64) {
    let seconds = micros.div_euclid(MICROS_PER_SECOND);
    let nanos = micros.rem_euclid(MICROS_PER_SECOND) * 1_000;
    encode_struct_header_into(buf, 4, b'E');
    encode_i64_into(buf, i64::from(months));
    encode_i64_into(buf, i64::from(days));
    encode_i64_into(buf, seconds);
    encode_i64_into(buf, nanos);
}

fn nanos_since_midnight(time: time::Time) -> i64 {
    let hours = i64::from(time.hour());
    let minutes = i64::from(time.minute());
    let seconds = i64::from(time.second());
    let nanos = i64::from(time.nanosecond());
    ((hours * 60 + minutes) * 60 + seconds) * NANOS_PER_SECOND + nanos
}

fn encode_struct_header_into(buf: &mut Vec<u8>, len: usize, signature: u8) {
    debug_assert!(len <= 15);
    buf.push(0xB0 | (len as u8));
    buf.push(signature);
}

fn encode_json_into(
    buf: &mut Vec<u8>,
    value: &serde_json::Value,
    depth: usize,
) -> Result<(), io::Error> {
    if depth >= BOLT_MAX_ENCODE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bolt json nesting too deep",
        ));
    }
    match value {
        serde_json::Value::Null => buf.push(0xC0),
        serde_json::Value::Bool(false) => buf.push(0xC2),
        serde_json::Value::Bool(true) => buf.push(0xC3),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                encode_i64_into(buf, value);
            } else if let Some(value) = number.as_u64() {
                match i64::try_from(value) {
                    Ok(value) => encode_i64_into(buf, value),
                    Err(_) => encode_string_into(buf, &number.to_string()),
                }
            } else if let Some(value) = number.as_f64() {
                buf.push(0xC1);
                buf.extend_from_slice(&value.to_be_bytes());
            } else {
                encode_string_into(buf, &number.to_string());
            }
        }
        serde_json::Value::String(value) => encode_string_into(buf, value),
        serde_json::Value::Array(values) => {
            encode_list_header_into(buf, values.len());
            for value in values {
                encode_json_into(buf, value, depth + 1)?;
            }
        }
        serde_json::Value::Object(map) => {
            encode_map_header_into(buf, map.len());
            for (key, value) in map {
                encode_string_into(buf, key);
                encode_json_into(buf, value, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn encode_i64_into(buf: &mut Vec<u8>, value: i64) {
    if (-16..=127).contains(&value) {
        buf.push(value as i8 as u8);
    } else if (i8::MIN as i64..=i8::MAX as i64).contains(&value) {
        buf.push(0xC8);
        buf.push(value as i8 as u8);
    } else if (i16::MIN as i64..=i16::MAX as i64).contains(&value) {
        buf.push(0xC9);
        buf.extend_from_slice(&(value as i16).to_be_bytes());
    } else if (i32::MIN as i64..=i32::MAX as i64).contains(&value) {
        buf.push(0xCA);
        buf.extend_from_slice(&(value as i32).to_be_bytes());
    } else {
        buf.push(0xCB);
        buf.extend_from_slice(&value.to_be_bytes());
    }
}

fn encode_bytes_into(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len();
    if u8::try_from(len).is_ok() {
        buf.push(0xCC);
        buf.push(len as u8);
    } else if u16::try_from(len).is_ok() {
        buf.push(0xCD);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xCE);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(bytes);
}

async fn read_chunked_message<R>(reader: &mut R) -> Result<Vec<u8>, io::Error>
where
    R: AsyncReadExt + Unpin,
{
    let mut message = Vec::new();
    loop {
        let mut header = [0u8; 2];
        reader.read_exact(&mut header).await?;
        let chunk_len = u16::from_be_bytes(header) as usize;
        if chunk_len == 0 {
            break;
        }
        if message.len().saturating_add(chunk_len) > BOLT_MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt message exceeds size limit",
            ));
        }
        let start = message.len();
        message.resize(start + chunk_len, 0);
        reader.read_exact(&mut message[start..]).await?;
    }
    Ok(message)
}

async fn write_chunked_message<W>(writer: &mut W, payload: &[u8]) -> Result<(), io::Error>
where
    W: AsyncWriteExt + Unpin,
{
    let mut offset = 0usize;
    while offset < payload.len() {
        let chunk_len = (payload.len() - offset).min(u16::MAX as usize);
        writer
            .write_all(&(chunk_len as u16).to_be_bytes())
            .await?;
        writer
            .write_all(&payload[offset..offset + chunk_len])
            .await?;
        offset += chunk_len;
    }
    writer.write_all(&0u16.to_be_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_config::{RuntimeConfig, StorageBackend};

    fn test_engine() -> Arc<Engine> {
        let mut config = RuntimeConfig::default();
        crate::apply_server_security_baseline(&mut config, StorageBackend::InMemory);
        crate::build_server_engine(None, &config, StorageBackend::InMemory, false)
            .expect("build test engine")
    }

    fn assert_run_success_message(
        payload: &[u8],
        expected_fields: &[&str],
        expected_qid: i64,
        expected_db: &str,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("run success map");
        assert_eq!(map_len, 5);

        let mut fields = None;
        let mut qid = None;
        let mut db = None;
        let mut t_first = None;
        let mut result_available_after = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("run success key");
            match key.as_str() {
                "fields" => {
                    let len = decode_list_len(&mut remaining).expect("fields len");
                    let mut values = Vec::with_capacity(len);
                    for _ in 0..len {
                        values.push(decode_string(&mut remaining).expect("field"));
                    }
                    fields = Some(values);
                }
                "qid" => qid = decode_optional_i64(&mut remaining).expect("qid"),
                "db" => db = Some(decode_string(&mut remaining).expect("db")),
                "t_first" => t_first = decode_optional_i64(&mut remaining).expect("t_first"),
                "result_available_after" => {
                    result_available_after =
                        decode_optional_i64(&mut remaining).expect("result_available_after")
                }
                other => panic!("unexpected key {other}"),
            }
        }

        assert!(remaining.is_empty());
        assert_eq!(
            fields.as_deref(),
            Some(
                &expected_fields
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>()[..]
            )
        );
        assert_eq!(qid, Some(expected_qid));
        assert_eq!(db.as_deref(), Some(expected_db));
        assert!(t_first.unwrap_or(-1) >= 0);
        assert_eq!(t_first, result_available_after);
    }

    fn assert_hello_success_message(payload: &[u8]) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("hello success map");
        assert_eq!(map_len, 3);

        let mut server = None;
        let mut connection_id = None;
        let mut telemetry_enabled = None;
        let mut recv_timeout = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("hello success key");
            match key.as_str() {
                "server" => server = Some(decode_string(&mut remaining).expect("server")),
                "connection_id" => {
                    connection_id = Some(decode_string(&mut remaining).expect("connection_id"));
                }
                "hints" => {
                    let hints_len = decode_map_len(&mut remaining).expect("hello hints map");
                    assert_eq!(hints_len, 2);
                    for _ in 0..hints_len {
                        let hint_key = decode_string(&mut remaining).expect("hint key");
                        match hint_key.as_str() {
                            "telemetry.enabled" => {
                                let Some((&marker, rest)) = remaining.split_first() else {
                                    panic!("missing telemetry bool payload");
                                };
                                remaining = rest;
                                telemetry_enabled = Some(match marker {
                                    0xC3 => true,
                                    0xC2 => false,
                                    other => panic!("unexpected bool marker {other:#x}"),
                                });
                            }
                            "connection.recv_timeout_seconds" => {
                                recv_timeout =
                                    decode_optional_i64(&mut remaining).expect("recv timeout");
                            }
                            other => panic!("unexpected hint key {other}"),
                        }
                    }
                }
                other => panic!("unexpected key {other}"),
            }
        }

        assert!(remaining.is_empty());
        assert_eq!(server.as_deref(), Some("AionDB/bolt-compat"));
        let connection_id = connection_id.expect("connection_id");
        assert!(connection_id.starts_with(BOLT_APPLICATION_NAME));
        assert!(connection_id.contains(':'));
        assert_eq!(telemetry_enabled, Some(false));
        assert_eq!(recv_timeout, Some(BOLT_CONNECTION_RECV_TIMEOUT_SECONDS));
    }

    fn assert_pull_success_message(
        payload: &[u8],
        expected_pairs: &[(&str, &str)],
        expected_has_more: bool,
        expected_status: BoltStatusKind,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_SUCCESS_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("pull success map");

        let mut values = JsonMap::new();
        let mut saw_stats = false;
        let mut saw_statuses = false;
        let mut qid = None;
        let mut t_last = None;
        let mut result_consumed_after = None;

        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("pull success key");
            match key.as_str() {
                "has_more" => {
                    let Some((&marker, rest)) = remaining.split_first() else {
                        panic!("missing bool payload for has_more");
                    };
                    remaining = rest;
                    values.insert(
                        key,
                        JsonValue::Bool(match marker {
                            0xC3 => true,
                            0xC2 => false,
                            other => panic!("unexpected bool marker {other:#x}"),
                        }),
                    );
                }
                "stats" => {
                    let stats_len = decode_map_len(&mut remaining).expect("stats map");
                    assert_eq!(stats_len, 14);
                    let mut saw_zero_keys = std::collections::BTreeSet::new();
                    let mut saw_false_keys = std::collections::BTreeSet::new();
                    for _ in 0..stats_len {
                        let stats_key = decode_string(&mut remaining).expect("stats key");
                        match stats_key.as_str() {
                            "nodes-created"
                            | "nodes-deleted"
                            | "relationships-created"
                            | "relationships-deleted"
                            | "properties-set"
                            | "labels-added"
                            | "labels-removed"
                            | "indexes-added"
                            | "indexes-removed"
                            | "constraints-added"
                            | "constraints-removed"
                            | "system-updates" => {
                                assert_eq!(
                                    decode_optional_i64(&mut remaining).expect("zero stat"),
                                    Some(0)
                                );
                                saw_zero_keys.insert(stats_key);
                            }
                            "contains-updates" | "contains-system-updates" => {
                                let Some((&marker, rest)) = remaining.split_first() else {
                                    panic!("missing boolean stat payload");
                                };
                                remaining = rest;
                                assert_eq!(marker, 0xC2);
                                saw_false_keys.insert(stats_key);
                            }
                            other => panic!("unexpected stats key {other}"),
                        }
                    }
                    assert_eq!(
                        saw_zero_keys,
                        [
                            "nodes-created",
                            "nodes-deleted",
                            "relationships-created",
                            "relationships-deleted",
                            "properties-set",
                            "labels-added",
                            "labels-removed",
                            "indexes-added",
                            "indexes-removed",
                            "constraints-added",
                            "constraints-removed",
                            "system-updates",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<std::collections::BTreeSet<_>>()
                    );
                    assert_eq!(
                        saw_false_keys,
                        ["contains-updates", "contains-system-updates"]
                            .into_iter()
                            .map(str::to_owned)
                            .collect::<std::collections::BTreeSet<_>>()
                    );
                    saw_stats = true;
                }
                "statuses" => {
                    let statuses_len = decode_list_len(&mut remaining).expect("statuses len");
                    assert_eq!(statuses_len, 1);
                    let status_len = decode_map_len(&mut remaining).expect("status map");
                    assert_eq!(status_len, 5);
                    let mut gql_status = None;
                    let mut status_description = None;
                    let mut title = None;
                    let mut description = None;
                    let mut saw_diagnostic_record = false;
                    for _ in 0..status_len {
                        let status_key = decode_string(&mut remaining).expect("status key");
                        match status_key.as_str() {
                            "gql_status" => {
                                gql_status = Some(decode_string(&mut remaining).expect("gql_status"));
                            }
                            "status_description" => {
                                status_description = Some(
                                    decode_string(&mut remaining).expect("status_description"),
                                );
                            }
                            "title" => {
                                title = Some(decode_string(&mut remaining).expect("title"));
                            }
                            "description" => {
                                description =
                                    Some(decode_string(&mut remaining).expect("description"));
                            }
                            "diagnostic_record" => {
                                let diagnostic_len =
                                    decode_map_len(&mut remaining).expect("diagnostic record");
                                assert_eq!(diagnostic_len, 3);
                                for _ in 0..diagnostic_len {
                                    let _ = decode_string(&mut remaining).expect("diag key");
                                    let _ = decode_string(&mut remaining).expect("diag value");
                                }
                                saw_diagnostic_record = true;
                            }
                            other => panic!("unexpected status key {other}"),
                        }
                    }
                    assert_eq!(
                        gql_status.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "00000",
                            BoltStatusKind::OmittedResult => "00001",
                            BoltStatusKind::NoData => "02000",
                        })
                    );
                    assert_eq!(
                        status_description.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "note: successful completion",
                            BoltStatusKind::OmittedResult => {
                                "note: successful completion - omitted result"
                            }
                            BoltStatusKind::NoData => "note: no data",
                        })
                    );
                    assert_eq!(
                        title.as_deref(),
                        Some(match expected_status {
                            BoltStatusKind::Success => "Successful completion",
                            BoltStatusKind::OmittedResult => {
                                "Successful completion - omitted result"
                            }
                            BoltStatusKind::NoData => "No data",
                        })
                    );
                    assert_eq!(description, status_description);
                    assert!(saw_diagnostic_record);
                    saw_statuses = true;
                }
                "qid" => qid = decode_optional_i64(&mut remaining).expect("qid"),
                "t_last" => t_last = decode_optional_i64(&mut remaining).expect("t_last"),
                "result_consumed_after" => {
                    result_consumed_after =
                        decode_optional_i64(&mut remaining).expect("result_consumed_after")
                }
                _ => {
                    let value = decode_string(&mut remaining).expect("string summary value");
                    values.insert(key, JsonValue::String(value));
                }
            }
        }

        assert!(remaining.is_empty());
        for (key, value) in expected_pairs {
            assert_eq!(values.get(*key), Some(&JsonValue::String((*value).to_owned())));
        }
        assert_eq!(
            values.get("has_more"),
            if expected_has_more {
                Some(&JsonValue::Bool(true))
            } else {
                None
            }
        );
        assert!(saw_stats);
        assert!(saw_statuses);
        assert_eq!(qid, Some(0));
        assert!(t_last.unwrap_or(-1) >= 0);
        assert_eq!(t_last, result_consumed_after);
    }

    fn assert_failure_message(
        payload: &[u8],
        expected_code: &str,
        expected_gql_status: &str,
        expected_description: &str,
    ) {
        assert!(payload.len() >= 2);
        assert_eq!(payload[0], 0xB1);
        assert_eq!(payload[1], BOLT_FAILURE_SIGNATURE);
        let mut remaining = &payload[2..];
        let map_len = decode_map_len(&mut remaining).expect("failure map");
        assert_eq!(map_len, 6);
        let mut code = None;
        let mut neo4j_code = None;
        let mut message = None;
        let mut gql_status = None;
        let mut description = None;
        let mut saw_diagnostic_record = false;
        for _ in 0..map_len {
            let key = decode_string(&mut remaining).expect("failure key");
            match key.as_str() {
                "code" => code = Some(decode_string(&mut remaining).expect("code")),
                "neo4j_code" => {
                    neo4j_code = Some(decode_string(&mut remaining).expect("neo4j_code"))
                }
                "message" => message = Some(decode_string(&mut remaining).expect("message")),
                "gql_status" => {
                    gql_status = Some(decode_string(&mut remaining).expect("gql_status"))
                }
                "description" => {
                    description = Some(decode_string(&mut remaining).expect("description"))
                }
                "diagnostic_record" => {
                    let diagnostic_len = decode_map_len(&mut remaining).expect("diagnostic map");
                    assert_eq!(diagnostic_len, 0);
                    saw_diagnostic_record = true;
                }
                other => panic!("unexpected failure key {other}"),
            }
        }
        assert_eq!(code.as_deref(), Some(expected_code));
        assert_eq!(neo4j_code.as_deref(), Some(expected_code));
        assert!(message.is_some());
        assert_eq!(gql_status.as_deref(), Some(expected_gql_status));
        assert_eq!(description.as_deref(), Some(expected_description));
        assert!(saw_diagnostic_record);
        assert!(remaining.is_empty());
    }

    fn decode_list_len<'a>(input: &mut &'a [u8]) -> Result<usize, io::Error> {
        let Some((&marker, rest)) = input.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected eof in bolt list",
            ));
        };
        *input = rest;
        match marker {
            0x90..=0x9F => Ok((marker & 0x0F) as usize),
            0xD4 => {
                let (len, tail) = decode_sized_len(*input, 1)?;
                *input = tail;
                Ok(len)
            }
            0xD5 => {
                let (len, tail) = decode_sized_len(*input, 2)?;
                *input = tail;
                Ok(len)
            }
            0xD6 => {
                let (len, tail) = decode_sized_len(*input, 4)?;
                *input = tail;
                Ok(len)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bolt value must be a list",
            )),
        }
    }

    #[test]
    fn negotiate_exact_version_match() {
        let offered = [0x0000_0105, 0, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0105));
    }

    #[test]
    fn negotiate_minor_range_match() {
        let offered = [0x0003_0404, 0, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0404));
    }

    #[test]
    fn negotiate_manifest_is_not_supported_yet() {
        let offered = [BOLT_MANIFEST_V1, 0x0000_0105, 0, 0];
        assert_eq!(negotiate_version(&offered), Some(0x0000_0105));
    }

    #[tokio::test]
    async fn bolt_handshake_returns_selected_version() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine).await.expect("handshake");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0105u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0105);
        client
            .write_all(&0u16.to_be_bytes())
            .await
            .expect("write empty message");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn chunked_message_roundtrip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let payload = read_chunked_message(&mut stream).await.expect("read payload");
            assert_eq!(payload, b"hello bolt");
            write_chunked_message(&mut stream, b"ack").await.expect("write ack");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        write_chunked_message(&mut client, b"hello bolt")
            .await
            .expect("write payload");
        let ack = read_chunked_message(&mut client).await.expect("read ack");
        assert_eq!(ack, b"ack");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn chunked_message_rejects_oversize_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let err = read_chunked_message(&mut stream)
                .await
                .expect_err("oversize must fail");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let chunk = vec![0u8; u16::MAX as usize];
        let mut sent = 0usize;
        while sent <= BOLT_MAX_MESSAGE_BYTES {
            client
                .write_all(&(chunk.len() as u16).to_be_bytes())
                .await
                .expect("write header");
            client.write_all(&chunk).await.expect("write chunk");
            sent += chunk.len();
        }
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_without_auth_returns_success_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        write_chunked_message(&mut client, &[0xB1, BOLT_HELLO_SIGNATURE, 0xA0])
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&response);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_without_auth_returns_failure_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        write_chunked_message(&mut client, &[0xB1, BOLT_HELLO_SIGNATURE, 0xA0])
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        write_chunked_message(&mut client, &[0xB1, BOLT_LOGON_SIGNATURE, 0xA0])
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        let expected = encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required");
        assert_eq!(response, expected);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_with_basic_auth_returns_success_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt hello with auth");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&response);
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_with_invalid_basic_auth_returns_failure_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "wrong");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_failure_message(
            &response,
            BOLT_AUTH_FAILURE_CODE,
            "28000",
            "error: invalid authorization specification",
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_unsupported_auth_scheme() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "bearer");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "token");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt compatibility only supports auth scheme \"basic\", got \"bearer\"",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_incomplete_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 3);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt authentication metadata must include both principal and credentials",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_auth_without_scheme() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 3);
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_AUTH_FAILURE_CODE,
                "Bolt authentication metadata must include scheme",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_failure_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 3);
        encode_string_into(&mut logon, "scheme");
        encode_string_into(&mut logon, "basic");
        encode_string_into(&mut logon, "principal");
        encode_string_into(&mut logon, "admin");
        encode_string_into(&mut logon, "credentials");
        encode_string_into(&mut logon, "wrong");
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(response[1], BOLT_FAILURE_SIGNATURE);

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_missing_auth_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let logon = vec![0xB1, BOLT_LOGON_SIGNATURE, 0xA0];
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_invalid_metadata_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 4);
        encode_string_into(&mut logon, "scheme");
        encode_string_into(&mut logon, "basic");
        encode_string_into(&mut logon, "principal");
        encode_string_into(&mut logon, "admin");
        encode_string_into(&mut logon, "credentials");
        encode_string_into(&mut logon, "StrongPass123!");
        encode_string_into(&mut logon, "notifications_minimum_severity");
        encode_i64_into(&mut logon, 42);
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logon_malformed_message_clears_existing_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logon = vec![0xB1, BOLT_LOGON_SIGNATURE];
        encode_map_header_into(&mut logon, 1);
        encode_string_into(&mut logon, "principal");
        encode_i64_into(&mut logon, 42);
        write_chunked_message(&mut client, &logon)
            .await
            .expect("write logon");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logon response");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "invalid LOGON message")
        );

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            run_response,
            encode_failure_message(BOLT_AUTH_FAILURE_CODE, "authentication required")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_logoff_rejects_non_empty_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut logoff = vec![0xB1, BOLT_LOGOFF_SIGNATURE];
        encode_map_header_into(&mut logoff, 0);
        write_chunked_message(&mut client, &logoff)
            .await
            .expect("write logoff");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read logoff response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt LOGOFF must be a zero-field struct",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_reset_clears_pending_query_inside_transaction() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut begin = vec![0xB1, BOLT_BEGIN_SIGNATURE];
        encode_map_header_into(&mut begin, 0);
        write_chunked_message(&mut client, &begin)
            .await
            .expect("write begin");
        let begin_response = read_chunked_message(&mut client)
            .await
            .expect("read begin response");
        assert_eq!(begin_response, encode_success_message(&[]));

        let reset = vec![0xB0, BOLT_RESET_SIGNATURE];
        write_chunked_message(&mut client, &reset)
            .await
            .expect("write reset");
        let reset_response = read_chunked_message(&mut client)
            .await
            .expect("read reset response");
        assert_eq!(reset_response, encode_success_message(&[]));

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let pull_response = read_chunked_message(&mut client)
            .await
            .expect("read pull response");
        assert_eq!(
            pull_response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "no pending result to pull")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_non_string_notification_severity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "notifications_minimum_severity");
        encode_i64_into(&mut hello, 42);
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_session_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "session_auth");
        encode_map_header_into(&mut hello, 0);
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support session_auth metadata",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_db_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 1);
        encode_string_into(&mut hello, "db");
        encode_string_into(&mut hello, "default");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support db metadata in HELLO/LOGON",
            )
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_hello_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = handle_bolt_connection(stream, engine).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 2);
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        encode_string_into(&mut hello, "foo");
        encode_string_into(&mut hello, "bar");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt metadata key \"foo\" in HELLO/LOGON",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_telemetry_message_returns_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt telemetry");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut telemetry = vec![0xB1, BOLT_TELEMETRY_SIGNATURE];
        encode_i64_into(&mut telemetry, 0);
        write_chunked_message(&mut client, &telemetry)
            .await
            .expect("write telemetry");
        let telemetry_response = read_chunked_message(&mut client)
            .await
            .expect("read telemetry response");
        assert_eq!(telemetry_response, encode_success_message(&[]));

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_pull_returns_query_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run pull");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            false,
            BoltStatusKind::Success,
        );
        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_no_data_returns_no_data_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt no data");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n WHERE FALSE");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            false,
            BoltStatusKind::NoData,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_discard_returns_omitted_result_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt discard");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");

        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let hello_response = read_chunked_message(&mut client)
            .await
            .expect("read hello response");
        assert_hello_success_message(&hello_response);

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run success");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut discard = vec![0xB1, BOLT_DISCARD_SIGNATURE];
        encode_map_header_into(&mut discard, 0);
        write_chunked_message(&mut client, &discard)
            .await
            .expect("write discard");
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read discard summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::OmittedResult,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_n_pages_records() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt paged pull");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n UNION ALL SELECT 2 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "n");
        encode_i64_into(&mut pull, 1);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull 1");
        let record1 = read_chunked_message(&mut client)
            .await
            .expect("read record1");
        assert_eq!(record1, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary1 = read_chunked_message(&mut client)
            .await
            .expect("read summary1");
        assert_pull_success_message(
            &summary1,
            &[("type", "r"), ("db", BOLT_DEFAULT_DATABASE)],
            true,
            BoltStatusKind::Success,
        );

        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull 2");
        let record2 = read_chunked_message(&mut client)
            .await
            .expect("read record2");
        assert_eq!(record2, encode_record_message(&[aiondb_engine::Value::Int(2)]).unwrap());
        let summary2 = read_chunked_message(&mut client)
            .await
            .expect("read summary2");
        assert_pull_success_message(
            &summary2,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_with_named_parameter_returns_record() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run with param");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT $n AS n");
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "n");
        encode_i64_into(&mut run, 7);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let run_response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_run_success_message(&run_response, &["n"], 0, BOLT_DEFAULT_DATABASE);

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(7)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_autocommit_pull_returns_bookmark() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt autocommit bookmark");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(1)]).unwrap());
        let summary = read_chunked_message(&mut client)
            .await
            .expect("read summary");
        assert_pull_success_message(
            &summary,
            &[
                ("type", "r"),
                ("db", BOLT_DEFAULT_DATABASE),
                ("bookmark", "aiondb:bookmark:1"),
            ],
            false,
            BoltStatusKind::Success,
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_impersonation_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run impersonation");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "imp_user");
        encode_string_into(&mut run, "alice");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_FORBIDDEN_FAILURE_CODE,
                "Bolt compatibility does not support impersonation (\"alice\")",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_returns_single_node_routing_table() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let route_response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            route_response,
            encode_route_success_message(&addr.to_string(), "default")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_invalid_bookmarks_field() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject bookmarks field");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_string_into(&mut route, "not-a-list");
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "bolt value must be a list",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_invalid_routing_context_field() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject routing context field");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_string_into(&mut route, "not-a-map");
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "default");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "bolt hello metadata must be a map",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_route_rejects_non_default_database() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt route reject db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut route = vec![0xB3, BOLT_ROUTE_SIGNATURE];
        encode_map_header_into(&mut route, 0);
        encode_list_header_into(&mut route, 0);
        encode_map_header_into(&mut route, 1);
        encode_string_into(&mut route, "db");
        encode_string_into(&mut route, "other");
        write_chunked_message(&mut client, &route)
            .await
            .expect("write route");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read route response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_DATABASE_FAILURE_CODE,
                "Bolt compatibility only supports database \"default\", got \"other\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_begin_run_commit_returns_record() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt begin run commit");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut begin = vec![0xB1, BOLT_BEGIN_SIGNATURE];
        encode_map_header_into(&mut begin, 0);
        write_chunked_message(&mut client, &begin)
            .await
            .expect("write begin");
        let begin_response = read_chunked_message(&mut client)
            .await
            .expect("read begin response");
        assert_eq!(begin_response, encode_success_message(&[]));

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 3 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 0);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let record = read_chunked_message(&mut client)
            .await
            .expect("read record");
        assert_eq!(record, encode_record_message(&[aiondb_engine::Value::Int(3)]).unwrap());
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read summary");

        let commit = vec![0xB0, BOLT_COMMIT_SIGNATURE];
        write_chunked_message(&mut client, &commit)
            .await
            .expect("write commit");
        let commit_response = read_chunked_message(&mut client)
            .await
            .expect("read commit response");
        assert_eq!(
            commit_response,
            encode_success_message(&[
                ("bookmark", "aiondb:bookmark:1"),
                ("db", BOLT_DEFAULT_DATABASE),
            ])
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_default_database() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "db");
        encode_string_into(&mut run, "other");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_DATABASE_FAILURE_CODE,
                "Bolt compatibility only supports database \"default\", got \"other\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_database_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject non-string db");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "db");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "Bolt db metadata must be a string")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_notification_severity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject notification severity");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "notifications_minimum_severity");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt notifications_minimum_severity metadata must be a string",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_invalid_bookmarks_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject bookmarks");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "bookmarks");
        encode_string_into(&mut run, "not-a-list");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt bookmarks metadata must be a list of strings",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_tx_timeout_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject tx_timeout");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "tx_timeout");
        encode_i64_into(&mut run, 1000);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support tx_timeout metadata",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_auth_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject auth metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "auth");
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt compatibility does not support auth metadata",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_write_access_mode() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject write mode");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "mode");
        encode_string_into(&mut run, "w");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_FORBIDDEN_FAILURE_CODE,
                "Bolt compatibility is read-only and does not support write access mode",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_non_string_impersonation_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject non-string impersonation metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "imp_user");
        encode_i64_into(&mut run, 42);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "Bolt imp_user metadata must be a string",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_run_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt run reject unknown metadata key");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 1);
        encode_string_into(&mut run, "foo");
        encode_string_into(&mut run, "bar");
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read run response");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt metadata key \"foo\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_rejects_unsupported_qid() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt pull reject qid");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run response");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "qid");
        encode_i64_into(&mut pull, 42);
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read failure");
        assert_eq!(
            response,
            encode_failure_message(BOLT_REQUEST_FAILURE_CODE, "unsupported Bolt qid 42")
        );

        drop(client);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn bolt_pull_rejects_unknown_metadata_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let engine = test_engine();
        engine
            .bootstrap_role("admin", "StrongPass123!", true)
            .expect("bootstrap admin");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            handle_bolt_connection(stream, engine)
                .await
                .expect("bolt pull reject unknown metadata");
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");
        let mut request = Vec::from(BOLT_MAGIC);
        request.extend_from_slice(&0x0000_0404u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        client.write_all(&request).await.expect("write handshake");
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.expect("read reply");
        assert_eq!(u32::from_be_bytes(reply), 0x0000_0404);

        let mut hello = vec![0xB1, BOLT_HELLO_SIGNATURE];
        encode_map_header_into(&mut hello, 4);
        encode_string_into(&mut hello, "scheme");
        encode_string_into(&mut hello, "basic");
        encode_string_into(&mut hello, "principal");
        encode_string_into(&mut hello, "admin");
        encode_string_into(&mut hello, "credentials");
        encode_string_into(&mut hello, "StrongPass123!");
        encode_string_into(&mut hello, "user_agent");
        encode_string_into(&mut hello, "neo4j/test");
        write_chunked_message(&mut client, &hello)
            .await
            .expect("write hello");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read hello response");

        let mut run = vec![0xB3, BOLT_RUN_SIGNATURE];
        encode_string_into(&mut run, "SELECT 1 AS n");
        encode_map_header_into(&mut run, 0);
        encode_map_header_into(&mut run, 0);
        write_chunked_message(&mut client, &run)
            .await
            .expect("write run");
        let _ = read_chunked_message(&mut client)
            .await
            .expect("read run success");

        let mut pull = vec![0xB1, BOLT_PULL_SIGNATURE];
        encode_map_header_into(&mut pull, 1);
        encode_string_into(&mut pull, "foo");
        encode_string_into(&mut pull, "bar");
        write_chunked_message(&mut client, &pull)
            .await
            .expect("write pull");
        let response = read_chunked_message(&mut client)
            .await
            .expect("read pull failure");
        assert_eq!(
            response,
            encode_failure_message(
                BOLT_REQUEST_FAILURE_CODE,
                "unsupported Bolt stream control metadata key \"foo\"",
            )
        );

        drop(client);
        server.await.expect("server join");
    }

    #[test]
    fn encode_record_message_supports_arrays() {
        let encoded = encode_record_message(&[aiondb_engine::Value::Array(vec![
            aiondb_engine::Value::Int(1),
            aiondb_engine::Value::Text("x".to_owned()),
        ])])
        .expect("encode array");
        assert_eq!(encoded, vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91, 0x92, 0x01, 0x81, b'x']);
    }

    #[test]
    fn encode_record_message_supports_jsonb_objects() {
        let encoded = encode_record_message(&[aiondb_engine::Value::Jsonb(serde_json::json!({
            "a": 1,
            "b": [true, null]
        }))])
        .expect("encode jsonb");
        assert_eq!(
            encoded,
            vec![
                0xB1,
                BOLT_RECORD_SIGNATURE,
                0x91,
                0xA2,
                0x81,
                b'a',
                0x01,
                0x81,
                b'b',
                0x92,
                0xC3,
                0xC0,
            ]
        );
    }

    #[test]
    fn encode_record_message_supports_bolt_date() {
        let date = time::Date::from_calendar_date(1970, time::Month::January, 2)
            .expect("date");
        let encoded =
            encode_record_message(&[aiondb_engine::Value::Date(date)]).expect("encode date");
        assert_eq!(encoded, vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91, 0xB1, b'D', 0x01]);
    }

    #[test]
    fn encode_record_message_supports_bolt_duration() {
        let mut encoded = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_bolt_duration(&mut encoded, 14, 3, 4_500_001);
        assert_eq!(
            encoded,
            vec![
                0xB1,
                BOLT_RECORD_SIGNATURE,
                0x91,
                0xB4,
                b'E',
                0x0E,
                0x03,
                0x04,
                0xCA,
                0x1D,
                0xCD,
                0x68,
                0xE8,
            ]
        );
    }

    #[test]
    fn encode_record_message_supports_bolt_time_with_offset() {
        let time = time::Time::from_hms_micro(12, 34, 56, 123_456).expect("time");
        let offset = time::UtcOffset::from_whole_seconds(3600).expect("offset");
        let mut expected = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_struct_header_into(&mut expected, 2, b'T');
        encode_i64_into(&mut expected, 45_296_123_456_000);
        encode_i64_into(&mut expected, 3600);
        let encoded =
            encode_record_message(&[aiondb_engine::Value::TimeTz(time, offset)]).expect("encode timetz");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn encode_record_message_supports_bolt_datetime_with_offset() {
        let date =
            time::Date::from_calendar_date(1970, time::Month::January, 2).expect("date");
        let time = time::Time::from_hms_micro(3, 4, 5, 123_456).expect("time");
        let offset = time::UtcOffset::from_whole_seconds(3600).expect("offset");
        let datetime = time::PrimitiveDateTime::new(date, time).assume_offset(offset);
        let mut expected = vec![0xB1, BOLT_RECORD_SIGNATURE, 0x91];
        encode_struct_header_into(&mut expected, 3, b'I');
        encode_i64_into(&mut expected, 93_845);
        encode_i64_into(&mut expected, 123_456_000);
        encode_i64_into(&mut expected, 3600);
        let encoded = encode_record_message(&[aiondb_engine::Value::TimestampTz(datetime)])
            .expect("encode timestamptz");
        assert_eq!(encoded, expected);
    }
}
