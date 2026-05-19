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
const BOLT_SERVER_AGENT: &str = "Neo4j/5.26.0";
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
    stats: BoltQueryStats,
    summary: Vec<(&'static str, String)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoltStatusKind {
    Success,
    OmittedResult,
    NoData,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BoltQueryStats {
    contains_updates: bool,
    contains_system_updates: bool,
}

struct BoltRunMessage {
    statement: String,
    params: JsonMap<String, JsonValue>,
    database: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompatProcedure {
    DbPing,
    DbmsComponents,
    DbLabels,
    DbRelationshipTypes,
    DbPropertyKeys,
}

struct CompatGraphMetadata {
    node_labels: Vec<String>,
    relationship_types: Vec<String>,
    property_keys: Vec<String>,
}

struct CompatProcedureQuery {
    procedure: CompatProcedure,
    projection: Vec<(String, String)>,
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
            if let Err(err) = ensure_supported_transaction_metadata(&route_metadata) {
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
            if let Err(err) = ensure_supported_transaction_metadata(&begin_metadata) {
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
                    &pending.stats,
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
            let default_stats = BoltQueryStats::default();
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
                    pending
                        .as_ref()
                        .map(|pending| &pending.stats)
                        .unwrap_or(&default_stats),
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
    ensure_string_map_metadata(&hello.extra_metadata, "bolt_agent")?;
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
    encode_string_into(&mut payload, BOLT_SERVER_AGENT);
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
    stats: &BoltQueryStats,
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
    encode_stats_into(&mut payload, stats);
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

fn encode_stats_into(buf: &mut Vec<u8>, stats: &BoltQueryStats) {
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
    encode_string_into(buf, "contains-updates");
    buf.push(if stats.contains_updates { 0xC3 } else { 0xC2 });
    encode_string_into(buf, "contains-system-updates");
    buf.push(if stats.contains_system_updates { 0xC3 } else { 0xC2 });
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
    for role in ["ROUTE", "READ", "WRITE"] {
        encode_map_header_into(&mut payload, 2);
        encode_string_into(&mut payload, "role");
        encode_string_into(&mut payload, role);
        encode_string_into(&mut payload, "addresses");
        encode_list_header_into(&mut payload, 1);
        encode_string_into(&mut payload, address);
    }
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
    ensure_supported_transaction_metadata(&extra_metadata)?;
    ensure_no_unsupported_session_auth_metadata(&extra_metadata)?;
    ensure_string_metadata(&extra_metadata, "db")?;
    ensure_notification_filtering_metadata_types(&extra_metadata)?;
    ensure_supported_access_mode(&extra_metadata)?;
    ensure_supported_database(database.as_deref())?;
    let (statement, params) = rewrite_named_parameters(statement, &params)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
    if let Some(procedure) = compat_procedure_for_sql(&statement) {
        return compat_procedure_pending_query(engine, session, procedure);
    }
    ensure_supported_run_statement(&statement)?;
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

fn ensure_supported_transaction_metadata(
    metadata: &JsonMap<String, JsonValue>,
) -> Result<(), io::Error> {
    ensure_integer_metadata(metadata, "tx_timeout")?;
    ensure_json_object_metadata(metadata, "tx_metadata")?;
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
        "bolt_agent",
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

fn ensure_string_map_metadata(
    metadata: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<(), io::Error> {
    match metadata.get(key) {
        None => Ok(()),
        Some(JsonValue::Object(values)) if values.values().all(JsonValue::is_string) => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {key} metadata must be a map of strings"),
        )),
    }
}

fn ensure_integer_metadata(
    metadata: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<(), io::Error> {
    match metadata.get(key) {
        None => Ok(()),
        Some(JsonValue::Number(value)) if value.as_i64().is_some() => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {key} metadata must be an integer"),
        )),
    }
}

fn ensure_json_object_metadata(
    metadata: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<(), io::Error> {
    match metadata.get(key) {
        None => Ok(()),
        Some(JsonValue::Object(_)) => Ok(()),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Bolt {key} metadata must be a map"),
        )),
    }
}

fn ensure_supported_access_mode(metadata: &JsonMap<String, JsonValue>) -> Result<(), io::Error> {
    ensure_string_metadata(metadata, "mode")?;
    let Some(mode) = metadata.get("mode").and_then(JsonValue::as_str) else {
        return Ok(());
    };
    match mode {
        "r" | "read" | "READ" | "w" | "write" | "WRITE" => Ok(()),
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

fn ensure_supported_run_statement(sql: &str) -> Result<(), io::Error> {
    if compat_procedure_for_sql(sql).is_some() {
        return Ok(());
    }
    let statements = parse_sql(sql)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    if statements.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bolt compatibility only supports one statement per RUN",
        ));
    }
    if !statement_is_bolt_run_supported(&statements[0]) {
        let statement_kind = preview_debug(&statements[0]);
        let statement_preview = preview_statement(sql);
        tracing::warn!(
            statement_kind = %statement_kind,
            statement_sql = %statement_preview,
            "bolt compatibility rejected unsupported RUN statement",
        );
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Bolt compatibility does not support transaction-control or copy statements in RUN (kind={statement_kind}, sql={statement_preview:?})"
            ),
        ));
    }
    Ok(())
}

fn compat_ping_pending_query() -> BoltPendingQuery {
    BoltPendingQuery {
        fields: Vec::new(),
        records: Vec::new(),
        next_record: 0,
        qid: 0,
        bookmark: None,
        result_available_after_ms: 0,
        status: BoltStatusKind::NoData,
        stats: BoltQueryStats::default(),
        summary: vec![("type", "r".to_owned())],
    }
}

fn compat_procedure_pending_query(
    engine: &Arc<Engine>,
    session: &SessionHandle,
    query: CompatProcedureQuery,
) -> Result<BoltPendingQuery, io::Error> {
    match query.procedure {
        CompatProcedure::DbPing => Ok(compat_ping_pending_query()),
        procedure => {
            let graph_metadata = compat_graph_metadata(engine, session)?;
            build_projected_compat_query(&query.projection, compat_procedure_rows(procedure, &graph_metadata))
        }
    }
}

fn build_projected_compat_query(
    projection: &[(String, String)],
    rows: Vec<Vec<(String, aiondb_engine::Value)>>,
) -> Result<BoltPendingQuery, io::Error> {
    let fields = projection
        .iter()
        .map(|(_, output)| output.clone())
        .collect::<Vec<_>>();
    let records = rows
        .into_iter()
        .map(|row| {
            let values = projection
                .iter()
                .map(|(source, _)| {
                    row.iter()
                        .find(|(name, _)| name == source)
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("compat procedure is missing projected field {source:?}"),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            encode_record_message(&values)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let status = if records.is_empty() {
        BoltStatusKind::NoData
    } else {
        BoltStatusKind::Success
    };
    Ok(BoltPendingQuery {
        fields,
        records,
        next_record: 0,
        qid: 0,
        bookmark: None,
        result_available_after_ms: 0,
        status,
        stats: BoltQueryStats::default(),
        summary: vec![("type", "r".to_owned())],
    })
}

fn compat_graph_metadata(
    engine: &Arc<Engine>,
    session: &SessionHandle,
) -> Result<CompatGraphMetadata, io::Error> {
    let (node_labels, relationship_types, property_keys) = engine
        .bolt_graph_compat_snapshot(session)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    Ok(CompatGraphMetadata {
        node_labels,
        relationship_types,
        property_keys,
    })
}

fn compat_procedure_rows(
    procedure: CompatProcedure,
    graph_metadata: &CompatGraphMetadata,
) -> Vec<Vec<(String, aiondb_engine::Value)>> {
    match procedure {
        CompatProcedure::DbPing => Vec::new(),
        CompatProcedure::DbmsComponents => vec![vec![
            ("name".to_owned(), aiondb_engine::Value::Text("Neo4j Kernel".to_owned())),
            (
                "versions".to_owned(),
                aiondb_engine::Value::Array(vec![aiondb_engine::Value::Text("5.26.0".to_owned())]),
            ),
            (
                "edition".to_owned(),
                aiondb_engine::Value::Text("community".to_owned()),
            ),
        ]],
        CompatProcedure::DbLabels => graph_metadata
            .node_labels
            .iter()
            .map(|label| {
                vec![("label".to_owned(), aiondb_engine::Value::Text(label.clone()))]
            })
            .collect(),
        CompatProcedure::DbRelationshipTypes => graph_metadata
            .relationship_types
            .iter()
            .map(|label| {
                vec![(
                    "relationshiptype".to_owned(),
                    aiondb_engine::Value::Text(label.clone()),
                )]
            })
            .collect(),
        CompatProcedure::DbPropertyKeys => graph_metadata
            .property_keys
            .iter()
            .map(|key| {
                vec![(
                    "propertykey".to_owned(),
                    aiondb_engine::Value::Text(key.clone()),
                )]
            })
            .collect(),
    }
}

fn compat_procedure_default_projection(procedure: CompatProcedure) -> Vec<(String, String)> {
    match procedure {
        CompatProcedure::DbPing => Vec::new(),
        CompatProcedure::DbmsComponents => vec![
            ("name".to_owned(), "name".to_owned()),
            ("versions".to_owned(), "versions".to_owned()),
            ("edition".to_owned(), "edition".to_owned()),
        ],
        CompatProcedure::DbLabels => vec![("label".to_owned(), "label".to_owned())],
        CompatProcedure::DbRelationshipTypes => {
            vec![("relationshiptype".to_owned(), "relationshipType".to_owned())]
        }
        CompatProcedure::DbPropertyKeys => {
            vec![("propertykey".to_owned(), "propertyKey".to_owned())]
        }
    }
}

fn compat_procedure_for_sql(sql: &str) -> Option<CompatProcedureQuery> {
    let Ok(statements) = parse_sql(sql) else {
        return None;
    };
    let [statement] = statements.as_slice() else {
        return None;
    };
    compat_procedure_for_statement(statement)
}

fn compat_procedure_for_statement(statement: &Statement) -> Option<CompatProcedureQuery> {
    let Statement::Cypher(statement) = statement else {
        return None;
    };
    if statement.union.is_some() {
        return None;
    }
    let first_clause = statement.clauses.first()?;
    let aiondb_parser::cypher_ast::CypherClause::Call(call) = first_clause else {
        return None;
    };
    if !call.args.is_empty() || call.subquery.is_some() {
        return None;
    }
    let procedure = match call.procedure.to_ascii_lowercase().as_str() {
        "db.ping" => CompatProcedure::DbPing,
        "dbms.components" => CompatProcedure::DbmsComponents,
        "db.labels" => CompatProcedure::DbLabels,
        "db.relationshiptypes" => CompatProcedure::DbRelationshipTypes,
        "db.propertykeys" => CompatProcedure::DbPropertyKeys,
        _ => return None,
    };
    let projection = if let Some(return_clause) = statement.clauses.get(1) {
        let aiondb_parser::cypher_ast::CypherClause::Return(ret) = return_clause else {
            return None;
        };
        if statement.clauses.len() != 2 {
            return None;
        }
        ret.items
            .iter()
            .map(|item| {
                let aiondb_parser::ast::Expr::Identifier(name) = &item.expr else {
                    return None;
                };
                let [source] = name.parts.as_slice() else {
                    return None;
                };
                let output = item.alias.clone().unwrap_or_else(|| source.clone());
                Some((source.clone(), output))
            })
            .collect::<Option<Vec<_>>>()?
    } else if call.yields.is_empty() {
        if statement.clauses.len() != 1 {
            return None;
        }
        compat_procedure_default_projection(procedure)
    } else {
        if statement.clauses.len() != 1 {
            return None;
        }
        call.yields
            .iter()
            .map(|field| (field.clone(), field.clone()))
            .collect()
    };
    Some(CompatProcedureQuery {
        procedure,
        projection,
    })
}

fn preview_statement(sql: &str) -> String {
    let mut preview = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 160;
    if preview.len() > MAX_LEN {
        preview.truncate(MAX_LEN - 3);
        preview.push_str("...");
    }
    preview
}

fn preview_debug<T: std::fmt::Debug>(value: &T) -> String {
    let mut preview = format!("{value:?}");
    const MAX_LEN: usize = 120;
    if preview.len() > MAX_LEN {
        preview.truncate(MAX_LEN - 3);
        preview.push_str("...");
    }
    preview
}

fn statement_is_bolt_run_supported(statement: &Statement) -> bool {
    match statement {
        Statement::Begin { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::CommitPrepared { .. }
        | Statement::RollbackPrepared { .. }
        | Statement::Savepoint { .. }
        | Statement::RollbackToSavepoint { .. }
        | Statement::ReleaseSavepoint { .. }
        | Statement::SetTransaction(_)
        | Statement::Copy(_) => false,
        Statement::Explain { statement, .. } => statement_is_bolt_run_supported(statement),
        _ => true,
    }
}

fn build_pending_query(results: Vec<StatementResult>) -> Result<BoltPendingQuery, io::Error> {
    let mut pending = None;
    for result in results {
        match result {
            StatementResult::Notice { .. } => {}
            StatementResult::Query { columns, rows } => {
                if pending.is_some() {
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
                pending = Some(BoltPendingQuery {
                    fields,
                    qid: 0,
                    bookmark: None,
                    result_available_after_ms: 0,
                    status,
                    stats: BoltQueryStats::default(),
                    summary: vec![("type", "r".to_owned())],
                    records,
                    next_record: 0,
                });
            }
            StatementResult::Command { .. } => {
                if pending.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Bolt compatibility only supports one command result",
                    ));
                }
                pending = Some(BoltPendingQuery {
                    fields: Vec::new(),
                    qid: 0,
                    bookmark: None,
                    result_available_after_ms: 0,
                    status: BoltStatusKind::NoData,
                    stats: BoltQueryStats {
                        contains_updates: true,
                        contains_system_updates: false,
                    },
                    summary: vec![("type", "w".to_owned())],
                    records: Vec::new(),
                    next_record: 0,
                });
            }
            StatementResult::CopyIn { .. } | StatementResult::CopyOut { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Bolt compatibility does not support COPY results",
                ));
            }
        }
    }
    pending.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Bolt compatibility expected a query or command result",
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
mod tests;
