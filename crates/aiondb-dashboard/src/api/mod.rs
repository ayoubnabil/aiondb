mod auth;
mod info;
mod metrics;
mod query;
mod schema;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Json;
use axum::Router;
use tokio::sync::OwnedSemaphorePermit;

use crate::server::AppState;

const DASHBOARD_BLOCKING_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(auth::routes())
        .merge(info::routes())
        .merge(query::routes())
        .merge(schema::routes())
        .merge(metrics::routes())
}

async fn acquire_blocking_permit(
    state: &Arc<AppState>,
) -> Result<OwnedSemaphorePermit, (StatusCode, Json<serde_json::Value>)> {
    match tokio::time::timeout(
        DASHBOARD_BLOCKING_ACQUIRE_TIMEOUT,
        Arc::clone(&state.blocking_ops).acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) | Err(_) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "dashboard is busy, retry later"})),
        )),
    }
}
