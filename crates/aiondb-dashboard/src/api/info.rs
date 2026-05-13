use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};

use aiondb_config::V0_1_PRODUCT_CONSTRAINTS;

use crate::server::AppState;

use super::auth::validate_session;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/info", post(get_info))
}

async fn get_info(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _session = match validate_session(&state, &body) {
        Ok(session) => session,
        Err(resp) => return resp,
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "release_line": V0_1_PRODUCT_CONSTRAINTS.release_line,
            "deployment": {
                "mode": V0_1_PRODUCT_CONSTRAINTS.topology,
                "clustering": V0_1_PRODUCT_CONSTRAINTS.clustering.as_str(),
                "summary": V0_1_PRODUCT_CONSTRAINTS.clustering_summary(),
                "label": "single-node only",
            },
            "storage": {
                "encryption_at_rest": V0_1_PRODUCT_CONSTRAINTS.encryption_at_rest.as_str(),
                "summary": V0_1_PRODUCT_CONSTRAINTS.encryption_at_rest_summary(),
                "label": "unencrypted at rest",
            },
            "operations": {
                "backup_restore": V0_1_PRODUCT_CONSTRAINTS.backup_restore.as_str(),
                "summary": V0_1_PRODUCT_CONSTRAINTS.backup_restore_summary(),
                "label": "logical dump/restore",
            },
        })),
    )
}
