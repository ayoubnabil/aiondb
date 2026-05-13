//! TCP server that accepts `PostgreSQL` connections and delegates to
//! [`PgWireEngine`].
//!
//! # Lifecycle
//! 1. Bind to the configured address and port.
//! 2. Accept TCP connections, spawning a task per client.
//! 3. Each task runs a [`Connection`] through
//!    startup, message loop, and termination.
//! 4. Graceful shutdown via the `shutdown` token - the server stops accepting
//!    new connections and waits for active ones to finish (with a timeout).

use std::collections::HashMap;
use std::fmt::Write;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aiondb_config::{
    pgwire::{DEFAULT_PGWIRE_BIND_ADDRESS, DEFAULT_PGWIRE_PORT},
    runtime::DEFAULT_LIMITS_MAX_PORTALS,
    EnginePoolConfig,
};
use aiondb_engine::{DbError, PgWireEngine, SessionHandle, SqlState};
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use crate::codec::MessageWriter;
use crate::connection::Connection;
use crate::engine_pool::EnginePool;
use crate::messages;
use crate::tls::{self, NegotiatedStream, TlsConfig};

/// Default timeout when waiting for active connections during shutdown.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTION_READ_BUFFER_BYTES: usize = 16 * 1024;
static FALLBACK_CANCEL_SECRET_COUNTER: AtomicU32 = AtomicU32::new(1);

type CancelRegistryMap = HashMap<(u32, u32), SessionHandle>;
type ConnectionSlots = HashMap<String, u32>;

fn lock_connection_slots<'a, P>(
    connection_slots: &'a Arc<Mutex<ConnectionSlots>>,
    peer: &P,
    action: &str,
) -> Option<MutexGuard<'a, ConnectionSlots>>
where
    P: std::fmt::Display + ?Sized,
{
    match connection_slots.lock() {
        Ok(slots) => Some(slots),
        Err(error) => {
            warn!(peer = %peer, action = action, "connection slot mutex poisoned: {error}");
            None
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_CANCEL_SECRET_RNG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn fill_cancel_secret_bytes(buf: &mut [u8; 4]) -> Result<(), String> {
    #[cfg(test)]
    {
        let fail = FAIL_NEXT_CANCEL_SECRET_RNG.with(|flag| {
            let fail = flag.get();
            flag.set(false);
            fail
        });
        if fail {
            return Err("injected RNG failure".to_owned());
        }
    }

    getrandom::fill(buf).map_err(|error| error.to_string())
}

fn checked_deadline_after(timeout: Duration, timeout_name: &str) -> Option<tokio::time::Instant> {
    if timeout.is_zero() {
        return None;
    }
    match tokio::time::Instant::now().checked_add(timeout) {
        Some(deadline) => Some(deadline),
        None => {
            warn!(
                timeout_name,
                timeout_ms = timeout.as_millis(),
                "timeout too large to schedule safely; disabling deadline"
            );
            None
        }
    }
}

/// Generate a cryptographically random cancel secret key.
///
/// When `fail_on_weak_rng` is `true`, the function returns an error instead of
/// falling back to the predictable counter+time XOR derivation. Production
/// deployments should set this flag so that a broken RNG is surfaced
fn generate_cancel_secret(fail_on_weak_rng: bool) -> Result<u32, DbError> {
    let mut buf = [0u8; 4];
    match fill_cancel_secret_bytes(&mut buf) {
        Ok(()) => Ok(u32::from_ne_bytes(buf)),
        Err(error) => {
            if fail_on_weak_rng {
                return Err(DbError::internal(format!(
                    "getrandom failed and fail_on_weak_rng is enabled — \
                     refusing to generate a predictable cancel secret: {error}"
                )));
            }
            warn!(%error, "failed to generate cancel secret securely; using degraded fallback secret");
            // Mix multiple entropy sources through SHA-256 to make the
            // fallback secret harder to predict than a plain counter XOR.
            use sha2::{Digest, Sha256};
            let counter = FALLBACK_CANCEL_SECRET_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0u128, |duration| duration.as_nanos());
            let mut hasher = Sha256::new();
            hasher.update(counter.to_le_bytes());
            hasher.update(nanos.to_le_bytes());
            hasher.update(std::process::id().to_le_bytes());
            // ThreadId has no stable numeric accessor; use its Debug repr
            // as an additional entropy source.
            let tid = format!("{:?}", std::thread::current().id());
            hasher.update(tid.as_bytes());
            let hash = hasher.finalize();
            Ok(u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]))
        }
    }
}

#[cfg(test)]
fn inject_cancel_secret_rng_failure() {
    FAIL_NEXT_CANCEL_SECRET_RNG.with(|flag| flag.set(true));
}

/// Configuration for the pgwire server.
#[derive(Debug, Clone)]
pub struct PgWireConfig {
    pub bind_address: String,
    pub port: u16,
    /// Maximum number of concurrently active TCP connections.
    pub max_connections: u32,
    /// Maximum number of concurrently active TCP connections per source IP.
    pub max_connections_per_ip: u32,
    /// Maximum time allowed to complete the startup handshake.
    pub startup_timeout: Duration,
    /// Maximum time to wait for active connections to finish during shutdown.
    pub shutdown_timeout: Duration,
    /// Delay applied before responding to authentication failures at startup.
    pub auth_failure_backoff: Duration,
    /// Bounded blocking dispatcher used for engine calls from the async server.
    pub engine_pool: EnginePoolConfig,
    /// Optional TLS configuration. If set, the server will accept SSL connections.
    pub tls: Option<TlsConfig>,
    /// Maximum time a connection may remain idle (waiting for client messages)
    /// before being closed. `Duration::ZERO` disables the timeout.
    pub idle_timeout: Duration,
    /// When `true`, plaintext (non-TLS) connections are rejected.
    /// Enforcement is handled by the TLS negotiation layer.
    pub require_tls: bool,
    /// When `true`, cancel-secret generation panics instead of falling back to
    /// a predictable counter+time XOR derivation when `getrandom` fails.
    /// Production deployments should enable this flag.
    pub fail_on_weak_rng: bool,
    /// Maximum number of concurrently open portals per connection.
    pub max_portals: usize,
}

impl Default for PgWireConfig {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_PGWIRE_BIND_ADDRESS.to_string(),
            port: DEFAULT_PGWIRE_PORT,
            max_connections: 128,
            max_connections_per_ip: 128,
            startup_timeout: Duration::from_secs(5),
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            auth_failure_backoff: Duration::from_millis(250),
            idle_timeout: Duration::from_secs(60 * 5),
            engine_pool: EnginePoolConfig::default(),
            tls: None,
            require_tls: true,
            fail_on_weak_rng: true,
            max_portals: DEFAULT_LIMITS_MAX_PORTALS,
        }
    }
}

/// Registry mapping `(pid, secret_key)` to a [`SessionHandle`], used for
/// `CancelRequest` handling on separate connections.
#[derive(Debug, Default, Clone)]
pub struct CancelRegistry {
    inner: Arc<Mutex<CancelRegistryMap>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_inner(&self, pid: u32, action: &str) -> Option<MutexGuard<'_, CancelRegistryMap>> {
        match self.inner.lock() {
            Ok(map) => Some(map),
            Err(error) => {
                warn!(
                    pid,
                    action = action,
                    "cancel registry mutex poisoned: {error}"
                );
                None
            }
        }
    }

    /// Register a `(pid, secret_key) -> SessionHandle` mapping.
    pub fn register(&self, pid: u32, secret_key: u32, session: SessionHandle) {
        if let Some(mut map) = self.lock_inner(pid, "register") {
            map.insert((pid, secret_key), session);
        }
    }

    /// Remove a previously registered mapping.
    pub fn unregister(&self, pid: u32, secret_key: u32) {
        if let Some(mut map) = self.lock_inner(pid, "unregister") {
            map.remove(&(pid, secret_key));
        }
    }

    /// Look up a session handle by `(pid, secret_key)`.
    pub fn lookup(&self, pid: u32, secret_key: u32) -> Option<SessionHandle> {
        self.lock_inner(pid, "lookup")
            .and_then(|map| map.get(&(pid, secret_key)).cloned())
    }
}

/// Basic server metrics exposed as atomic counters.
#[derive(Debug, Default)]
pub struct ServerMetrics {
    /// Total number of connections accepted since startup.
    pub total_connections: AtomicU64,
    /// Number of currently active connections.
    pub active_connections: AtomicU64,
    /// Total number of queries executed across all connections.
    pub total_queries: AtomicU64,
    /// Number of successful startup handshakes that established a session.
    pub successful_startups: AtomicU64,
    /// Number of startup attempts that failed before a session was established.
    pub failed_startups: AtomicU64,
    /// Number of startup failures caused by authentication/authorization.
    pub authentication_failures: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ServerMetricsSnapshot {
    pub total_connections: u64,
    pub active_connections: u64,
    pub total_queries: u64,
    pub successful_startups: u64,
    pub failed_startups: u64,
    pub authentication_failures: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerHealthState {
    Idle,
    Ready,
    Draining,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerHealthSnapshot {
    pub state: ServerHealthState,
    pub accepting_connections: bool,
    pub active_connections: u64,
}

impl ServerHealthSnapshot {
    pub fn is_ready(&self) -> bool {
        self.state == ServerHealthState::Ready
    }

    /// Render the health snapshot as a stable JSON document for embedding and health checks.
    pub fn to_json_string(&self) -> String {
        serde_json::json!({
            "state": self.state.as_str(),
            "accepting_connections": self.accepting_connections,
            "active_connections": self.active_connections,
            "ready": self.is_ready(),
        })
        .to_string()
    }
}

impl ServerHealthState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Ready => "ready",
            Self::Draining => "draining",
        }
    }
}

impl ServerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }

    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    pub fn total_queries(&self) -> u64 {
        self.total_queries.load(Ordering::Relaxed)
    }

    pub fn successful_startups(&self) -> u64 {
        self.successful_startups.load(Ordering::Relaxed)
    }

    pub fn failed_startups(&self) -> u64 {
        self.failed_startups.load(Ordering::Relaxed)
    }

    pub fn authentication_failures(&self) -> u64 {
        self.authentication_failures.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> ServerMetricsSnapshot {
        ServerMetricsSnapshot {
            total_connections: self.total_connections(),
            active_connections: self.active_connections(),
            total_queries: self.total_queries(),
            successful_startups: self.successful_startups(),
            failed_startups: self.failed_startups(),
            authentication_failures: self.authentication_failures(),
        }
    }

    pub(crate) fn record_startup_success(&self) {
        self.successful_startups.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_startup_failure(&self, error: &DbError) {
        self.failed_startups.fetch_add(1, Ordering::Relaxed);
        if matches!(
            error.sqlstate(),
            SqlState::InvalidAuthorizationSpecification | SqlState::TooManyAuthenticationFailures
        ) {
            self.authentication_failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl ServerMetricsSnapshot {
    /// Render the metrics snapshot as a stable JSON document for embedding and debug endpoints.
    pub fn to_json_string(&self) -> String {
        serde_json::json!({
            "total_connections": self.total_connections,
            "active_connections": self.active_connections,
            "total_queries": self.total_queries,
            "successful_startups": self.successful_startups,
            "failed_startups": self.failed_startups,
            "authentication_failures": self.authentication_failures,
        })
        .to_string()
    }

    /// Render the metrics snapshot using the Prometheus text exposition format.
    pub fn to_prometheus_text(&self) -> String {
        let mut output = String::new();
        write_counter_metric(
            &mut output,
            "aiondb_pgwire_connections_total",
            "Total number of accepted PostgreSQL wire connections since startup.",
            self.total_connections,
        );
        write_gauge_metric(
            &mut output,
            "aiondb_pgwire_connections_active",
            "Number of currently active PostgreSQL wire connections.",
            self.active_connections,
        );
        write_counter_metric(
            &mut output,
            "aiondb_pgwire_queries_total",
            "Total number of queries executed across all PostgreSQL wire connections.",
            self.total_queries,
        );
        write_counter_metric(
            &mut output,
            "aiondb_pgwire_successful_startups_total",
            "Total number of successful PostgreSQL wire startup handshakes.",
            self.successful_startups,
        );
        write_counter_metric(
            &mut output,
            "aiondb_pgwire_failed_startups_total",
            "Total number of failed PostgreSQL wire startup attempts.",
            self.failed_startups,
        );
        write_counter_metric(
            &mut output,
            "aiondb_pgwire_authentication_failures_total",
            "Total number of PostgreSQL wire startup failures caused by authentication or authorization.",
            self.authentication_failures,
        );
        output
    }
}

// `writeln!` to a `String` is infallible; `let _ =` silences unused-result warnings.

fn write_counter_metric(output: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name} {value}");
}

fn write_gauge_metric(output: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value}");
}

/// The pgwire server. Generic over the query engine implementation.
pub struct PgWireServer<E: PgWireEngine + 'static> {
    engine: Arc<E>,
    engine_pool: EnginePool<E>,
    config: PgWireConfig,
    next_pid: AtomicU32,
    cancel_registry: CancelRegistry,
    tls_acceptor: Option<TlsAcceptor>,
    metrics: Arc<ServerMetrics>,
    accepting_connections: AtomicBool,
    draining: AtomicBool,
    connection_slots: Arc<Mutex<HashMap<String, u32>>>,
}

impl<E: PgWireEngine + 'static> PgWireServer<E> {
    fn new_with_tls(
        engine: Arc<E>,
        mut config: PgWireConfig,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Self {
        if config.max_connections == 0 {
            warn!(
                configured = 0,
                normalized = 1,
                "pgwire max_connections must be >= 1; normalizing invalid value"
            );
            config.max_connections = 1;
        }
        if config.max_connections_per_ip == 0 {
            warn!(
                configured = 0,
                normalized = 1,
                "pgwire max_connections_per_ip must be >= 1; normalizing invalid value"
            );
            config.max_connections_per_ip = 1;
        }
        if config.max_connections_per_ip > config.max_connections {
            warn!(
                configured = config.max_connections_per_ip,
                normalized = config.max_connections,
                max_connections = config.max_connections,
                "pgwire max_connections_per_ip exceeds max_connections; clamping to global limit"
            );
            config.max_connections_per_ip = config.max_connections;
        }

        let engine_pool = EnginePool::new(Arc::clone(&engine), config.engine_pool.clone());
        Self {
            engine,
            engine_pool,
            config,
            next_pid: AtomicU32::new(1),
            cancel_registry: CancelRegistry::new(),
            tls_acceptor,
            metrics: Arc::new(ServerMetrics::new()),
            accepting_connections: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            connection_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a new server. If TLS is configured, builds the TLS acceptor
    /// eagerly so cert/key errors surface at startup.
    pub fn new(engine: Arc<E>, config: PgWireConfig) -> std::io::Result<Self> {
        let tls_acceptor = config
            .tls
            .as_ref()
            .map(tls::build_tls_acceptor)
            .transpose()?;
        Ok(Self::new_with_tls(engine, config, tls_acceptor))
    }

    /// Create a new server without TLS (convenience for tests).
    pub fn new_plain(engine: Arc<E>, config: PgWireConfig) -> Self {
        Self::new_with_tls(engine, config, None)
    }

    fn try_acquire_connection_slot(&self, peer: &SocketAddr) -> Option<String> {
        let peer_ip = peer.ip().to_string();
        let mut slots = lock_connection_slots(&self.connection_slots, peer, "acquire")?;

        // Check global limit inside the mutex to prevent TOCTOU races.
        let active_connections = self.metrics.active_connections();
        if active_connections >= u64::from(self.config.max_connections) {
            warn!(
                %peer,
                active_connections,
                max_connections = self.config.max_connections,
                "connection rejected: global connection limit reached"
            );
            return None;
        }

        let current = slots.get(&peer_ip).copied().unwrap_or(0);
        if current >= self.config.max_connections_per_ip {
            warn!(
                %peer,
                peer_ip = %peer_ip,
                max_connections_per_ip = self.config.max_connections_per_ip,
                "connection rejected: per-IP connection limit reached"
            );
            return None;
        }

        slots.insert(peer_ip.clone(), current + 1);
        self.metrics
            .total_connections
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .active_connections
            .fetch_add(1, Ordering::Relaxed);
        Some(peer_ip)
    }

    async fn reject_connection_with_error(mut stream: TcpStream, peer: SocketAddr, error: DbError) {
        let mut writer = MessageWriter::new();
        messages::write_error_response(&mut writer, &error);
        if let Err(write_error) = writer.flush(&mut stream).await {
            warn!(
                %peer,
                error = %write_error,
                "failed to send pgwire connection rejection"
            );
            return;
        }
        if let Err(shutdown_error) = stream.shutdown().await {
            warn!(
                %peer,
                error = %shutdown_error,
                "failed to shutdown rejected pgwire connection"
            );
        }
    }

    fn release_connection_slot(
        metrics: &Arc<ServerMetrics>,
        connection_slots: &Arc<Mutex<ConnectionSlots>>,
        peer_ip: Option<&str>,
    ) {
        metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
        if let Some(peer_ip) = peer_ip {
            if let Some(mut slots) = lock_connection_slots(connection_slots, peer_ip, "release") {
                match slots.get_mut(peer_ip) {
                    Some(count) if *count > 1 => *count -= 1,
                    Some(_) => {
                        slots.remove(peer_ip);
                    }
                    None => {}
                }
            }
        }
    }

    /// Returns a reference to the cancel registry (useful for testing).
    pub fn cancel_registry(&self) -> &CancelRegistry {
        &self.cancel_registry
    }

    /// Returns a reference to server metrics.
    pub fn metrics(&self) -> &Arc<ServerMetrics> {
        &self.metrics
    }

    /// Returns the engine served by this PgWire instance.
    pub fn engine(&self) -> &Arc<E> {
        &self.engine
    }

    /// Returns a point-in-time snapshot of server metrics.
    pub fn metrics_snapshot(&self) -> ServerMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns the current metrics snapshot encoded as JSON.
    pub fn metrics_json(&self) -> String {
        self.metrics_snapshot().to_json_string()
    }

    /// Returns the current metrics snapshot encoded in Prometheus text format.
    pub fn metrics_prometheus_text(&self) -> String {
        self.metrics_snapshot().to_prometheus_text()
    }

    /// Returns a point-in-time health snapshot for embedding/monitoring.
    pub fn health_snapshot(&self) -> ServerHealthSnapshot {
        let accepting_connections = self.accepting_connections.load(Ordering::Acquire);
        let state = if self.draining.load(Ordering::Acquire) {
            ServerHealthState::Draining
        } else if accepting_connections {
            ServerHealthState::Ready
        } else {
            ServerHealthState::Idle
        };

        ServerHealthSnapshot {
            state,
            accepting_connections,
            active_connections: self.metrics.active_connections(),
        }
    }

    /// Returns the current health snapshot encoded as JSON.
    pub fn health_json(&self) -> String {
        self.health_snapshot().to_json_string()
    }

    /// Start the server. Runs until the shutdown signal is received.
    ///
    /// After receiving the shutdown signal the server stops accepting new
    /// connections and waits up to `config.shutdown_timeout` for active
    /// connections to finish before returning.
    pub async fn start(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), std::io::Error> {
        let addr = format!("{}:{}", self.config.bind_address, self.config.port);
        let listener = TcpListener::bind(&addr).await?;
        self.draining.store(false, Ordering::Release);
        self.accepting_connections.store(true, Ordering::Release);
        info!("pgwire server listening on {addr}");

        // Track active connection tasks so we can wait for them on shutdown.
        let mut active_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        loop {
            // Clean up already-finished tasks before each iteration.
            active_tasks.retain(|h| !h.is_finished());

            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer)) => {
                            if let Err(error) = stream.set_nodelay(true) {
                                warn!(%peer, %error, "failed to enable TCP_NODELAY on client socket");
                            }
                            let Some(peer_ip) = self.try_acquire_connection_slot(&peer) else {
                                let error = DbError::authorization_error(
                                    SqlState::TooManyConnections,
                                    "too many pgwire connections; increase AIONDB_PGWIRE_MAX_CONNECTIONS or reduce client pool size",
                                );
                                self.metrics.record_startup_failure(&error);
                                Self::reject_connection_with_error(stream, peer, error).await;
                                continue;
                            };
                            let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
                            let secret = match generate_cancel_secret(self.config.fail_on_weak_rng) {
                                Ok(secret) => secret,
                                Err(error) => {
                                    error!(
                                        pid,
                                        %peer,
                                        error = %error,
                                        "failed to generate cancel secret"
                                    );
                                    self.metrics.record_startup_failure(&error);
                                    Self::release_connection_slot(
                                        &self.metrics,
                                        &self.connection_slots,
                                        Some(peer_ip.as_str()),
                                    );
                                    continue;
                                }
                            };
                            let engine = Arc::clone(&self.engine);
                            let engine_pool = self.engine_pool.clone();
                            let registry = self.cancel_registry.clone();
                            let tls_acceptor = self.tls_acceptor.clone();
                            let metrics = Arc::clone(&self.metrics);
                            let connection_slots = Arc::clone(&self.connection_slots);
                            let auth_failure_backoff = self.config.auth_failure_backoff;
                            let startup_timeout = self.config.startup_timeout;
                            let idle_timeout = self.config.idle_timeout;
                            let startup_deadline =
                                checked_deadline_after(startup_timeout, "server startup timeout");
                            let peer_addr = peer.to_string();
                            info!(pid, %peer, "accepted connection");

                            let require_tls = self.config.require_tls;
                            let max_portals = self.config.max_portals;
                            let handle = tokio::spawn(async move {
                                let tls_result = if let Some(deadline) = startup_deadline {
                                    match tokio::time::timeout_at(
                                        deadline,
                                        tls::negotiate_tls(stream, tls_acceptor.as_ref(), require_tls),
                                    )
                                    .await
                                    {
                                        Ok(result) => result,
                                        Err(_) => {
                                            let error = DbError::protocol(format!(
                                                "startup timeout exceeded after {} ms",
                                                startup_timeout.as_millis()
                                            ));
                                            metrics.record_startup_failure(&error);
                                            error!(pid, error = %error, "TLS negotiation timed out");
                                            Self::release_connection_slot(
                                                &metrics,
                                                &connection_slots,
                                                Some(peer_ip.as_str()),
                                            );
                                            return;
                                        }
                                    }
                                } else {
                                    tls::negotiate_tls(stream, tls_acceptor.as_ref(), require_tls).await
                                };
                                match tls_result {
                                    Ok(NegotiatedStream::Plain { stream, startup_bytes }) => {
                                        let (reader, writer) = tokio::io::split(stream);
                                        let prepend = tls::PrependReader::new(startup_bytes, reader);
                                        let reader = BufReader::with_capacity(
                                            CONNECTION_READ_BUFFER_BYTES,
                                            prepend,
                                        );
                                        let mut conn = Connection::new(
                                            engine, reader, writer, pid, secret, registry,
                                        );
                                        conn.set_engine_pool(engine_pool.clone());
                                        conn.set_tls(false);
                                        conn.set_auth_failure_backoff(auth_failure_backoff);
                                        conn.set_startup_timeout(startup_timeout);
                                        conn.set_startup_deadline(startup_deadline);
                                        conn.set_idle_timeout(idle_timeout);
                                        conn.set_max_portals(max_portals);
                                        conn.set_peer_addr(Some(peer_addr.clone()));
                                        conn.set_metrics(Arc::clone(&metrics));
                                        if let Err(e) = conn.run().await {
                                            error!(pid, error = %e, "connection error");
                                        }
                                    }
                                    Ok(NegotiatedStream::Tls(tls_stream)) => {
                                        let (reader, writer) = tokio::io::split(*tls_stream);
                                        let reader = BufReader::with_capacity(
                                            CONNECTION_READ_BUFFER_BYTES,
                                            reader,
                                        );
                                        let mut conn = Connection::new(
                                            engine, reader, writer, pid, secret, registry,
                                        );
                                        conn.set_engine_pool(engine_pool.clone());
                                        conn.set_tls(true);
                                        conn.set_auth_failure_backoff(auth_failure_backoff);
                                        conn.set_startup_timeout(startup_timeout);
                                        conn.set_startup_deadline(startup_deadline);
                                        conn.set_idle_timeout(idle_timeout);
                                        conn.set_max_portals(max_portals);
                                        conn.set_peer_addr(Some(peer_addr.clone()));
                                        conn.set_metrics(Arc::clone(&metrics));
                                        if let Err(e) = conn.run().await {
                                            error!(pid, error = %e, "connection error (TLS)");
                                        }
                                    }
                                    Err(e) => {
                                        error!(pid, error = %e, "TLS negotiation failed");
                                    }
                                }
                                Self::release_connection_slot(
                                    &metrics,
                                    &connection_slots,
                                    Some(peer_ip.as_str()),
                                );
                            });
                            active_tasks.push(handle);
                        }
                        Err(e) => {
                            error!("accept error: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        self.accepting_connections.store(false, Ordering::Release);
                        self.draining.store(true, Ordering::Release);
                        info!("pgwire server shutting down");
                        break;
                    }
                }
            }
        }

        // --- Graceful shutdown: wait for active connections with timeout ---
        active_tasks.retain(|h| !h.is_finished());
        if !active_tasks.is_empty() {
            info!(
                count = active_tasks.len(),
                "waiting for active connections to finish"
            );

            let deadline = tokio::time::sleep(self.config.shutdown_timeout);
            tokio::pin!(deadline);

            loop {
                if active_tasks.is_empty() {
                    break;
                }

                tokio::select! {
                    () = &mut deadline => {
                        warn!(
                            remaining = active_tasks.len(),
                            "shutdown timeout reached, aborting remaining connections"
                        );
                        for h in &active_tasks {
                            h.abort();
                        }
                        break;
                    }
                    () = async {
                        // Wait for any one task to finish.
                        for task in &mut active_tasks {
                            if task.is_finished() {
                                return;
                            }
                        }
                        // If none are finished yet, yield and re-check.
                        tokio::task::yield_now().await;
                    } => {
                        active_tasks.retain(|h| !h.is_finished());
                    }
                }
            }
        }

        self.draining.store(false, Ordering::Release);
        info!("pgwire server stopped");
        Ok(())
    }

    /// Start the server without a shutdown signal (runs forever).
    pub async fn start_forever(&self) -> Result<(), std::io::Error> {
        let (_tx, rx) = watch::channel(false);
        self.start(rx).await
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
