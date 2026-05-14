use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use tracing::warn;

use aiondb_engine::{DbError, QueryEngine, SqlState, StatementResult, Value};

use crate::server::AppState;

use super::auth::validate_session;

const DASHBOARD_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DASHBOARD_MAX_RESULT_SETS: usize = 128;
const DASHBOARD_MAX_TEXT_VALUE_BYTES: usize = 64 * 1024;
const DASHBOARD_MAX_BLOB_RENDER_BYTES: usize = 32 * 1024;
const DASHBOARD_ROW_SERIALIZATION_BUDGET_BYTES: usize = 128 * 1024;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/query", post(execute_query))
}

async fn execute_query(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let Some(sql) = body.get("sql").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing sql field"})),
        );
    };

    if sql.len() > state.config.max_query_length {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "query too long"})),
        );
    }

    if sql.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "empty query"})),
        );
    }

    let start = std::time::Instant::now();
    let timeout = state.config.query_timeout;
    let engine = state.engine.clone();
    let engine_session = session.engine_session.clone();
    let sql_owned = sql.to_owned();
    let permit = match super::acquire_blocking_permit(&state).await {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let query_task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        engine.execute_sql(&engine_session, &sql_owned)
    });
    let results = tokio::time::timeout(timeout, query_task).await;
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let results = match results {
        Ok(Ok(r)) => r,
        Ok(Err(join_error)) => {
            warn!(%join_error, "dashboard query task failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "query execution failed: internal error",
                    "elapsed_ms": elapsed_ms,
                })),
            );
        }
        Err(_) => {
            if let Err(err) = state.engine.cancel_session(&session.engine_session) {
                warn!(%err, "failed to request cancellation for timed-out dashboard query");
            }
            let timed_out_session = state
                .sessions
                .remove(&session.session_id)
                .unwrap_or(session);
            if let Err(err) = state.engine.terminate(timed_out_session.engine_session) {
                warn!(%err, "failed to terminate timed-out dashboard session");
            }
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(serde_json::json!({
                    "error": format!("query timed out after {}s", timeout.as_secs()),
                    "elapsed_ms": elapsed_ms,
                    "session_terminated": true,
                })),
            );
        }
    };

    match results {
        Ok(results) => {
            let json_results = match format_results(
                &results,
                state.config.max_result_rows,
                DASHBOARD_MAX_RESPONSE_BYTES,
            ) {
                Ok(json_results) => json_results,
                Err(error) => {
                    return (
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Json(serde_json::json!({
                            "error": error,
                            "elapsed_ms": elapsed_ms,
                        })),
                    );
                }
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "results": json_results,
                    "elapsed_ms": elapsed_ms,
                })),
            )
        }
        Err(err) => {
            let status = map_db_error_status(&err);
            (
                status,
                Json(serde_json::json!({
                    "error": err.to_string(),
                    "sqlstate": err.sqlstate().code(),
                    "elapsed_ms": elapsed_ms,
                })),
            )
        }
    }
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

fn format_results(
    results: &[StatementResult],
    max_rows: usize,
    max_response_bytes: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let mut formatted = Vec::with_capacity(results.len().min(DASHBOARD_MAX_RESULT_SETS));
    let mut estimated_total_bytes = 2usize;

    for (result_index, r) in results.iter().enumerate() {
        if result_index >= DASHBOARD_MAX_RESULT_SETS {
            return Err(format!(
                "dashboard response exceeds maximum number of statement results ({DASHBOARD_MAX_RESULT_SETS})"
            ));
        }
        let value = match r {
            StatementResult::Query { columns, rows } => {
                let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
                let col_types: Vec<&str> =
                    columns.iter().map(|c| c.data_type.pg_type_name()).collect();
                let mut row_data: Vec<serde_json::Value> = Vec::new();
                let mut truncated = rows.len() > max_rows;
                let mut statement_bytes = 0usize;
                for row in rows.iter().take(max_rows) {
                    let row_json =
                        serde_json::Value::Array(row.values.iter().map(value_to_json).collect());
                    let row_bytes = serde_json::to_vec(&row_json)
                        .map(|bytes| bytes.len())
                        .map_err(|error| format!("failed to encode dashboard row: {error}"))?;
                    if row_bytes > DASHBOARD_ROW_SERIALIZATION_BUDGET_BYTES {
                        truncated = true;
                        break;
                    }
                    statement_bytes = statement_bytes.saturating_add(row_bytes);
                    if statement_bytes > max_response_bytes {
                        truncated = true;
                        break;
                    }
                    row_data.push(row_json);
                }
                let mut map = serde_json::Map::with_capacity(6);
                map.insert(
                    "type".to_owned(),
                    serde_json::Value::String("query".to_owned()),
                );
                map.insert("columns".to_owned(), serde_json::json!(col_names));
                map.insert("column_types".to_owned(), serde_json::json!(col_types));
                map.insert("rows".to_owned(), serde_json::Value::Array(row_data));
                map.insert("row_count".to_owned(), serde_json::json!(rows.len()));
                map.insert("truncated".to_owned(), serde_json::Value::Bool(truncated));
                serde_json::Value::Object(map)
            }
            StatementResult::Command { tag, rows_affected } => {
                serde_json::json!({
                    "type": "command",
                    "tag": tag,
                    "rows_affected": rows_affected,
                })
            }
            StatementResult::Notice { message } => {
                serde_json::json!({
                    "type": "notice",
                    "message": message,
                })
            }
            StatementResult::CopyIn { .. } => {
                serde_json::json!({
                    "type": "copy_in",
                    "message": "COPY IN not supported via dashboard",
                })
            }
            StatementResult::CopyOut { data, column_count } => {
                serde_json::json!({
                    "type": "copy_out",
                    "data": truncate_utf8_for_dashboard(data, DASHBOARD_MAX_TEXT_VALUE_BYTES),
                    "column_count": column_count,
                })
            }
        };
        let encoded_size = serde_json::to_vec(&value)
            .map(|bytes| bytes.len())
            .map_err(|error| format!("failed to encode dashboard response: {error}"))?;
        estimated_total_bytes = estimated_total_bytes.saturating_add(encoded_size);
        if estimated_total_bytes > max_response_bytes {
            return Err(format!(
                "dashboard response exceeds maximum size of {max_response_bytes} bytes"
            ));
        }
        formatted.push(value);
    }

    Ok(formatted)
}

pub(crate) fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Int(v) => serde_json::Value::Number(serde_json::Number::from(*v)),
        Value::BigInt(v) => serde_json::Value::Number(serde_json::Number::from(*v)),
        Value::Real(v) => float_to_json(f64::from(*v)),
        Value::Double(v) => float_to_json(*v),
        Value::Numeric(v) => serde_json::Value::String(v.to_string()),
        Value::Money(v) => serde_json::Value::String(v.to_string()),
        Value::Text(v) => serde_json::Value::String(truncate_utf8_for_dashboard(
            v,
            DASHBOARD_MAX_TEXT_VALUE_BYTES,
        )),
        Value::Boolean(v) => serde_json::Value::Bool(*v),
        Value::Blob(v) => {
            if v.len() > DASHBOARD_MAX_BLOB_RENDER_BYTES {
                let rendered = aiondb_core::hex_encode(&v[..DASHBOARD_MAX_BLOB_RENDER_BYTES]);
                serde_json::Value::String(format!(
                    "\\x{rendered}...(truncated,{}/{})",
                    DASHBOARD_MAX_BLOB_RENDER_BYTES,
                    v.len()
                ))
            } else {
                serde_json::Value::String(format!("\\x{}", aiondb_core::hex_encode(v)))
            }
        }
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
        Value::Vector(v) => {
            serde_json::json!(v.values)
        }
        Value::Array(v) => {
            let items: Vec<serde_json::Value> = v.iter().map(value_to_json).collect();
            serde_json::json!(items)
        }
    }
}

fn truncate_utf8_for_dashboard(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        let next = idx.saturating_add(ch.len_utf8());
        if next > max_bytes {
            break;
        }
        end = next;
    }
    if end == 0 {
        return "[truncated]".to_owned();
    }
    format!("{}...[truncated]", &text[..end])
}

fn float_to_json(v: f64) -> serde_json::Value {
    if v.is_nan() {
        serde_json::json!("NaN")
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            serde_json::json!("Infinity")
        } else {
            serde_json::json!("-Infinity")
        }
    } else {
        serde_json::json!(v)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::response::{IntoResponse, Response};
    use serde_json::json;

    use aiondb_engine::{Credential, SecretString, StartupParams, TransportInfo};

    use super::*;
    use crate::auth::{generate_session_id, DashboardSession, SessionSecret, SessionStore};
    use crate::server::{build_dashboard_engine, BootstrapAdmin, DashboardConfig, DashboardServer};

    fn app_state() -> Arc<AppState> {
        let engine = build_dashboard_engine().expect("build dashboard engine");
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());
        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        Arc::new(AppState {
            engine: engine.clone(),
            sessions: Arc::new(SessionStore::new(Duration::from_secs(60), 16, engine)),
            secret: SessionSecret::generate().expect("session secret"),
            config: DashboardConfig::default(),
            blocking_ops: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    fn create_session(state: &Arc<AppState>) -> (String, String) {
        let (engine_session, _) = state
            .engine
            .startup(StartupParams {
                database: "default".to_owned(),
                application_name: Some("dashboard-query-test".to_owned()),
                options: Default::default(),
                credential: Credential::CleartextPassword {
                    user: "admin".to_owned(),
                    password: SecretString::new("Secret123456".to_owned()),
                },
                transport: TransportInfo::in_process(),
            })
            .expect("startup");
        let session_id = generate_session_id().expect("session id");
        let now = Instant::now();
        let session = DashboardSession {
            session_id: session_id.clone(),
            username: "admin".to_owned(),
            database: "aiondb".to_owned(),
            created_at: now,
            last_activity: now,
            engine_session,
        };
        assert!(state.sessions.insert(session));
        let csrf = state.secret.sign_csrf(&session_id);
        (session_id, csrf)
    }

    async fn execute_query_response(
        state: Arc<AppState>,
        session_id: &str,
        csrf_token: &str,
        sql: &str,
    ) -> Response {
        execute_query(
            State(state),
            Json(json!({
                "session_id": session_id,
                "csrf_token": csrf_token,
                "sql": sql,
            })),
        )
        .await
        .into_response()
    }

    #[test]
    fn db_error_status_mapping_is_stable() {
        assert_eq!(
            map_db_error_status(&DbError::syntax_error("syntax error")),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            map_db_error_status(&DbError::insufficient_privilege("denied")),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            map_db_error_status(&DbError::constraint_error(
                SqlState::UniqueViolation,
                "duplicate"
            )),
            StatusCode::CONFLICT
        );
        assert_eq!(
            map_db_error_status(&DbError::query_canceled("canceled")),
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(
            map_db_error_status(&DbError::storage_error(
                SqlState::TooManyConnections,
                "too many"
            )),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            map_db_error_status(&DbError::internal("internal")),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn execute_query_returns_4xx_for_syntax_error() {
        let state = app_state();
        let (session_id, csrf_token) = create_session(&state);

        let response = execute_query_response(state, &session_id, &csrf_token, "SELECT FROM").await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn execute_query_returns_409_for_unique_violation() {
        let state = app_state();
        let (session_id, csrf_token) = create_session(&state);

        let create = execute_query_response(
            state.clone(),
            &session_id,
            &csrf_token,
            "CREATE TABLE uq_http_map (id INT UNIQUE)",
        )
        .await;
        assert_eq!(create.status(), StatusCode::OK);

        let first_insert = execute_query_response(
            state.clone(),
            &session_id,
            &csrf_token,
            "INSERT INTO uq_http_map VALUES (1)",
        )
        .await;
        assert_eq!(first_insert.status(), StatusCode::OK);

        let second_insert = execute_query_response(
            state.clone(),
            &session_id,
            &csrf_token,
            "INSERT INTO uq_http_map VALUES (1)",
        )
        .await;
        assert_eq!(second_insert.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn truncate_utf8_for_dashboard_respects_character_boundaries() {
        assert_eq!(
            truncate_utf8_for_dashboard("abcdef", 3),
            "abc...[truncated]"
        );
        assert_eq!(truncate_utf8_for_dashboard("ééé", 3), "é...[truncated]");
    }

    #[test]
    fn format_results_rejects_oversized_payload() {
        let columns = vec![aiondb_engine::ResultColumn {
            name: "v".to_owned(),
            data_type: aiondb_engine::DataType::Text,
            text_type_modifier: None,
            nullable: true,
        }];
        let rows = vec![aiondb_engine::Row::new(vec![Value::Text("abcdef".to_owned())]); 16];
        let results = vec![StatementResult::Query { columns, rows }];
        let err = format_results(&results, 16, 64).expect_err("payload should exceed hard cap");
        assert!(
            err.contains("exceeds maximum size"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn format_results_rejects_too_many_statement_results() {
        let results = (0..=DASHBOARD_MAX_RESULT_SETS)
            .map(|_| StatementResult::Notice {
                message: "n".to_owned(),
            })
            .collect::<Vec<_>>();
        let err = format_results(&results, 10, usize::MAX).expect_err("must reject excessive set");
        assert!(
            err.contains("maximum number of statement results"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn value_to_json_truncates_large_text_and_blob_values() {
        let oversized_text = "a".repeat(DASHBOARD_MAX_TEXT_VALUE_BYTES.saturating_add(32));
        let text_json = value_to_json(&Value::Text(oversized_text));
        let text_rendered = text_json
            .as_str()
            .expect("text value should serialize to a string");
        assert!(text_rendered.ends_with("...[truncated]"));

        let oversized_blob = vec![0xAB; DASHBOARD_MAX_BLOB_RENDER_BYTES.saturating_add(8)];
        let blob_json = value_to_json(&Value::Blob(oversized_blob));
        let blob_rendered = blob_json
            .as_str()
            .expect("blob value should serialize to a string");
        assert!(blob_rendered.contains("truncated"));
    }
}
