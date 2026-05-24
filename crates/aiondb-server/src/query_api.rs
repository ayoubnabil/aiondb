use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use tracing::{error, warn};

use aiondb_engine::{
    Credential, DbError, Engine, PortalBatch, QueryEngine, SecretString, SessionHandle,
    SessionInfo, SqlState, StartupParams, StatementResult, TransportInfo, TransportKind, Value,
};

use crate::ObservabilityState;

const QUERY_API_APPLICATION_NAME: &str = "aiondb-query-api";
const QUERY_API_MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const QUERY_API_MAX_STATEMENT_BYTES: usize = 1024 * 1024;
const QUERY_API_MAX_BASIC_AUTH_BYTES: usize = 8 * 1024;
const QUERY_API_MAX_USER_BYTES: usize = 128;
const QUERY_API_MAX_PASSWORD_BYTES: usize = 1024;
const QUERY_API_MAX_DATABASE_BYTES: usize = 128;
const QUERY_API_MAX_TX_ID_BYTES: usize = 128;
const QUERY_API_MAX_STATEMENTS_PER_REQUEST: usize = 32;
const QUERY_API_MAX_PARAMETERS_PER_STATEMENT: usize = 128;
const QUERY_API_MAX_PARAMETER_JSON_DEPTH: usize = 32;
const QUERY_API_MAX_PARAMETER_JSON_NODES: usize = 4096;
const QUERY_API_MAX_PARAMETER_STRING_BYTES: usize = 64 * 1024;
const QUERY_API_MAX_OPEN_TRANSACTIONS: usize = 64;
const QUERY_API_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct QueryApiRequest {
    statement: String,
    #[serde(default)]
    parameters: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "accessMode")]
    access_mode: Option<String>,
}

struct QueryApiIdentity {
    user: String,
    password: String,
}

pub(crate) struct QueryApiTransactionSession {
    pub session: SessionHandle,
    pub owner_user: String,
    pub database_name: String,
    pub last_activity: Instant,
}

pub(crate) fn routes() -> Router<Arc<ObservabilityState>> {
    Router::new()
        .route("/", get(discovery_handler))
        .route("/db/{database_name}/query/v2", post(query_v2_handler))
        .route("/db/{database_name}/tx", post(tx_begin_handler))
        .route("/db/{database_name}/tx/commit", post(tx_commit_handler))
        .route("/db/{database_name}/tx/{tx_id}", post(tx_continue_handler))
        .route(
            "/db/{database_name}/tx/{tx_id}",
            delete(tx_rollback_handler),
        )
        .route(
            "/db/{database_name}/tx/{tx_id}/commit",
            post(tx_id_commit_handler),
        )
        .layer(DefaultBodyLimit::max(QUERY_API_MAX_REQUEST_BYTES))
}

async fn discovery_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "name": "AionDB Query API compatibility wrapper",
            "query": "/db/{databaseName}/query/v2",
            "tx_begin": "/db/{databaseName}/tx",
            "tx_continue": "/db/{databaseName}/tx/{transactionId}",
            "tx_commit": "/db/{databaseName}/tx/commit",
            "tx_id_commit": "/db/{databaseName}/tx/{transactionId}/commit",
            "tx_rollback": "/db/{databaseName}/tx/{transactionId}",
            "auth": "basic",
            "compatibility": "neo4j-query-api-subset",
            "notes": [
                "single statement per request on /query/v2",
                "named parameters are rewritten to positional prepared parameters",
                "official neo4j drivers still require Bolt; this wrapper targets HTTP interoperability"
            ]
        })),
    )
}

async fn query_v2_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path(database_name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<QueryApiRequest>,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };

    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_query_api_request(&body) {
        return response;
    }

    let engine = Arc::clone(state.server.engine());
    let (session, _) = match startup_query_api_session(&engine, database_name, identity).await {
        Ok(session) => session,
        Err(response) => return response,
    };

    let start = Instant::now();
    let execute = tokio::task::spawn_blocking({
        let engine = Arc::clone(&engine);
        let session = session.clone();
        move || execute_statement_request(engine.as_ref(), &session, body)
    });

    let results = match tokio::time::timeout(QUERY_API_TIMEOUT, execute).await {
        Ok(Ok(results)) => results,
        Ok(Err(join_error)) => {
            let _ = engine.terminate(session);
            error!(%join_error, "query api execution worker failed");
            return internal_error("query execution failed");
        }
        Err(_) => {
            if let Err(err) = engine.cancel_session(&session) {
                warn!(%err, "failed to cancel timed-out query api session");
            }
            if let Err(err) = engine.terminate(session) {
                warn!(%err, "failed to terminate timed-out query api session");
            }
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({
                    "error": format!("query timed out after {}s", QUERY_API_TIMEOUT.as_secs()),
                })),
            )
                .into_response();
        }
    };

    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let terminate_result = engine.terminate(session);
    if let Err(err) = terminate_result {
        warn!(%err, "failed to terminate query api session after request");
    }

    match results {
        Ok(statement_results) => query_results_response(statement_results, elapsed_ms),
        Err(err) => db_error_response(err, elapsed_ms),
    }
}

#[derive(Deserialize)]
struct QueryApiTxCommitRequest {
    statements: Vec<QueryApiRequest>,
}

#[derive(Deserialize)]
struct QueryApiTxRequest {
    #[serde(default)]
    statements: Vec<QueryApiRequest>,
}

async fn tx_begin_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path(database_name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<QueryApiTxRequest>,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_statement_batch(&body.statements, true) {
        return response;
    }

    let engine = Arc::clone(state.server.engine());
    let owner_user = identity.user.clone();
    let (session, _) =
        match startup_query_api_session(&engine, database_name.clone(), identity).await {
            Ok(session) => session,
            Err(response) => return response,
        };

    if let Err(err) = engine.execute_sql(&session, "BEGIN") {
        let _ = engine.terminate(session);
        return db_error_response(err, 0);
    }

    if !body.statements.is_empty() {
        let execute = tokio::task::spawn_blocking({
            let engine = Arc::clone(&engine);
            let session = session.clone();
            move || execute_statements_request(engine.as_ref(), &session, body.statements)
        });
        match tokio::time::timeout(QUERY_API_TIMEOUT, execute).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(err))) => {
                let _ = engine.execute_sql(&session, "ROLLBACK");
                let _ = engine.terminate(session);
                return db_error_response(err, 0);
            }
            Ok(Err(join_error)) => {
                let _ = engine.execute_sql(&session, "ROLLBACK");
                let _ = engine.terminate(session);
                error!(%join_error, "query api tx begin worker failed");
                return internal_error("query execution failed");
            }
            Err(_) => {
                let _ = engine.cancel_session(&session);
                let _ = engine.execute_sql(&session, "ROLLBACK");
                let _ = engine.terminate(session);
                return (
                    StatusCode::REQUEST_TIMEOUT,
                    Json(json!({
                        "error": format!("query timed out after {}s", QUERY_API_TIMEOUT.as_secs()),
                    })),
                )
                    .into_response();
            }
        }
    }

    let tx_id = match generate_transaction_id() {
        Ok(tx_id) => tx_id,
        Err(error) => {
            let _ = engine.execute_sql(&session, "ROLLBACK");
            let _ = engine.terminate(session);
            error!(%error, "failed to generate query api transaction id");
            return internal_error("failed to create transaction");
        }
    };

    terminate_transaction_sessions(
        engine.as_ref(),
        evict_expired_query_api_transactions(&state, Instant::now()).await,
    );

    let replaced = {
        let mut transactions = state.query_api_transactions.lock().await;
        if transactions.len() >= QUERY_API_MAX_OPEN_TRANSACTIONS {
            drop(transactions);
            let _ = engine.execute_sql(&session, "ROLLBACK");
            let _ = engine.terminate(session);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "too many open query api transactions"})),
            )
                .into_response();
        }
        transactions.insert(
            tx_id.clone(),
            QueryApiTransactionSession {
                session,
                owner_user,
                database_name,
                last_activity: Instant::now(),
            },
        )
    };
    if let Some(replaced) = replaced {
        terminate_transaction_sessions(engine.as_ref(), vec![replaced]);
    }

    (
        StatusCode::OK,
        Json(json!({
            "txId": tx_id,
            "expiresInMs": QUERY_API_TIMEOUT.as_millis(),
        })),
    )
        .into_response()
}

async fn tx_commit_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path(database_name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<QueryApiTxCommitRequest>,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_statement_batch(&body.statements, false) {
        return response;
    }
    let statement_count = body.statements.len();

    let engine = Arc::clone(state.server.engine());
    let (session, _) = match startup_query_api_session(&engine, database_name, identity).await {
        Ok(session) => session,
        Err(response) => return response,
    };

    let start = Instant::now();
    let execute = tokio::task::spawn_blocking({
        let engine = Arc::clone(&engine);
        let session = session.clone();
        move || execute_tx_commit_request(engine.as_ref(), &session, body)
    });

    let results = match tokio::time::timeout(QUERY_API_TIMEOUT, execute).await {
        Ok(Ok(results)) => results,
        Ok(Err(join_error)) => {
            let _ = engine.terminate(session);
            error!(%join_error, "query api tx/commit worker failed");
            return internal_error("query execution failed");
        }
        Err(_) => {
            if let Err(err) = engine.cancel_session(&session) {
                warn!(%err, "failed to cancel timed-out query api tx session");
            }
            if let Err(err) = engine.terminate(session) {
                warn!(%err, "failed to terminate timed-out query api tx session");
            }
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({
                    "error": format!("query timed out after {}s", QUERY_API_TIMEOUT.as_secs()),
                })),
            )
                .into_response();
        }
    };

    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Err(err) = engine.terminate(session) {
        warn!(%err, "failed to terminate query api tx session after request");
    }

    match results {
        Ok(results) => (
            StatusCode::OK,
            Json(json!({
                "results": results.into_iter().map(format_statement_result).collect::<Vec<_>>(),
                "bookmarks": [],
                "summary": {
                    "elapsed_ms": elapsed_ms,
                    "statement_count": statement_count,
                }
            })),
        )
            .into_response(),
        Err(err) => db_error_response(err, elapsed_ms),
    }
}

async fn tx_continue_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path((database_name, tx_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<QueryApiTxRequest>,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_tx_id(&tx_id) {
        return response;
    }
    if let Err(response) = validate_statement_batch(&body.statements, true) {
        return response;
    }
    let session =
        match authorize_existing_transaction(&state, &database_name, &tx_id, identity).await {
            Ok(session) => session,
            Err(response) => return response,
        };
    let engine = Arc::clone(state.server.engine());
    let start = Instant::now();
    let execute = tokio::task::spawn_blocking({
        let engine = Arc::clone(&engine);
        let session = session.clone();
        move || execute_statements_request(engine.as_ref(), &session, body.statements)
    });
    match tokio::time::timeout(QUERY_API_TIMEOUT, execute).await {
        Ok(Ok(Ok(results))) => (
            StatusCode::OK,
            Json(json!({
                "results": results.into_iter().map(format_statement_result).collect::<Vec<_>>(),
                "txId": tx_id,
                "summary": {
                    "elapsed_ms": u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                }
            })),
        )
            .into_response(),
        Ok(Ok(Err(err))) => db_error_response(
            err,
            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        ),
        Ok(Err(join_error)) => {
            error!(%join_error, "query api tx continue worker failed");
            internal_error("query execution failed")
        }
        Err(_) => {
            let _ = engine.cancel_session(&session);
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({
                    "error": format!("query timed out after {}s", QUERY_API_TIMEOUT.as_secs()),
                })),
            )
                .into_response()
        }
    }
}

async fn tx_id_commit_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path((database_name, tx_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<QueryApiTxRequest>,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_tx_id(&tx_id) {
        return response;
    }
    if let Err(response) = validate_statement_batch(&body.statements, true) {
        return response;
    }
    let session =
        match authorize_existing_transaction(&state, &database_name, &tx_id, identity).await {
            Ok(session) => session,
            Err(response) => return response,
        };
    let engine = Arc::clone(state.server.engine());
    let start = Instant::now();
    let execute = tokio::task::spawn_blocking({
        let engine = Arc::clone(&engine);
        let session = session.clone();
        move || {
            execute_statements_request(engine.as_ref(), &session, body.statements)?;
            engine.execute_sql(&session, "COMMIT")?;
            Ok::<(), DbError>(())
        }
    });
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let response = match tokio::time::timeout(QUERY_API_TIMEOUT, execute).await {
        Ok(Ok(Ok(()))) => (
            StatusCode::OK,
            Json(json!({
                "txId": tx_id,
                "summary": {
                    "committed": true,
                    "elapsed_ms": elapsed_ms,
                }
            })),
        )
            .into_response(),
        Ok(Ok(Err(err))) => db_error_response(err, elapsed_ms),
        Ok(Err(join_error)) => {
            error!(%join_error, "query api tx commit worker failed");
            internal_error("query execution failed")
        }
        Err(_) => {
            let _ = engine.cancel_session(&session);
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({
                    "error": format!("query timed out after {}s", QUERY_API_TIMEOUT.as_secs()),
                })),
            )
                .into_response()
        }
    };
    let removed = state.query_api_transactions.lock().await.remove(&tx_id);
    if let Some(existing) = removed {
        let _ = engine.terminate(existing.session);
    }
    response
}

async fn tx_rollback_handler(
    State(state): State<Arc<ObservabilityState>>,
    Path((database_name, tx_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let identity = match parse_basic_auth(&headers) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    if let Err(response) = validate_database_name(&database_name) {
        return response;
    }
    if let Err(response) = validate_tx_id(&tx_id) {
        return response;
    }
    let session =
        match authorize_existing_transaction(&state, &database_name, &tx_id, identity).await {
            Ok(session) => session,
            Err(response) => return response,
        };
    let engine = Arc::clone(state.server.engine());
    let response = match engine.execute_sql(&session, "ROLLBACK") {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({
                "txId": tx_id,
                "summary": {
                    "rolled_back": true,
                }
            })),
        )
            .into_response(),
        Err(err) => db_error_response(err, 0),
    };
    let removed = state.query_api_transactions.lock().await.remove(&tx_id);
    if let Some(existing) = removed {
        let _ = engine.terminate(existing.session);
    }
    response
}

fn validate_database_name(database_name: &str) -> Result<(), axum::response::Response> {
    if database_name.trim().is_empty() {
        return Err(bad_request("database name must not be empty"));
    }
    if database_name.len() > QUERY_API_MAX_DATABASE_BYTES {
        return Err(bad_request("database name too large"));
    }
    Ok(())
}

fn validate_tx_id(tx_id: &str) -> Result<(), axum::response::Response> {
    if tx_id.is_empty() {
        return Err(bad_request("transaction id must not be empty"));
    }
    if tx_id.len() > QUERY_API_MAX_TX_ID_BYTES {
        return Err(bad_request("transaction id too large"));
    }
    Ok(())
}

fn validate_statement_batch(
    statements: &[QueryApiRequest],
    allow_empty: bool,
) -> Result<(), axum::response::Response> {
    if statements.is_empty() && !allow_empty {
        return Err(bad_request("statements must not be empty"));
    }
    if statements.len() > QUERY_API_MAX_STATEMENTS_PER_REQUEST {
        return Err(bad_request("too many statements"));
    }
    for statement in statements {
        validate_query_api_request(statement)?;
    }
    Ok(())
}

fn validate_query_api_request(request: &QueryApiRequest) -> Result<(), axum::response::Response> {
    if request.statement.trim().is_empty() {
        return Err(bad_request("statement must not be empty"));
    }
    if request.statement.len() > QUERY_API_MAX_STATEMENT_BYTES {
        return Err(bad_request("statement too large"));
    }
    if let Some(mode) = request.access_mode.as_deref() {
        if mode != "READ" && mode != "WRITE" {
            return Err(bad_request("accessMode must be READ or WRITE"));
        }
    }
    if request.parameters.len() > QUERY_API_MAX_PARAMETERS_PER_STATEMENT {
        return Err(bad_request("too many parameters"));
    }
    for value in request.parameters.values() {
        let mut nodes = 0usize;
        if !json_value_within_limits(value, 0, &mut nodes) {
            return Err(bad_request("parameter value too large"));
        }
    }
    Ok(())
}

fn json_value_within_limits(value: &serde_json::Value, depth: usize, nodes: &mut usize) -> bool {
    if depth > QUERY_API_MAX_PARAMETER_JSON_DEPTH {
        return false;
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > QUERY_API_MAX_PARAMETER_JSON_NODES {
        return false;
    }
    match value {
        serde_json::Value::String(text) => text.len() <= QUERY_API_MAX_PARAMETER_STRING_BYTES,
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| json_value_within_limits(value, depth + 1, nodes)),
        serde_json::Value::Object(object) => object.iter().all(|(key, value)| {
            key.len() <= QUERY_API_MAX_PARAMETER_STRING_BYTES
                && json_value_within_limits(value, depth + 1, nodes)
        }),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

async fn startup_query_api_session(
    engine: &Arc<Engine>,
    database_name: String,
    identity: QueryApiIdentity,
) -> Result<(SessionHandle, SessionInfo), axum::response::Response> {
    let startup_params = StartupParams {
        database: database_name,
        application_name: Some(QUERY_API_APPLICATION_NAME.to_owned()),
        options: Default::default(),
        credential: Credential::CleartextPassword {
            user: identity.user,
            password: SecretString::new(identity.password),
        },
        transport: TransportInfo {
            kind: TransportKind::Network {
                tls: true,
                peer_addr: Some("127.0.0.1:0".to_owned()),
            },
        },
    };

    let startup = tokio::task::spawn_blocking({
        let engine = Arc::clone(engine);
        move || engine.startup(startup_params)
    })
    .await;

    match startup {
        Ok(Ok(session)) => Ok(session),
        Ok(Err(err)) if is_auth_error(&err) => Err(unauthorized()),
        Ok(Err(err)) => Err(db_error_response(err, 0)),
        Err(join_error) => {
            error!(%join_error, "query api startup worker failed");
            Err(internal_error("query startup failed"))
        }
    }
}

fn execute_statement_request(
    engine: &Engine,
    session: &SessionHandle,
    request: QueryApiRequest,
) -> Result<Vec<StatementResult>, DbError> {
    let (statement, params) = rewrite_named_parameters(request.statement, &request.parameters)
        .map_err(|message| DbError::bind_error(SqlState::InvalidParameterValue, message))?;
    if params.is_empty() {
        engine.execute_sql(session, &statement)
    } else {
        let stmt_name = String::new();
        engine.prepare(session, stmt_name.clone(), statement)?;
        let (batch, notices) =
            engine.execute_prepared_statement_with_notices(session, stmt_name, params, 0)?;
        Ok(portal_batch_to_statement_results(batch, notices))
    }
}

fn execute_tx_commit_request(
    engine: &Engine,
    session: &SessionHandle,
    request: QueryApiTxCommitRequest,
) -> Result<Vec<StatementResult>, DbError> {
    engine.execute_sql(session, "BEGIN")?;
    let mut out = Vec::new();
    for statement in request.statements {
        match execute_statement_request(engine, session, statement) {
            Ok(results) => out.extend(results),
            Err(err) => {
                let _ = engine.execute_sql(session, "ROLLBACK");
                return Err(err);
            }
        }
    }
    engine.execute_sql(session, "COMMIT")?;
    Ok(out)
}

fn execute_statements_request(
    engine: &Engine,
    session: &SessionHandle,
    statements: Vec<QueryApiRequest>,
) -> Result<Vec<StatementResult>, DbError> {
    let mut out = Vec::new();
    for statement in statements {
        out.extend(execute_statement_request(engine, session, statement)?);
    }
    Ok(out)
}

async fn authorize_existing_transaction(
    state: &Arc<ObservabilityState>,
    database_name: &str,
    tx_id: &str,
    identity: QueryApiIdentity,
) -> Result<SessionHandle, axum::response::Response> {
    let engine = Arc::clone(state.server.engine());
    terminate_transaction_sessions(
        engine.as_ref(),
        evict_expired_query_api_transactions(state, Instant::now()).await,
    );

    let tx = {
        let sessions = state.query_api_transactions.lock().await;
        sessions.get(tx_id).map(|tx| QueryApiTransactionSession {
            session: tx.session.clone(),
            owner_user: tx.owner_user.clone(),
            database_name: tx.database_name.clone(),
            last_activity: tx.last_activity,
        })
    };
    let Some(tx) = tx else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "transaction not found"})),
        )
            .into_response());
    };
    if tx.database_name != database_name || tx.owner_user != identity.user {
        return Err(unauthorized());
    }

    let validation_session =
        match startup_query_api_session(&engine, database_name.to_owned(), identity).await {
            Ok(session) => session,
            Err(response) => return Err(response),
        };
    let _ = engine.terminate(validation_session.0);
    if let Some(transaction) = state.query_api_transactions.lock().await.get_mut(tx_id) {
        transaction.last_activity = Instant::now();
    }
    Ok(tx.session)
}

async fn evict_expired_query_api_transactions(
    state: &Arc<ObservabilityState>,
    now: Instant,
) -> Vec<QueryApiTransactionSession> {
    let mut transactions = state.query_api_transactions.lock().await;
    let expired_ids = transactions
        .iter()
        .filter(|(_, tx)| now.duration_since(tx.last_activity) >= QUERY_API_TIMEOUT)
        .map(|(tx_id, _)| tx_id.clone())
        .collect::<Vec<_>>();
    expired_ids
        .into_iter()
        .filter_map(|tx_id| transactions.remove(&tx_id))
        .collect()
}

fn terminate_transaction_sessions(engine: &Engine, sessions: Vec<QueryApiTransactionSession>) {
    for tx in sessions {
        let _ = engine.execute_sql(&tx.session, "ROLLBACK");
        if let Err(err) = engine.terminate(tx.session) {
            warn!(%err, "failed to terminate expired query api transaction");
        }
    }
}

fn generate_transaction_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn is_auth_error(err: &DbError) -> bool {
    matches!(
        err.sqlstate(),
        SqlState::InvalidAuthorizationSpecification
            | SqlState::InsufficientPrivilege
            | SqlState::TooManyAuthenticationFailures
    )
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

fn format_statement_result(result: StatementResult) -> serde_json::Value {
    match result {
        StatementResult::Query { columns, rows } => json!({
            "type": "query",
            "fields": columns.into_iter().map(|column| column.name).collect::<Vec<_>>(),
            "values": rows.into_iter().map(|row| {
                row.values.into_iter().map(|value| value_to_json(&value)).collect::<Vec<_>>()
            }).collect::<Vec<_>>(),
        }),
        StatementResult::Command { tag, rows_affected } => json!({
            "type": "command",
            "tag": tag,
            "rows_affected": rows_affected,
        }),
        StatementResult::Notice { message } => json!({
            "type": "notice",
            "description": message,
        }),
        StatementResult::CopyIn { .. } => json!({
            "type": "copy_in",
            "error": "COPY is not supported by the query api wrapper",
        }),
        StatementResult::CopyOut { .. } => json!({
            "type": "copy_out",
            "error": "COPY is not supported by the query api wrapper",
        }),
    }
}

pub(crate) fn rewrite_named_parameters(
    statement: String,
    parameters: &serde_json::Map<String, serde_json::Value>,
) -> Result<(String, Vec<Value>), String> {
    if parameters.is_empty() {
        return Ok((statement, Vec::new()));
    }

    let chars: Vec<char> = statement.chars().collect();
    let mut output = String::with_capacity(statement.len());
    let mut order = Vec::<String>::new();
    let mut index = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while index < chars.len() {
        let ch = chars[index];
        let next = chars.get(index + 1).copied();

        if line_comment {
            output.push(ch);
            if ch == '\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            output.push(ch);
            if ch == '*' && next == Some('/') {
                output.push('/');
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if in_single {
            output.push(ch);
            if ch == '\'' {
                if next == Some('\'') {
                    output.push('\'');
                    index += 2;
                    continue;
                }
                in_single = false;
            }
            index += 1;
            continue;
        }
        if in_double {
            output.push(ch);
            if ch == '"' {
                in_double = false;
            }
            index += 1;
            continue;
        }

        if ch == '-' && next == Some('-') {
            output.push(ch);
            output.push('-');
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && next == Some('*') {
            output.push(ch);
            output.push('*');
            block_comment = true;
            index += 2;
            continue;
        }
        if ch == '\'' {
            output.push(ch);
            in_single = true;
            index += 1;
            continue;
        }
        if ch == '"' {
            output.push(ch);
            in_double = true;
            index += 1;
            continue;
        }
        if ch == '$' {
            let Some(start) = chars.get(index + 1).copied() else {
                output.push(ch);
                index += 1;
                continue;
            };
            if start.is_ascii_alphabetic() || start == '_' {
                let mut end = index + 2;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                let name: String = chars[index + 1..end].iter().collect();
                if !parameters.contains_key(&name) {
                    return Err(format!("missing parameter ${name}"));
                }
                let position =
                    if let Some(pos) = order.iter().position(|existing| existing == &name) {
                        pos + 1
                    } else {
                        order.push(name);
                        order.len()
                    };
                output.push('$');
                output.push_str(&position.to_string());
                index = end;
                continue;
            }
        }

        output.push(ch);
        index += 1;
    }

    let params = order
        .into_iter()
        .map(|name| json_to_value(parameters.get(&name).expect("parameter exists")))
        .collect::<Vec<_>>();
    Ok((output, params))
}

fn json_to_value(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(v) => Value::Boolean(*v),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                match i32::try_from(i) {
                    Ok(narrow) => Value::Int(narrow),
                    Err(_) => Value::BigInt(i),
                }
            } else if let Some(u) = v.as_u64() {
                match i32::try_from(u) {
                    Ok(narrow) => Value::Int(narrow),
                    Err(_) => match i64::try_from(u) {
                        Ok(i) => Value::BigInt(i),
                        Err(_) => Value::Text(u.to_string()),
                    },
                }
            } else if let Some(f) = v.as_f64() {
                Value::Double(f)
            } else {
                Value::Text(v.to_string())
            }
        }
        serde_json::Value::String(v) => Value::Text(v.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(json_to_value).collect::<Vec<_>>())
        }
        serde_json::Value::Object(_) => Value::Jsonb(value.clone()),
    }
}

fn query_results_response(
    results: Vec<StatementResult>,
    elapsed_ms: u64,
) -> axum::response::Response {
    let mut primary_results = Vec::new();
    let mut notifications = Vec::new();

    for result in results {
        match result {
            StatementResult::Notice { message } => {
                notifications.push(json!({
                    "code": "AIONDB_NOTICE",
                    "severity": "INFORMATION",
                    "description": message,
                }));
            }
            other => primary_results.push(other),
        }
    }

    if primary_results.len() != 1 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "wrapper supports exactly one primary statement result per request",
                "elapsed_ms": elapsed_ms,
            })),
        )
            .into_response();
    }

    match primary_results.pop().expect("primary result") {
        StatementResult::Query { columns, rows } => {
            let row_count = rows.len();
            (
                StatusCode::OK,
                Json(json!({
                    "data": {
                        "fields": columns.into_iter().map(|column| column.name).collect::<Vec<_>>(),
                        "values": rows.into_iter().map(|row| {
                            row.values.into_iter().map(|value| value_to_json(&value)).collect::<Vec<_>>()
                        }).collect::<Vec<_>>(),
                    },
                    "bookmarks": [],
                    "notifications": notifications,
                    "summary": {
                        "result_type": "query",
                        "row_count": row_count,
                        "elapsed_ms": elapsed_ms,
                    }
                })),
            )
                .into_response()
        }
        StatementResult::Command { tag, rows_affected } => (
            StatusCode::OK,
            Json(json!({
                "data": {
                    "fields": [],
                    "values": [],
                },
                "bookmarks": [],
                "notifications": notifications,
                "summary": {
                    "result_type": "command",
                    "tag": tag,
                    "rows_affected": rows_affected,
                    "elapsed_ms": elapsed_ms,
                }
            })),
        )
            .into_response(),
        StatementResult::CopyIn { .. } | StatementResult::CopyOut { .. } => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "COPY is not supported by the query api wrapper",
                "elapsed_ms": elapsed_ms,
            })),
        )
            .into_response(),
        StatementResult::Notice { .. } => unreachable!("notices are filtered before formatting"),
    }
}

fn parse_basic_auth(headers: &HeaderMap) -> Result<QueryApiIdentity, axum::response::Response> {
    let Some(raw_header) = headers.get(header::AUTHORIZATION) else {
        return Err(unauthorized());
    };
    if raw_header.as_bytes().len() > QUERY_API_MAX_BASIC_AUTH_BYTES {
        return Err(unauthorized());
    }
    let Ok(raw_header) = raw_header.to_str() else {
        return Err(unauthorized());
    };
    let Some(encoded) = raw_header.strip_prefix("Basic ") else {
        return Err(unauthorized());
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Err(unauthorized());
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return Err(unauthorized());
    };
    let Some((user, password)) = decoded.split_once(':') else {
        return Err(unauthorized());
    };
    if user.trim().is_empty() || password.is_empty() {
        return Err(unauthorized());
    }
    if user.len() > QUERY_API_MAX_USER_BYTES || password.len() > QUERY_API_MAX_PASSWORD_BYTES {
        return Err(unauthorized());
    }
    Ok(QueryApiIdentity {
        user: user.to_owned(),
        password: password.to_owned(),
    })
}

fn unauthorized() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"AionDB Query API\"")],
        Json(json!({
            "error": "authentication required",
        })),
    )
        .into_response()
}

fn bad_request(message: &str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn internal_error(message: &str) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}

fn db_error_response(err: DbError, elapsed_ms: u64) -> axum::response::Response {
    (
        map_db_error_status(&err),
        Json(json!({
            "error": err.to_string(),
            "code": err.sqlstate().code(),
            "elapsed_ms": elapsed_ms,
        })),
    )
        .into_response()
}

fn map_db_error_status(err: &DbError) -> StatusCode {
    match err.sqlstate() {
        SqlState::InvalidAuthorizationSpecification | SqlState::InsufficientPrivilege => {
            StatusCode::FORBIDDEN
        }
        SqlState::SerializationFailure
        | SqlState::DeadlockDetected
        | SqlState::LockNotAvailable
        | SqlState::UniqueViolation
        | SqlState::ForeignKeyViolation
        | SqlState::NotNullViolation
        | SqlState::CheckViolation
        | SqlState::DependentObjectsStillExist
        | SqlState::ObjectNotInPrerequisiteState
        | SqlState::DuplicateSchema
        | SqlState::DuplicateColumn
        | SqlState::DuplicateObject
        | SqlState::InFailedSqlTransaction => StatusCode::CONFLICT,
        SqlState::TooManyConnections
        | SqlState::ProgramLimitExceeded
        | SqlState::TooManyAuthenticationFailures
        | SqlState::AdminShutdown => StatusCode::SERVICE_UNAVAILABLE,
        SqlState::IdleInTransactionSessionTimeout
        | SqlState::IdleSessionTimeout
        | SqlState::QueryCanceled => StatusCode::REQUEST_TIMEOUT,
        SqlState::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        SqlState::SyntaxError
        | SqlState::InvalidDatetimeFormat
        | SqlState::InvalidCatalogName
        | SqlState::UndefinedTable
        | SqlState::UndefinedColumn
        | SqlState::UndefinedFunction
        | SqlState::UndefinedObject
        | SqlState::UndefinedParameter
        | SqlState::InvalidCursorName
        | SqlState::InvalidCursorState
        | SqlState::InvalidSchemaName
        | SqlState::AmbiguousFunction
        | SqlState::DatatypeMismatch
        | SqlState::FeatureNotSupported
        | SqlState::InvalidTextRepresentation
        | SqlState::NumericValueOutOfRange
        | SqlState::DatetimeFieldOverflow
        | SqlState::StringDataRightTruncation
        | SqlState::InvalidParameterValue
        | SqlState::NoActiveSqlTransaction
        | SqlState::InvalidSavepointSpecification
        | SqlState::GroupingError
        | SqlState::InvalidTableDefinition
        | SqlState::WrongObjectType => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Int(v) => serde_json::Value::Number(serde_json::Number::from(*v)),
        Value::BigInt(v) => serde_json::Value::Number(serde_json::Number::from(*v)),
        Value::Real(v) => float_to_json(f64::from(*v)),
        Value::Double(v) => float_to_json(*v),
        Value::Numeric(v) => serde_json::Value::String(v.to_string()),
        Value::Money(v) => serde_json::Value::String(v.to_string()),
        Value::Text(v) => serde_json::Value::String(v.clone()),
        Value::Boolean(v) => serde_json::Value::Bool(*v),
        Value::Blob(v) => serde_json::Value::String(format!("\\x{}", hex_encode(v))),
        Value::Timestamp(v) => serde_json::Value::String(v.to_string()),
        Value::Date(v) => serde_json::Value::String(v.to_string()),
        Value::LargeDate(v) => serde_json::Value::String(v.to_string()),
        Value::Time(v) => serde_json::Value::String(v.to_string()),
        Value::TimeTz(v, offset) => serde_json::Value::String(format!("{v}{offset}")),
        Value::Interval(v) => serde_json::Value::String(v.to_string()),
        Value::Tid(v) => serde_json::Value::String(v.to_string()),
        Value::PgLsn(v) => serde_json::Value::String(v.to_string()),
        Value::MacAddr(v) => serde_json::Value::String(v.to_string()),
        Value::MacAddr8(v) => serde_json::Value::String(v.to_string()),
        Value::Uuid(v) => serde_json::Value::String(Value::Uuid(*v).to_string()),
        Value::TimestampTz(v) => serde_json::Value::String(v.to_string()),
        Value::Jsonb(v) => v.clone(),
        Value::Vector(v) => json!(v.values),
        Value::Array(v) => json!(v.iter().map(value_to_json).collect::<Vec<_>>()),
    }
}

fn float_to_json(v: f64) -> serde_json::Value {
    if v.is_nan() {
        json!("NaN")
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            json!("Infinity")
        } else {
            json!("-Infinity")
        }
    } else {
        json!(v)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
