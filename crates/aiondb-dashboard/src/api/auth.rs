use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use aiondb_engine::{
    Credential, QueryEngine, SecretString, StartupParams, TransportInfo, TransportKind,
};

use crate::auth::{generate_session_id, DashboardSession};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/status", post(session_status))
}

const MAX_USERNAME_LENGTH: usize = 128;
const MAX_PASSWORD_LENGTH: usize = 1024;
const MAX_DATABASE_LENGTH: usize = 128;
const MAX_SESSION_ID_LENGTH: usize = 128;
const MAX_CSRF_TOKEN_LENGTH: usize = 128;

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    #[serde(default = "default_database")]
    database: String,
}

fn default_database() -> String {
    "default".to_owned()
}

async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    // Validate input lengths to prevent abuse.
    if req.username.len() > MAX_USERNAME_LENGTH
        || req.password.len() > MAX_PASSWORD_LENGTH
        || req.database.len() > MAX_DATABASE_LENGTH
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "input too long"})),
        );
    }

    if req.username.trim().is_empty() || req.database.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "username and database must not be empty"})),
        );
    }

    // Dashboard login always requires a password - anonymous access over
    // the network is a security risk. Reject empty passwords upfront rather
    // than falling through to an Anonymous credential that could bypass
    // password-based authentication.
    if req.password.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "authentication failed"})),
        );
    }

    let credential = Credential::CleartextPassword {
        user: req.username.clone(),
        password: SecretString::new(req.password),
    };

    let transport_tls = match resolve_dashboard_login_transport_tls(&state, &headers) {
        Ok(tls) => tls,
        Err(response) => return response,
    };

    let params = StartupParams {
        database: req.database.clone(),
        application_name: Some("aiondb-dashboard".to_owned()),
        options: Default::default(),
        credential,
        transport: TransportInfo {
            kind: TransportKind::Network {
                tls: transport_tls,
                peer_addr: Some(peer_addr.to_string()),
            },
        },
    };

    let permit = match super::acquire_blocking_permit(&state).await {
        Ok(permit) => permit,
        Err(response) => return response,
    };
    let engine = Arc::clone(&state.engine);
    let startup_result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        engine.startup(params)
    })
    .await;

    let (engine_session, _info) = match startup_result {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            tracing::warn!(user = %req.username, peer = %peer_addr, "dashboard login failed: {err}");
            // Apply a fixed delay on failed login attempts to slow down
            // brute-force attacks against the dashboard HTTP endpoint.
            // The engine's own rate limiter handles lockout; this delay
            // adds defense-in-depth at the HTTP layer.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "authentication failed"})),
            );
        }
        Err(join_error) => {
            tracing::error!(%join_error, user = %req.username, peer = %peer_addr, "dashboard login worker failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "authentication backend failed"})),
            );
        }
    };

    let cleanup_session = engine_session.clone();
    let session_id = match generate_session_id() {
        Ok(session_id) => session_id,
        Err(err) => {
            tracing::error!(%err, "failed to generate dashboard session id");
            if let Err(term_err) = state.engine.terminate(cleanup_session) {
                tracing::warn!(%term_err, user = %req.username, "engine session cleanup failed during login error");
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to create session"})),
            );
        }
    };
    let now = std::time::Instant::now();
    let session = DashboardSession {
        session_id: session_id.clone(),
        username: req.username.clone(),
        database: req.database.clone(),
        created_at: now,
        last_activity: now,
        engine_session,
    };

    if !state.sessions.insert(session) {
        if let Err(err) = state.engine.terminate(cleanup_session) {
            tracing::warn!(%err, user = %req.username, "failed to clean up rejected dashboard login session");
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "too many active sessions"})),
        );
    }

    let csrf_token = state.secret.sign_csrf(&session_id);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "session_id": session_id,
            "csrf_token": csrf_token,
            "username": req.username,
            "database": req.database,
        })),
    )
}

async fn logout(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let (session_id, csrf_token) = match extract_session_tokens(&body) {
        Ok(tokens) => tokens,
        Err(response) => return response.into_response(),
    };

    if !state.secret.verify_csrf(session_id, csrf_token) {
        return StatusCode::FORBIDDEN.into_response();
    }

    if let Some(session) = state.sessions.remove(session_id) {
        if let Err(err) = state.engine.terminate(session.engine_session) {
            tracing::warn!(%err, "engine session cleanup failed during logout");
        }
    }

    StatusCode::OK.into_response()
}

async fn session_status(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let (session_id, csrf_token) = match extract_session_tokens(&body) {
        Ok(tokens) => tokens,
        Err(response) => return response.into_response(),
    };

    if session_id.is_empty() || !state.secret.verify_csrf(session_id, csrf_token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"authenticated": false})),
        )
            .into_response();
    }

    match state.sessions.get(session_id) {
        Some(session) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "authenticated": true,
                "username": session.username,
                "database": session.database,
            })),
        ),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"authenticated": false})),
        ),
    }
    .into_response()
}

/// Extract and validate a session from a request body.
/// Returns `None` with an error response if invalid.
pub(crate) fn validate_session(
    state: &AppState,
    body: &serde_json::Value,
) -> Result<DashboardSession, (StatusCode, Json<serde_json::Value>)> {
    let (session_id, csrf_token) = extract_session_tokens(body)?;

    if session_id.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing session_id"})),
        ));
    }

    if !state.secret.verify_csrf(session_id, csrf_token) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid CSRF token"})),
        ));
    }

    state.sessions.get(session_id).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "session expired"})),
        )
    })
}

fn resolve_dashboard_login_transport_tls(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<bool, (StatusCode, Json<serde_json::Value>)> {
    if !has_proxy_forwarding_headers(headers) {
        return Ok(false);
    }

    if !state.config.trust_proxy_tls_headers {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "dashboard proxy headers are not trusted; set AIONDB_DASHBOARD_TRUST_PROXY_TLS_HEADERS=true only behind a local HTTPS reverse proxy"
            })),
        ));
    }

    if forwarded_proto_is_https(headers) {
        Ok(true)
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "dashboard login requires HTTPS at the trusted reverse proxy"
            })),
        ))
    }
}

fn has_proxy_forwarding_headers(headers: &HeaderMap) -> bool {
    headers.contains_key("x-forwarded-for")
        || headers.contains_key("x-forwarded-proto")
        || headers.contains_key("forwarded")
}

fn forwarded_proto_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
        || headers
            .get("forwarded")
            .and_then(|value| value.to_str().ok())
            .is_some_and(forwarded_header_proto_is_https)
}

fn forwarded_header_proto_is_https(value: &str) -> bool {
    value.split(',').any(|entry| {
        entry.split(';').any(|part| {
            let part = part.trim();
            part.split_once('=').is_some_and(|(key, raw_value)| {
                key.trim().eq_ignore_ascii_case("proto")
                    && raw_value
                        .trim()
                        .trim_matches('"')
                        .eq_ignore_ascii_case("https")
            })
        })
    })
}

fn extract_session_tokens(
    body: &serde_json::Value,
) -> Result<(&str, &str), (StatusCode, Json<serde_json::Value>)> {
    let session_id = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let csrf_token = body
        .get("csrf_token")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if session_id.len() > MAX_SESSION_ID_LENGTH || csrf_token.len() > MAX_CSRF_TOKEN_LENGTH {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "session token too long"})),
        ));
    }

    Ok((session_id, csrf_token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use axum::response::Response;

    use crate::auth::{SessionSecret, SessionStore};
    use crate::server::{build_dashboard_engine, DashboardConfig, DashboardServer};

    fn app_state(max_sessions: usize) -> Arc<AppState> {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());
        server
            .bootstrap_admin(&crate::server::BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let bootstrap_session = engine
            .startup(StartupParams {
                database: "default".to_owned(),
                application_name: Some("dashboard-auth-test-bootstrap".to_owned()),
                options: Default::default(),
                credential: Credential::CleartextPassword {
                    user: "admin".to_owned(),
                    password: SecretString::new("Secret123456".to_owned()),
                },
                transport: TransportInfo::in_process(),
            })
            .expect("bootstrap admin startup");
        engine
            .execute_sql(&bootstrap_session.0, "CREATE ROLE nopass LOGIN")
            .expect("create passwordless role");
        engine
            .terminate(bootstrap_session.0)
            .expect("terminate bootstrap session");

        Arc::new(AppState {
            engine: engine.clone(),
            sessions: Arc::new(SessionStore::new(
                std::time::Duration::from_secs(60),
                max_sessions,
                engine,
            )),
            secret: SessionSecret::generate().expect("session secret"),
            config: DashboardConfig::default(),
            blocking_ops: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    async fn login_response(
        state: Arc<AppState>,
        username: &str,
        password: &str,
        forwarded_proto: Option<&str>,
    ) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(proto) = forwarded_proto {
            headers.insert(
                "x-forwarded-proto",
                HeaderValue::from_str(proto).expect("header"),
            );
            headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
        }
        login(
            State(state),
            ConnectInfo("127.0.0.1:4000".parse::<SocketAddr>().expect("socket addr")),
            headers,
            Json(LoginRequest {
                username: username.to_owned(),
                password: password.to_owned(),
                database: "default".to_owned(),
            }),
        )
        .await
        .into_response()
    }

    #[tokio::test]
    async fn login_rejects_passwordless_roles_over_network_transport() {
        let state = app_state(4);

        let response = login_response(state, "nopass", "", None).await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_rejected_at_capacity_terminates_engine_session() {
        let state = app_state(1);

        let first = login_response(state.clone(), "admin", "Secret123456", None).await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(state.engine.session_count().expect("session count"), 1);

        let second = login_response(state.clone(), "admin", "Secret123456", None).await;
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(state.engine.session_count().expect("session count"), 1);
    }

    #[tokio::test]
    async fn login_rejects_proxy_headers_by_default() {
        let state = app_state(4);

        let response = login_response(state, "admin", "Secret123456", Some("https")).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn login_rejects_trusted_proxy_without_https_proto() {
        let mut config = DashboardConfig::default();
        config.trust_proxy_tls_headers = true;
        let state = app_state_with_config(4, config);

        let response = login_response(state, "admin", "Secret123456", Some("http")).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn login_accepts_trusted_https_proxy_headers() {
        let mut config = DashboardConfig::default();
        config.trust_proxy_tls_headers = true;
        let state = app_state_with_config(4, config);

        let response = login_response(state, "admin", "Secret123456", Some("https")).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn validate_session_rejects_oversized_tokens() {
        let state = app_state(4);
        let body = serde_json::json!({
            "session_id": "s".repeat(MAX_SESSION_ID_LENGTH + 1),
            "csrf_token": "c".repeat(MAX_CSRF_TOKEN_LENGTH + 1),
        });

        let Err(response) = validate_session(&state, &body) else {
            panic!("oversized tokens must fail");
        };
        assert_eq!(response.0, StatusCode::BAD_REQUEST);
    }

    fn app_state_with_config(max_sessions: usize, config: DashboardConfig) -> Arc<AppState> {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), config.clone());
        server
            .bootstrap_admin(&crate::server::BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        Arc::new(AppState {
            engine: engine.clone(),
            sessions: Arc::new(SessionStore::new(
                std::time::Duration::from_secs(60),
                max_sessions,
                engine,
            )),
            secret: SessionSecret::generate().expect("session secret"),
            config,
            blocking_ops: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }
}
