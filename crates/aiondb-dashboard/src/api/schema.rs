use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};

use aiondb_core::escape_sql_literal;
use aiondb_engine::{QueryEngine, StatementResult};

use crate::server::AppState;

use super::auth::validate_session;

const DASHBOARD_SCHEMA_MAX_ROWS: usize = 1_000;
const DASHBOARD_SCHEMA_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DASHBOARD_SCHEMA_ROW_SERIALIZATION_BUDGET_BYTES: usize = 64 * 1024;

/// Validate that a schema or table identifier is reasonable (ASCII
/// alphanumeric, underscores, dots -- reject anything else to block
/// injection attempts).  Returns `None` with an error response on
/// invalid input.
fn validate_identifier(
    value: &str,
    field_name: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Reject empty or excessively long identifiers.
    if value.is_empty() || value.len() > 128 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{field_name} is empty or too long")})),
        ));
    }
    // Only allow characters valid in standard SQL identifiers.
    // Hyphens are intentionally excluded - they are not valid in
    // unquoted SQL identifiers and could cause parsing ambiguity.
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("{field_name} contains invalid characters")})),
        ));
    }
    Ok(())
}

/// Wrap a catalog query result into an HTTP response with the given JSON key.
/// Error messages are sanitized to avoid leaking internal schema details.
fn catalog_response(
    result: Result<serde_json::Value, String>,
    key: &str,
) -> (StatusCode, Json<serde_json::Value>) {
    match result {
        Ok(data) => (StatusCode::OK, Json(serde_json::json!({key: data}))),
        Err(err) => {
            tracing::warn!(error = %err, "catalog query failed");
            if err.contains("catalog query timed out") {
                return (
                    StatusCode::REQUEST_TIMEOUT,
                    Json(serde_json::json!({"error": "catalog query timed out"})),
                );
            }
            if err.contains("catalog response exceeds maximum") {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(serde_json::json!({"error": "catalog response too large"})),
                );
            }
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "catalog query failed"})),
            )
        }
    }
}

async fn checked_catalog_query(
    state: Arc<AppState>,
    session: aiondb_engine::SessionHandle,
    sql: String,
    key: &'static str,
) -> (StatusCode, Json<serde_json::Value>) {
    let permit = match super::acquire_blocking_permit(&state).await {
        Ok(permit) => permit,
        Err(response) => return response,
    };

    catalog_response(catalog_query(state, session, sql, permit).await, key)
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/schema/databases", post(list_databases))
        .route("/schema/schemas", post(list_schemas))
        .route("/schema/tables", post(list_tables))
        .route("/schema/columns", post(list_columns))
        .route("/schema/indexes", post(list_indexes))
        .route("/schema/constraints", post(list_constraints))
        .route("/schema/sequences", post(list_sequences))
        .route("/schema/views", post(list_views))
        .route("/schema/functions", post(list_functions))
}

/// Execute a catalog query on the blocking pool and return the result as JSON.
async fn catalog_query(
    state: Arc<AppState>,
    session: aiondb_engine::SessionHandle,
    sql: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<serde_json::Value, String> {
    let engine = Arc::clone(&state.engine);
    let timeout = state.config.query_timeout;
    let session_for_cancel = session.clone();
    let query_task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        engine.execute_sql(&session, &sql)
    });
    let results = match tokio::time::timeout(timeout, query_task).await {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => return Err(format!("catalog query worker failed: {error}")),
        Err(_) => {
            if let Err(error) = state.engine.cancel_session(&session_for_cancel) {
                tracing::warn!(%error, "failed to cancel timed-out catalog query");
            }
            return Err(format!(
                "catalog query timed out after {}s",
                timeout.as_secs()
            ));
        }
    }
    .map_err(|e| e.to_string())?;

    for result in &results {
        if let StatementResult::Query { columns, rows } = result {
            let row_cap = state.config.max_result_rows.min(DASHBOARD_SCHEMA_MAX_ROWS);
            if rows.len() > row_cap {
                return Err(format!(
                    "catalog response exceeds maximum row count ({row_cap})"
                ));
            }
            let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
            let mut row_data: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
            let mut response_bytes = 2usize;
            for row in rows {
                let mut obj = serde_json::Map::with_capacity(col_names.len());
                for (i, val) in row.values.iter().enumerate() {
                    let key = col_names.get(i).copied().unwrap_or("?");
                    obj.insert(key.to_owned(), super::query::value_to_json(val));
                }
                let row_json = serde_json::Value::Object(obj);
                let row_bytes = serde_json::to_vec(&row_json)
                    .map(|bytes| bytes.len())
                    .unwrap_or(DASHBOARD_SCHEMA_ROW_SERIALIZATION_BUDGET_BYTES);
                response_bytes = response_bytes.saturating_add(row_bytes);
                if response_bytes > DASHBOARD_SCHEMA_MAX_RESPONSE_BYTES {
                    return Err(format!(
                        "catalog response exceeds maximum size ({DASHBOARD_SCHEMA_MAX_RESPONSE_BYTES} bytes)"
                    ));
                }
                row_data.push(row_json);
            }
            return Ok(serde_json::Value::Array(row_data));
        }
    }

    Ok(serde_json::json!([]))
}

async fn list_databases(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        "SELECT datname AS name FROM pg_catalog.pg_database ORDER BY datname".to_owned(),
        "databases",
    )
    .await
}

async fn list_schemas(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        "SELECT nspname AS name, oid \
         FROM pg_catalog.pg_namespace \
         ORDER BY nspname"
            .to_owned(),
        "schemas",
    )
    .await
}

async fn list_tables(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    let schema = escape_sql_literal(schema);

    // Use information_schema for portability.
    let sql = format!(
        "SELECT table_name AS name, table_type \
         FROM information_schema.tables \
         WHERE table_schema = '{schema}' \
         ORDER BY table_name",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "tables",
    )
    .await
}

async fn list_columns(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    let Some(table) = body.get("table").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing table field"})),
        );
    };
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    if let Err(resp) = validate_identifier(table, "table") {
        return resp;
    }
    let schema = escape_sql_literal(schema);
    let table = escape_sql_literal(table);

    let sql = format!(
        "SELECT column_name AS name, data_type, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_schema = '{schema}' AND table_name = '{table}' \
         ORDER BY ordinal_position",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "columns",
    )
    .await
}

async fn list_indexes(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    let Some(table) = body.get("table").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing table field"})),
        );
    };
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    if let Err(resp) = validate_identifier(table, "table") {
        return resp;
    }
    let schema = escape_sql_literal(schema);
    let table = escape_sql_literal(table);

    let sql = format!(
        "SELECT i.relname AS index_name, ix.indisunique AS is_unique, \
                ix.indisprimary AS is_primary \
         FROM pg_catalog.pg_index ix \
         JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid \
         JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         WHERE n.nspname = '{schema}' AND t.relname = '{table}' \
         ORDER BY i.relname",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "indexes",
    )
    .await
}

async fn list_constraints(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    let Some(table) = body.get("table").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing table field"})),
        );
    };
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    if let Err(resp) = validate_identifier(table, "table") {
        return resp;
    }
    let schema = escape_sql_literal(schema);
    let table = escape_sql_literal(table);

    let sql = format!(
        "SELECT constraint_name AS name, constraint_type \
         FROM information_schema.table_constraints \
         WHERE table_schema = '{schema}' AND table_name = '{table}' \
         ORDER BY constraint_name",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "constraints",
    )
    .await
}

async fn list_sequences(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    let schema = escape_sql_literal(schema);

    let sql = format!(
        "SELECT c.relname AS name \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{schema}' AND c.relkind = 'S' \
         ORDER BY c.relname",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "sequences",
    )
    .await
}

async fn list_views(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    let schema = escape_sql_literal(schema);

    let sql = format!(
        "SELECT table_name AS name, view_definition \
         FROM information_schema.views \
         WHERE table_schema = '{schema}' \
         ORDER BY table_name",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "views",
    )
    .await
}

async fn list_functions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let schema = body
        .get("schema")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    if let Err(resp) = validate_identifier(schema, "schema") {
        return resp;
    }
    let schema = escape_sql_literal(schema);

    let sql = format!(
        "SELECT p.proname AS name, l.lanname AS language \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         JOIN pg_catalog.pg_language l ON l.oid = p.prolang \
         WHERE n.nspname = '{schema}' \
         ORDER BY p.proname",
    );
    checked_catalog_query(
        Arc::clone(&state),
        session.engine_session.clone(),
        sql,
        "functions",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_response_maps_timeout_to_408() {
        let (status, body) = catalog_response(
            Err("catalog query timed out after 10s".to_owned()),
            "schemas",
        );
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            body.0["error"],
            serde_json::json!("catalog query timed out")
        );
    }

    #[test]
    fn catalog_response_maps_oversized_to_413() {
        let (status, body) = catalog_response(
            Err("catalog response exceeds maximum size".to_owned()),
            "schemas",
        );
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            body.0["error"],
            serde_json::json!("catalog response too large")
        );
    }

    #[test]
    fn catalog_response_wraps_success_under_key() {
        let (status, body) =
            catalog_response(Ok(serde_json::json!([{"name": "public"}])), "schemas");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.0["schemas"][0]["name"], serde_json::json!("public"));
    }
}
