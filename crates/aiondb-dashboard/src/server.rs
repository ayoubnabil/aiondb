use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::Router;
use tokio::sync::{watch, Semaphore};
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::info;

use aiondb_config::RuntimeConfig;
use aiondb_engine::{AllowAllAuthorizer, DbResult, Engine, EngineBuilder};

use crate::api;
use crate::auth::{SessionSecret, SessionStore};

/// Dashboard HTTP server configuration.
#[derive(Clone, Debug)]
pub struct DashboardConfig {
    pub bind_address: String,
    pub port: u16,
    /// Maximum dashboard sessions (not DB connections).
    pub max_sessions: usize,
    /// Session idle timeout.
    pub session_timeout: Duration,
    /// Maximum SQL query length accepted via the API.
    pub max_query_length: usize,
    /// Maximum rows returned per query via the dashboard.
    pub max_result_rows: usize,
    /// Per-query execution timeout.
    pub query_timeout: Duration,
    /// Allow the unauthenticated Prometheus text endpoint.
    ///
    /// Disabled by default because a loopback-only bind is often paired with a
    /// local reverse proxy for remote access. In that topology, relying only on
    /// `peer_addr.is_loopback()` would expose `/api/metrics-prom` to remote
    /// clients via the proxy hop.
    pub allow_unauthenticated_metrics_prometheus: bool,
    /// Trust TLS termination headers from a local reverse proxy.
    ///
    /// Disabled by default. When disabled, any request that presents
    /// `Forwarded` / `X-Forwarded-*` headers is rejected by the login route so
    /// the dashboard does not silently accept password logins behind an
    /// untrusted or plaintext proxy hop.
    pub trust_proxy_tls_headers: bool,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_owned(),
            port: 8080,
            max_sessions: 64,
            session_timeout: Duration::from_secs(30 * 60),
            max_query_length: 64 * 1024,
            max_result_rows: 10_000,
            query_timeout: Duration::from_secs(30),
            allow_unauthenticated_metrics_prometheus: false,
            trust_proxy_tls_headers: false,
        }
    }
}

const DASHBOARD_MIN_REQUEST_BODY_BYTES: usize = 16 * 1024;
const DASHBOARD_MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;
const DASHBOARD_MIN_BLOCKING_REQUESTS: usize = 4;
const DASHBOARD_MAX_BLOCKING_REQUESTS: usize = 16;

/// Shared state accessible by all API handlers.
pub struct AppState {
    pub engine: Arc<Engine>,
    pub sessions: Arc<SessionStore>,
    pub secret: SessionSecret,
    pub config: DashboardConfig,
    pub blocking_ops: Arc<Semaphore>,
}

/// Credentials for the initial admin role created at bootstrap.
#[derive(Clone)]
pub struct BootstrapAdmin {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for BootstrapAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapAdmin")
            .field("username", &self.username)
            .field("password", &"**redacted**")
            .finish()
    }
}

pub struct DashboardServer {
    config: DashboardConfig,
    engine: Arc<Engine>,
}

/// Build the dashboard's embedded engine with catalog-backed authentication
/// enabled. Allow authenticated logins to establish sessions while leaving
/// SQL-level access control to the engine's catalog-backed privilege checks.
pub fn build_dashboard_engine() -> DbResult<Arc<Engine>> {
    let mut runtime = RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Staging);
    runtime.security.password_min_length = 12;
    runtime.security.reject_role_name_as_password = true;
    // Dashboard uses an embedded in-memory engine; keep lockout semantics
    // but avoid sharing persistent lockout state across ephemeral instances.
    runtime.security.durable_auth_lockout = false;
    runtime.security.max_session_idle_timeout = Some(Duration::from_secs(15 * 60));
    runtime.security.max_session_lifetime = Some(Duration::from_secs(4 * 60 * 60));
    runtime.security.max_concurrent_sessions_per_role = Some(8);
    runtime.security.max_transaction_idle_timeout = Some(Duration::from_secs(5 * 60));

    Ok(Arc::new(
        EngineBuilder::new_in_memory()
            .with_runtime_config(runtime)
            .with_authorizer(Arc::new(AllowAllAuthorizer))
            .build()?,
    ))
}

impl DashboardServer {
    pub fn new(engine: Arc<Engine>, config: DashboardConfig) -> Self {
        Self { config, engine }
    }

    /// Create the initial admin role inside the engine so that the first user
    /// can log in to the dashboard.
    pub fn bootstrap_admin(&self, admin: &BootstrapAdmin) -> Result<(), String> {
        self.engine
            .bootstrap_role(&admin.username, &admin.password, true)
            .map_err(|e| format!("bootstrap CREATE ROLE failed: {e}"))?;
        info!(user = %admin.username, "bootstrap admin role created");
        Ok(())
    }

    /// Start the dashboard HTTP server. Blocks until `shutdown_rx` fires.
    pub async fn start(self, shutdown_rx: watch::Receiver<bool>) -> Result<(), std::io::Error> {
        validate_dashboard_bind_address(&self.config)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::PermissionDenied, error))?;

        let secret = SessionSecret::generate().map_err(|error| {
            std::io::Error::other(format!(
                "failed to generate dashboard session secret: {error}"
            ))
        })?;
        let state = Arc::new(AppState {
            engine: Arc::clone(&self.engine),
            sessions: Arc::new(SessionStore::new(
                self.config.session_timeout,
                self.config.max_sessions,
                Arc::clone(&self.engine),
            )),
            secret,
            config: self.config.clone(),
            blocking_ops: Arc::new(Semaphore::new(blocking_request_limit(&self.config))),
        });

        let app = build_router(state);

        let addr = format_dashboard_listen_addr(&self.config.bind_address, self.config.port);
        info!(address = %addr, "dashboard server starting");

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(shutdown_rx))
        .await
    }
}

fn validate_dashboard_bind_address(config: &DashboardConfig) -> Result<(), String> {
    if dashboard_bind_address_is_loopback(&config.bind_address) {
        Ok(())
    } else {
        Err(
            "dashboard only supports loopback binds until transport encryption is configured; bind to 127.0.0.1/::1 and terminate TLS in front of it for remote access"
                .to_owned(),
        )
    }
}

fn dashboard_bind_address_is_loopback(bind_address: &str) -> bool {
    let trimmed = bind_address.trim();
    let normalized = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);
    normalized.eq_ignore_ascii_case("localhost")
        || normalized
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn format_dashboard_listen_addr(bind_address: &str, port: u16) -> String {
    let bind_address = bind_address.trim();
    if bind_address.contains(':') && !bind_address.starts_with('[') {
        format!("[{bind_address}]:{port}")
    } else {
        format!("{bind_address}:{port}")
    }
}

fn build_router(state: Arc<AppState>) -> Router {
    let csp = "default-src 'self'; script-src 'self'; style-src 'self'; \
               img-src 'self' data:; font-src 'self'; connect-src 'self'; \
               frame-ancestors 'none'; base-uri 'self'; form-action 'self'";
    let api_body_limit = request_body_limit_bytes(&state.config);

    let api_routes = api::routes().layer(DefaultBodyLimit::max(api_body_limit));
    let api_routes = api_routes
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store, private"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::PRAGMA,
            axum::http::HeaderValue::from_static("no-cache"),
        ));
    let static_routes = static_routes();

    Router::new()
        .nest("/api", api_routes)
        .merge(static_routes)
        .layer(CompressionLayer::new())
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(csp),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::X_FRAME_OPTIONS,
            axum::http::HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            axum::http::HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::REFERRER_POLICY,
            axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::STRICT_TRANSPORT_SECURITY,
            axum::http::HeaderValue::from_static("max-age=63072000; includeSubDomains"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("x-permitted-cross-domain-policies"),
            axum::http::HeaderValue::from_static("none"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("cross-origin-opener-policy"),
            axum::http::HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("cross-origin-resource-policy"),
            axum::http::HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("cross-origin-embedder-policy"),
            axum::http::HeaderValue::from_static("require-corp"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("permissions-policy"),
            axum::http::HeaderValue::from_static(
                "accelerometer=(), camera=(), geolocation=(), gyroscope=(), \
                 magnetometer=(), microphone=(), payment=(), usb=()",
            ),
        ))
        .with_state(state)
}

fn request_body_limit_bytes(config: &DashboardConfig) -> usize {
    config.max_query_length.saturating_add(8 * 1024).clamp(
        DASHBOARD_MIN_REQUEST_BODY_BYTES,
        DASHBOARD_MAX_REQUEST_BODY_BYTES,
    )
}

fn blocking_request_limit(config: &DashboardConfig) -> usize {
    config.max_sessions.clamp(
        DASHBOARD_MIN_BLOCKING_REQUESTS,
        DASHBOARD_MAX_BLOCKING_REQUESTS,
    )
}

fn static_routes() -> Router<Arc<AppState>> {
    use axum::response::{Html, IntoResponse};
    use axum::routing::get;

    let index_html = include_str!("../static/index.html");
    let style_css = include_str!("../static/style.css");
    let app_js = include_str!("../static/app.js");

    Router::new()
        .route("/", get(move || async move { Html(index_html) }))
        .route(
            "/style.css",
            get(move || async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    style_css,
                )
                    .into_response()
            }),
        )
        .route(
            "/app.js",
            get(move || async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    app_js,
                )
                    .into_response()
            }),
        )
}

async fn shutdown_signal(mut rx: watch::Receiver<bool>) {
    let _ = rx.changed().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_engine::{
        Credential, QueryEngine, SecretString, StartupParams, TransportInfo, TransportKind,
    };

    fn startup_password(user: &str, password: &str) -> StartupParams {
        StartupParams {
            database: "default".to_owned(),
            application_name: Some("dashboard-test".to_owned()),
            options: Default::default(),
            credential: Credential::CleartextPassword {
                user: user.to_owned(),
                password: SecretString::new(password.to_owned()),
            },
            transport: TransportInfo {
                kind: TransportKind::Network {
                    tls: false,
                    peer_addr: Some("127.0.0.1:8080".to_owned()),
                },
            },
        }
    }

    #[test]
    fn default_config_is_localhost() {
        let cfg = DashboardConfig::default();
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert!(cfg.max_sessions > 0);
        assert!(cfg.max_query_length > 0);
        assert!(cfg.max_result_rows > 0);
    }

    #[test]
    fn dashboard_bind_policy_accepts_only_loopback_addresses() {
        assert!(dashboard_bind_address_is_loopback("127.0.0.1"));
        assert!(dashboard_bind_address_is_loopback("::1"));
        assert!(dashboard_bind_address_is_loopback("[::1]"));
        assert!(dashboard_bind_address_is_loopback("localhost"));
        assert!(!dashboard_bind_address_is_loopback("0.0.0.0"));
        assert!(!dashboard_bind_address_is_loopback("::"));
        assert!(!dashboard_bind_address_is_loopback("[::1"));
        assert!(!dashboard_bind_address_is_loopback("example.com"));
    }

    #[test]
    fn dashboard_bind_policy_rejects_public_addresses_in_library() {
        let config = DashboardConfig {
            bind_address: "0.0.0.0".to_owned(),
            ..DashboardConfig::default()
        };
        let error = validate_dashboard_bind_address(&config)
            .expect_err("library must reject public dashboard binds");
        assert!(error.contains("loopback"));
    }

    #[test]
    fn dashboard_listen_addr_formats_ipv6_loopback() {
        assert_eq!(
            format_dashboard_listen_addr("127.0.0.1", 8080),
            "127.0.0.1:8080"
        );
        assert_eq!(format_dashboard_listen_addr("::1", 8080), "[::1]:8080");
        assert_eq!(format_dashboard_listen_addr("[::1]", 8080), "[::1]:8080");
    }

    #[test]
    fn bootstrap_admin_seeds_real_role_on_non_testing_engine() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (session, info) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("bootstrapped admin should authenticate");
        assert_eq!(info.identity.user, "admin");
        engine.terminate(session).expect("terminate admin session");

        let err = engine
            .startup(startup_password("ghost", "Secret123456"))
            .expect_err("unknown role should be rejected");
        assert!(
            err.to_string().contains("invalid user name or password"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn dashboard_engine_denies_role_creation_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited LOGIN PASSWORD 'Password123!'; \
                 REVOKE CREATE ON SCHEMA public FROM limited; \
                 REVOKE CREATE ON SCHEMA public FROM PUBLIC;",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "CREATE ROLE pwned SUPERUSER LOGIN")
            .expect_err("limited dashboard user must not create roles");
        let msg = err.to_string();
        assert!(
            msg.contains("superuser") || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_alter_system_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited2 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited2", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "ALTER SYSTEM SET work_mem = '64MB'")
            .expect_err("limited dashboard user must not alter system settings");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to ALTER SYSTEM") || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_comment_on_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited3 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited3", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "COMMENT ON ROLE admin IS 'pwned'")
            .expect_err("limited dashboard user must not modify global comments");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to COMMENT ON") || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_security_label_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited4 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited4", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "SECURITY LABEL ON ROLE admin IS 'pwned'")
            .expect_err("limited dashboard user must not mutate security labels");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to SECURITY LABEL")
                || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_database_ddl_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited5 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited5", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "CREATE DATABASE pwned_db")
            .expect_err("limited dashboard user must not create databases");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to CREATE, ALTER, or DROP DATABASE")
                || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_alter_role_rename_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE limited6 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup limited role");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited6", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "ALTER ROLE admin RENAME TO admin_pwned")
            .expect_err("limited dashboard user must not rename roles");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to ALTER ROLE") || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn dashboard_engine_denies_drop_role_to_non_superusers() {
        let engine = build_dashboard_engine().unwrap();
        let server = DashboardServer::new(engine.clone(), DashboardConfig::default());

        server
            .bootstrap_admin(&BootstrapAdmin {
                username: "admin".to_owned(),
                password: "Secret123456".to_owned(),
            })
            .expect("bootstrap admin");

        let (admin, _) = engine
            .startup(startup_password("admin", "Secret123456"))
            .expect("admin startup");
        engine
            .execute_sql(
                &admin,
                "CREATE ROLE doomed LOGIN PASSWORD 'Password123!'; \
                 CREATE ROLE limited7 LOGIN PASSWORD 'Password123!';",
            )
            .expect("setup roles");
        engine.terminate(admin).expect("terminate admin session");

        let (limited, _) = engine
            .startup(startup_password("limited7", "Password123!"))
            .expect("limited startup");
        let err = engine
            .execute_sql(&limited, "DROP ROLE doomed")
            .expect_err("limited dashboard user must not drop roles");
        let msg = err.to_string();
        assert!(
            msg.contains("must be superuser to DROP ROLE") || msg.contains("permission denied"),
            "unexpected error: {msg}"
        );
    }
}
