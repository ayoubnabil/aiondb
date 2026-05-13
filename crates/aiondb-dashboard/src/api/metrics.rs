use std::sync::Arc;

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};

use aiondb_engine::QueryEngine;

use crate::server::AppState;

use super::auth::validate_session;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/metrics", post(get_metrics))
        // ADR-0010 ops surface: unauthenticated Prometheus text endpoint
        // meant for localhost scrape configs. Deployment docs emphasise
        // binding the dashboard to 127.0.0.1 so this endpoint is never
        // exposed on a public interface.
        .route("/metrics-prom", get(get_metrics_prometheus))
}

async fn get_metrics_prometheus(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    if !state.config.allow_unauthenticated_metrics_prometheus {
        return (
            StatusCode::NOT_FOUND,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            "# /api/metrics-prom is disabled\n".to_owned(),
        );
    }
    // ADR-0010 ops surface is loopback-only by design. Re-assert at request
    // time so a misconfigured deployment that binds non-loopback cannot
    // expose metrics to a co-tenant or SSRF target (audit dashboard F1).
    if !peer_addr.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            "# /api/metrics-prom is loopback-only\n".to_owned(),
        );
    }
    let permit = match super::acquire_blocking_permit(&state).await {
        Ok(permit) => permit,
        Err((status, _)) => {
            return (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; version=0.0.4"),
                )],
                "# dashboard is busy, retry later\n".to_owned(),
            );
        }
    };
    let engine = Arc::clone(&state.engine);
    let compat_result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        engine.compat_metrics_snapshot()
    })
    .await;
    let compat_snapshots = compat_result.unwrap_or_default();
    let body = aiondb_pg_compat::metrics::render_prometheus(&compat_snapshots);
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4"),
        )],
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::Response;

    use crate::auth::{SessionSecret, SessionStore};
    use crate::server::{build_dashboard_engine, AppState, DashboardConfig};

    fn test_state(allow_unauthenticated_metrics_prometheus: bool) -> Arc<AppState> {
        let engine = build_dashboard_engine().expect("dashboard engine");
        let mut config = DashboardConfig::default();
        config.allow_unauthenticated_metrics_prometheus = allow_unauthenticated_metrics_prometheus;
        Arc::new(AppState {
            sessions: Arc::new(SessionStore::new(
                std::time::Duration::from_secs(60),
                8,
                Arc::clone(&engine),
            )),
            engine,
            secret: SessionSecret::generate().expect("session secret"),
            config,
            blocking_ops: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }

    async fn metrics_prom_response(state: Arc<AppState>, peer: &str) -> Response {
        get_metrics_prometheus(
            State(state),
            ConnectInfo(peer.parse::<SocketAddr>().expect("socket addr")),
        )
        .await
        .into_response()
    }

    #[tokio::test]
    async fn prometheus_endpoint_is_disabled_by_default() {
        let response = metrics_prom_response(test_state(false), "127.0.0.1:4000").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn prometheus_endpoint_rejects_non_loopback_even_when_enabled() {
        let response = metrics_prom_response(test_state(true), "10.1.2.3:4000").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn prometheus_endpoint_allows_loopback_when_explicitly_enabled() {
        let response = metrics_prom_response(test_state(true), "127.0.0.1:4000").await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _session = match validate_session(&state, &body) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let permit = match super::acquire_blocking_permit(&state).await {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let engine = Arc::clone(&state.engine);
    let metrics_result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        (
            engine.query_metrics(),
            engine.session_count(),
            engine.compat_metrics_snapshot(),
        )
    })
    .await;
    let (snapshot, session_count, compat_snapshots) = match metrics_result {
        Ok((snapshot, session_count, compat)) => (snapshot, session_count.unwrap_or(0), compat),
        Err(join_error) => {
            tracing::error!(%join_error, "dashboard metrics worker failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to read metrics"})),
            );
        }
    };
    let dashboard_sessions = state.sessions.active_count();

    let compat_rows: Vec<serde_json::Value> = compat_snapshots
        .iter()
        .map(|s| {
            serde_json::json!({
                "command": s.command.as_tag(),
                "calls": s.calls,
                "fallbacks": s.fallbacks,
                "execute_count": s.execute.total_count,
                "execute_sum_us": s.execute.total_sum_us,
                "execute_mean_us": s.execute.mean_us(),
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "queries_total": snapshot.queries_total,
            "queries_failed": snapshot.queries_failed,
            "rows_returned_total": snapshot.rows_returned_total,
            "rows_affected_total": snapshot.rows_affected_total,
            "query_duration_micros_total": snapshot.query_duration_micros_total,
            "query_duration_p50_micros": snapshot.query_duration_p50_micros,
            "query_duration_p95_micros": snapshot.query_duration_p95_micros,
            "query_duration_p99_micros": snapshot.query_duration_p99_micros,
            "query_queue_depth_current": snapshot.query_queue_depth_current,
            "query_queue_depth_peak": snapshot.query_queue_depth_peak,
            "session_lock_wait_total": snapshot.session_lock_wait_total,
            "session_lock_wait_micros_total": snapshot.session_lock_wait_micros_total,
            "session_lock_wait_micros_max": snapshot.session_lock_wait_micros_max,
            "graph_ddl_operations": snapshot.graph_ddl_operations,
            "active_sessions": session_count,
            "dashboard_sessions": dashboard_sessions,
            "compat_commands": compat_rows,
        })),
    )
}
