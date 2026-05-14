//! Async TCP client for remote fragment execution.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use aiondb_core::{DbError, DbResult};
use aiondb_executor::ExecutionResult;
use aiondb_plan::PhysicalPlan;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tracing::{debug, warn};

use crate::auth::AuthToken;
use crate::protocol::{
    self, CancelRequest, FragmentRequest, FragmentResponse, FragmentSnapshot, TransportEnvelope,
    TransportPayload, PROTOCOL_VERSION,
};
use crate::tls::TlsClientConfig;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_millis(500);
const DEFAULT_MAX_IDLE_PER_HOST: usize = 8;

fn connection_pool_key(config: &FragmentClientConfig) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    config.addr.hash(&mut hasher);
    match &config.tls {
        Some(tls) => {
            "tls".hash(&mut hasher);
            tls.ca_cert_path.hash(&mut hasher);
            tls.client_cert_path.hash(&mut hasher);
            tls.client_key_path.hash(&mut hasher);
        }
        None => {
            "plain".hash(&mut hasher);
        }
    }
    // Mix the auth-token bytes in so two clients sharing the same address+TLS
    // identity but different auth tokens never share a TCP/TLS pooled
    // connection (audit fragment-transport F4 defence-in-depth). The
    // per-envelope auth check still rejects mismatches at the protocol
    // layer; this just keeps the pool partition tenant-aware.
    config.auth_token.as_str().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn is_retryable_pooled_connection_error(error: &DbError) -> bool {
    let DbError::Protocol(report) = error else {
        return false;
    };
    matches!(
        report.message.to_ascii_lowercase().as_str(),
        msg if msg.starts_with("write frame:")
            || msg.starts_with("flush:")
            || msg.starts_with("read header:")
            || msg.starts_with("read payload:")
    )
}

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------

/// A reusable TCP connection (plain or TLS-encrypted).
struct PooledConnection {
    stream: MaybeEncrypted,
}

/// Thread-safe pool of reusable TCP connections keyed by remote address.
///
/// Connections are lazily created by [`FragmentClient`] and cached here for
/// reuse across fragment requests to the same node.  Broken or stale
/// connections are removed automatically: the client clears the pool entry
/// for an address whenever an I/O error occurs on a connection obtained from
/// this pool.
///
/// The internal lock is [`std::sync::Mutex`] (not `tokio::Mutex`) because the
/// critical section is extremely short (a `Vec::pop` or `Vec::push`), so it
/// will never actually block a tokio worker thread.
pub struct ConnectionPool {
    /// Available idle connections per address.
    pool: Mutex<HashMap<String, Vec<PooledConnection>>>,
    /// Maximum number of idle connections to keep per address.
    max_idle_per_host: usize,
}

impl ConnectionPool {
    /// Create a new pool allowing up to `max_idle_per_host` idle connections
    /// per remote address.
    #[must_use]
    pub fn new(max_idle_per_host: usize) -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
            max_idle_per_host,
        }
    }

    /// Create a pool with the default idle-per-host limit.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_IDLE_PER_HOST)
    }

    /// Take an idle connection for `addr`, if one is available.
    ///
    /// Returns `None` when the pool has no idle connections for the address.
    fn take(&self, addr: &str) -> Option<PooledConnection> {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.get_mut(addr).and_then(Vec::pop)
    }

    /// Return a connection to the pool after successful use.
    ///
    /// If the pool for `addr` is already at capacity the connection is
    fn put(&self, addr: &str, conn: PooledConnection) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let conns = pool.entry(addr.to_owned()).or_default();
        if conns.len() < self.max_idle_per_host {
            conns.push(conn);
        }
        // else: connection is dropped here, closing TCP
    }

    /// Remove **all** idle connections for `addr`.
    ///
    /// Called when an error occurs on a connection to this address so that
    /// other callers do not reuse potentially-broken connections.
    fn clear(&self, addr: &str) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.remove(addr);
    }

    /// Number of idle connections currently held for `addr`.
    #[cfg(test)]
    fn idle_count(&self, addr: &str) -> usize {
        let pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.get(addr).map_or(0, Vec::len)
    }
}

// ---------------------------------------------------------------------------
// Stream wrapper: plain TCP or TLS-upgraded
// ---------------------------------------------------------------------------

/// A TCP stream that may or may not be TLS-encrypted.
enum MaybeEncrypted {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    #[cfg(test)]
    Mock(tokio::io::DuplexStream),
}

impl AsyncRead for MaybeEncrypted {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(test)]
            Self::Mock(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeEncrypted {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(test)]
            Self::Mock(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
            #[cfg(test)]
            Self::Mock(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(test)]
            Self::Mock(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Configuration for connecting to a remote fragment execution node.
#[derive(Clone)]
pub struct FragmentClientConfig {
    /// Address of the remote node (e.g. `"host:port"`).
    pub addr: String,
    /// Shared-secret token used for inter-node authentication.
    pub auth_token: AuthToken,
    /// Optional TLS configuration for encrypted connections.
    pub tls: Option<TlsClientConfig>,
    /// Timeout for establishing a TCP connection.
    pub connect_timeout: Duration,
    /// Maximum number of connection retry attempts.
    pub max_retries: u32,
    /// Base backoff duration between retries (doubled each attempt).
    pub retry_backoff: Duration,
}

impl std::fmt::Debug for FragmentClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentClientConfig")
            .field("addr", &self.addr)
            .field("auth_token", &self.auth_token)
            .field("tls", &self.tls.is_some())
            .field("connect_timeout", &self.connect_timeout)
            .field("max_retries", &self.max_retries)
            .field("retry_backoff", &self.retry_backoff)
            .finish()
    }
}

impl FragmentClientConfig {
    /// Create a new config with the given address and auth token, using
    /// default values for timeout, retries, and backoff.
    #[must_use]
    pub fn new(addr: impl Into<String>, auth_token: AuthToken) -> Self {
        Self {
            addr: addr.into(),
            auth_token,
            tls: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            retry_backoff: DEFAULT_RETRY_BACKOFF,
        }
    }

    /// Set the TLS configuration for this client.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsClientConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Override the connection timeout.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Override the maximum number of connection retries.
    #[must_use]
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Override the base retry backoff duration.
    #[must_use]
    pub fn with_retry_backoff(mut self, backoff: Duration) -> Self {
        self.retry_backoff = backoff;
        self
    }
}

/// Serializable execution metadata sent alongside a fragment plan.
#[derive(Clone, Debug)]
pub struct FragmentContext {
    /// Transaction identifier on the coordinator.
    pub txn_id: u64,
    /// Transaction isolation level (e.g. `"read committed"`).
    pub isolation: String,
    /// Maximum number of result rows the remote node should return.
    pub max_result_rows: u64,
    /// Maximum total bytes of result data.
    pub max_result_bytes: u64,
    /// Memory budget for this fragment execution.
    pub max_memory_bytes: u64,
    /// Temporary storage budget for this fragment execution.
    pub max_temp_bytes: u64,
    /// Optional MVCC snapshot selected by the coordinator.
    pub snapshot: Option<FragmentSnapshot>,
    /// Optional shard identifier for shard-scoped fragment execution.
    pub shard_id: Option<u32>,
    /// Optional absolute deadline as milliseconds since the Unix epoch.
    pub deadline_epoch_ms: Option<u64>,
}

/// Async TCP client for shipping query fragments to remote execution nodes.
///
/// Thread-safe: request IDs are generated atomically, and the client holds
/// no mutable connection state between calls.  An optional
/// [`ConnectionPool`] can be attached to reuse TCP connections across
/// requests to the same node, avoiding the cost of repeated TCP (and TLS)
/// handshakes.
pub struct FragmentClient {
    config: FragmentClientConfig,
    next_request_id: AtomicU64,
    pool_key: String,
    /// Optional shared connection pool.  When present, `execute()` tries to
    /// reuse an idle connection before opening a new one, and returns
    /// connections to the pool after a successful exchange.
    pool: Option<Arc<ConnectionPool>>,
}

impl FragmentClient {
    /// Create a new fragment client with the given configuration.
    #[must_use]
    pub fn new(config: FragmentClientConfig) -> Self {
        let pool_key = connection_pool_key(&config);
        Self {
            config,
            next_request_id: AtomicU64::new(1),
            pool_key,
            pool: None,
        }
    }

    /// Attach a shared connection pool.
    ///
    /// When a pool is present the client will try to reuse idle connections
    /// before creating new ones, and will return connections to the pool
    /// after a successful request/response exchange.
    #[must_use]
    pub fn with_connection_pool(mut self, pool: Arc<ConnectionPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    fn allocate_request_id(&self) -> DbResult<u64> {
        self.next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current == 0 {
                    None
                } else {
                    current.checked_add(1)
                }
            })
            .map_err(|_| DbError::internal("fragment request id space exhausted"))
    }

    /// Execute a physical plan fragment on the remote node.
    ///
    /// Connects (with retry), sends the fragment request, and waits for
    /// the response. If `deadline_epoch_ms` is set in the context, the
    /// entire operation is wrapped in a timeout.
    ///
    /// # Errors
    ///
    /// Returns `DbError` on connection failure, protocol error, timeout,
    /// or if the remote node reports an execution error.
    pub async fn execute(
        &self,
        plan: &PhysicalPlan,
        context: FragmentContext,
    ) -> DbResult<ExecutionResult> {
        let request_id = self.allocate_request_id()?;
        // Pseudo-random cancel-key derived from process-local entropy so a
        // peer who only learns `request_id` cannot synthesize a valid
        // cancel without seeing the original request envelope.
        let cancel_key = generate_cancel_key();
        debug!(request_id, addr = %self.config.addr, "sending fragment request");

        let request = FragmentRequest {
            request_id,
            plan: plan.clone(),
            txn_id: context.txn_id,
            isolation: context.isolation,
            max_result_rows: context.max_result_rows,
            max_result_bytes: context.max_result_bytes,
            max_memory_bytes: context.max_memory_bytes,
            max_temp_bytes: context.max_temp_bytes,
            snapshot: context.snapshot,
            deadline_epoch_ms: context.deadline_epoch_ms,
            shard_id: context.shard_id,
            cancel_key,
        };

        let envelope = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: self.config.auth_token.as_str().to_owned(),
            payload: TransportPayload::ExecuteFragment(Box::new(request)),
        };

        if let Some(deadline_ms) = context.deadline_epoch_ms {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let remaining = u128::from(deadline_ms).saturating_sub(now_ms);
            if remaining == 0 {
                return Err(DbError::query_canceled("fragment deadline already expired"));
            }
            let timeout_dur = Duration::from_millis(u64::try_from(remaining).unwrap_or(u64::MAX));
            match tokio::time::timeout(timeout_dur, self.send_and_receive(envelope)).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    warn!(request_id, "fragment execution timed out");
                    Err(DbError::query_canceled("fragment execution timed out"))
                }
            }
        } else {
            self.send_and_receive(envelope).await
        }
    }

    /// Send a cancellation request for an in-flight fragment execution.
    ///
    /// The caller must provide a mutable reference to the stream that was
    /// used for the original request.
    ///
    /// # Errors
    ///
    /// Returns `DbError` on protocol or I/O failure, or if the remote node
    /// does not acknowledge the cancellation.
    pub async fn cancel<S>(&self, stream: &mut S, request_id: u64, cancel_key: u64) -> DbResult<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        debug!(request_id, "sending cancel request");

        let envelope = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: self.config.auth_token.as_str().to_owned(),
            payload: TransportPayload::CancelFragment(CancelRequest {
                request_id,
                cancel_key,
            }),
        };

        protocol::write_envelope(stream, &envelope).await?;
        let response_envelope = protocol::read_envelope(stream).await?;

        match response_envelope.payload {
            TransportPayload::FragmentResult(FragmentResponse::CancelAck {
                request_id: ack_id,
            }) => {
                if ack_id != request_id {
                    return Err(DbError::protocol(format!(
                        "cancel ack request_id mismatch: expected {request_id}, got {ack_id}"
                    )));
                }
                debug!(request_id, "cancel acknowledged");
                Ok(())
            }
            TransportPayload::FragmentResult(FragmentResponse::Error { message, .. }) => {
                Err(DbError::protocol(format!("cancel rejected: {message}")))
            }
            other => Err(DbError::protocol(format!(
                "unexpected response to cancel: {other:?}"
            ))),
        }
    }

    /// Connect, send an envelope, read the response, and decode it.
    ///
    /// When a connection pool is available the method first tries an idle
    /// connection.  If the exchange fails on a *reused* connection (e.g. the
    /// remote end closed the socket), the pool is cleared and a single
    /// retry with a fresh connection is attempted.
    async fn send_and_receive(&self, envelope: TransportEnvelope) -> DbResult<ExecutionResult> {
        // -----------------------------------------------------------------
        // Attempt 1 – try a pooled connection if available
        // -----------------------------------------------------------------
        let (mut stream, from_pool) = match self.acquire_connection().await {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        };

        match self.exchange(&mut stream, &envelope).await {
            Ok(result) => {
                // Success – return the connection to the pool for reuse.
                self.release_connection(stream);
                return Ok(result);
            }
            Err(e) if from_pool => {
                if !is_retryable_pooled_connection_error(&e) {
                    return Err(e);
                }
                // The reused connection might have been stale.  Clear the
                // pool and retry once with a brand-new connection.
                debug!(
                    addr = %self.config.addr,
                    error = %e,
                    "pooled connection failed with transport I/O, retrying with a new connection"
                );
                if let Some(ref pool) = self.pool {
                    pool.clear(&self.pool_key);
                }
            }
            Err(e) => {
                // Fresh connection failed – clear pool just in case and
                // propagate the error.
                if let Some(ref pool) = self.pool {
                    pool.clear(&self.pool_key);
                }
                return Err(e);
            }
        }

        // -----------------------------------------------------------------
        // Attempt 2 – fresh connection after pool miss / stale hit
        // -----------------------------------------------------------------
        let mut stream = self.connect_new().await?;

        match self.exchange(&mut stream, &envelope).await {
            Ok(result) => {
                self.release_connection(stream);
                Ok(result)
            }
            Err(e) => {
                if let Some(ref pool) = self.pool {
                    pool.clear(&self.pool_key);
                }
                Err(e)
            }
        }
    }

    /// Perform the write-request / read-response exchange on `stream` and
    /// decode the [`FragmentResponse`] into an [`ExecutionResult`].
    async fn exchange(
        &self,
        stream: &mut MaybeEncrypted,
        envelope: &TransportEnvelope,
    ) -> DbResult<ExecutionResult> {
        protocol::write_envelope(stream, envelope).await?;
        let response_envelope = protocol::read_envelope(stream).await?;

        match response_envelope.payload {
            TransportPayload::FragmentResult(FragmentResponse::Success { result, .. }) => {
                Ok(result)
            }
            TransportPayload::FragmentResult(FragmentResponse::Error {
                message,
                sql_state,
                ..
            }) => {
                let detail = if let Some(state) = sql_state {
                    format!("{message} (SQLSTATE {state})")
                } else {
                    message
                };
                Err(DbError::internal(format!(
                    "remote fragment error: {detail}"
                )))
            }
            TransportPayload::FragmentResult(FragmentResponse::CancelAck { .. }) => Err(
                DbError::protocol("unexpected CancelAck in execute response"),
            ),
            other => Err(DbError::protocol(format!(
                "unexpected response payload: {other:?}"
            ))),
        }
    }

    /// Obtain a [`MaybeEncrypted`] stream - from the pool if possible,
    /// otherwise by opening a fresh TCP (+ optional TLS) connection.
    ///
    /// Returns the stream together with a boolean that is `true` when the
    /// connection came from the pool (so the caller knows it might be stale).
    async fn acquire_connection(&self) -> DbResult<(MaybeEncrypted, bool)> {
        if let Some(ref pool) = self.pool {
            if let Some(conn) = pool.take(&self.pool_key) {
                debug!(addr = %self.config.addr, "reusing pooled connection");
                return Ok((conn.stream, true));
            }
        }

        let stream = self.connect_new().await?;
        Ok((stream, false))
    }

    /// Return a connection to the pool for future reuse.
    ///
    /// If no pool is configured the connection is simply dropped.
    fn release_connection(&self, stream: MaybeEncrypted) {
        if let Some(ref pool) = self.pool {
            pool.put(&self.pool_key, PooledConnection { stream });
        }
    }

    /// Open a brand-new TCP connection (with retry) and optionally upgrade
    /// it to TLS.
    async fn connect_new(&self) -> DbResult<MaybeEncrypted> {
        let tcp_stream = self.connect_with_retry().await?;
        self.maybe_upgrade_tls(tcp_stream).await
    }

    /// Attempt to establish a TCP connection with exponential-backoff retry.
    async fn connect_with_retry(&self) -> DbResult<TcpStream> {
        let mut last_err = None;

        // attempt 0 is the initial try, 1..=max_retries are retries
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let backoff =
                    self.config.retry_backoff * 2u32.saturating_pow(attempt.saturating_sub(1));
                debug!(
                    attempt,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    addr = %self.config.addr,
                    "retrying connection"
                );
                tokio::time::sleep(backoff).await;
            }

            match tokio::time::timeout(
                self.config.connect_timeout,
                TcpStream::connect(&self.config.addr),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    debug!(addr = %self.config.addr, "connected");
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    warn!(
                        attempt,
                        addr = %self.config.addr,
                        error = %e,
                        "connection failed"
                    );
                    last_err = Some(format!("connect error: {e}"));
                }
                Err(_elapsed) => {
                    warn!(
                        attempt,
                        addr = %self.config.addr,
                        "connection timed out"
                    );
                    last_err = Some("connection timed out".to_owned());
                }
            }
        }

        Err(DbError::protocol(format!(
            "failed to connect to {} after {} attempts: {}",
            self.config.addr,
            self.config.max_retries + 1,
            last_err.unwrap_or_default(),
        )))
    }

    /// Optionally upgrade a plain TCP stream to TLS.
    ///
    /// If no TLS config is present, the raw stream is returned as-is.
    async fn maybe_upgrade_tls(&self, stream: TcpStream) -> DbResult<MaybeEncrypted> {
        match &self.config.tls {
            Some(tls_config) => {
                let connector = crate::tls::build_tls_connector(tls_config)
                    .map_err(|e| DbError::protocol(format!("TLS setup: {e}")))?;

                // Extract the host portion of addr for SNI.
                let host = self
                    .config
                    .addr
                    .split(':')
                    .next()
                    .unwrap_or(&self.config.addr);
                let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
                    .map_err(|e| DbError::protocol(format!("invalid TLS server name: {e}")))?;

                let tls_stream = tokio::time::timeout(
                    self.config.connect_timeout,
                    connector.connect(server_name, stream),
                )
                .await
                .map_err(|_| DbError::protocol("TLS handshake timed out"))?
                .map_err(|e| DbError::protocol(format!("TLS handshake: {e}")))?;

                Ok(MaybeEncrypted::Tls(Box::new(tls_stream)))
            }
            None => Ok(MaybeEncrypted::Plain(stream)),
        }
    }
}

/// Generate a non-zero cancel-authorization key from a per-process atomic
/// counter mixed with a random offset. Avoids pulling a CSPRNG dep just
/// for this; the key is a defence-in-depth identifier, not a secret.
fn generate_cancel_key() -> u64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = counter
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(nanos.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    if mixed == 0 {
        1
    } else {
        mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let config = FragmentClientConfig::new("127.0.0.1:9100", AuthToken::new("test-token"));
        assert_eq!(config.addr, "127.0.0.1:9100");
        assert_eq!(config.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(config.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(config.retry_backoff, DEFAULT_RETRY_BACKOFF);
        assert!(config.tls.is_none());
    }

    #[test]
    fn config_builder_overrides() {
        let config = FragmentClientConfig::new("10.0.0.1:9200", AuthToken::new("secret"))
            .with_connect_timeout(Duration::from_secs(5))
            .with_max_retries(5)
            .with_retry_backoff(Duration::from_millis(100));

        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.retry_backoff, Duration::from_millis(100));
    }

    #[test]
    fn request_id_increments() {
        let config = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let client = FragmentClient::new(config);

        let id1 = client.allocate_request_id().unwrap();
        let id2 = client.allocate_request_id().unwrap();
        let id3 = client.allocate_request_id().unwrap();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn request_id_overflow_is_rejected_without_wrapping() {
        let config = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let client = FragmentClient::new(config);
        client.next_request_id.store(u64::MAX, Ordering::Relaxed);

        let err = client
            .allocate_request_id()
            .expect_err("request id overflow must fail");
        assert!(err.to_string().contains("request id space exhausted"));
        assert_eq!(client.next_request_id.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn request_id_starts_at_one() {
        let config = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let client = FragmentClient::new(config);

        assert_eq!(client.next_request_id.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn debug_does_not_expose_auth_token() {
        let config =
            FragmentClientConfig::new("127.0.0.1:9100", AuthToken::new("super-secret-123"));
        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret-123"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn fragment_context_clone() {
        let ctx = FragmentContext {
            txn_id: 42,
            isolation: "read committed".to_owned(),
            max_result_rows: 1000,
            max_result_bytes: 1_048_576,
            max_memory_bytes: 10_485_760,
            max_temp_bytes: 5_242_880,
            snapshot: Some(FragmentSnapshot {
                xmin: 1,
                xmax: 5,
                active: vec![3],
            }),
            shard_id: Some(7),
            deadline_epoch_ms: Some(1_700_000_000_000),
        };
        let ctx2 = ctx.clone();
        assert_eq!(ctx.txn_id, ctx2.txn_id);
        assert_eq!(ctx.isolation, ctx2.isolation);
        assert_eq!(ctx.snapshot, ctx2.snapshot);
        assert_eq!(ctx.shard_id, ctx2.shard_id);
        assert_eq!(ctx.deadline_epoch_ms, ctx2.deadline_epoch_ms);
    }

    // -- Connection pool tests ------------------------------------------------

    /// Helper: create a dummy pooled connection without opening OS sockets.
    fn make_dummy_pooled_conn() -> PooledConnection {
        let (client_stream, _peer) = tokio::io::duplex(64);
        PooledConnection {
            stream: MaybeEncrypted::Mock(client_stream),
        }
    }

    #[test]
    fn pool_stores_and_retrieves_connections() {
        let pool = ConnectionPool::new(4);
        let addr = "10.0.0.1:9100";

        assert!(pool.take(addr).is_none());
        assert_eq!(pool.idle_count(addr), 0);

        pool.put(addr, make_dummy_pooled_conn());
        pool.put(addr, make_dummy_pooled_conn());
        assert_eq!(pool.idle_count(addr), 2);

        let conn = pool.take(addr);
        assert!(conn.is_some());
        assert_eq!(pool.idle_count(addr), 1);

        let conn2 = pool.take(addr);
        assert!(conn2.is_some());
        assert_eq!(pool.idle_count(addr), 0);

        assert!(pool.take(addr).is_none());
    }

    #[test]
    fn pool_respects_max_idle_limit() {
        let pool = ConnectionPool::new(2);
        let addr = "10.0.0.2:9200";

        pool.put(addr, make_dummy_pooled_conn());
        pool.put(addr, make_dummy_pooled_conn());
        pool.put(addr, make_dummy_pooled_conn());

        assert_eq!(pool.idle_count(addr), 2);
        assert!(pool.take(addr).is_some());
        assert!(pool.take(addr).is_some());
        assert!(pool.take(addr).is_none());
    }

    #[test]
    fn pool_clear_removes_all_for_address() {
        let pool = ConnectionPool::new(8);
        let addr_a = "10.0.0.3:9300";
        let addr_b = "10.0.0.4:9400";

        pool.put(addr_a, make_dummy_pooled_conn());
        pool.put(addr_a, make_dummy_pooled_conn());
        pool.put(addr_b, make_dummy_pooled_conn());

        assert_eq!(pool.idle_count(addr_a), 2);
        assert_eq!(pool.idle_count(addr_b), 1);

        pool.clear(addr_a);
        assert_eq!(pool.idle_count(addr_a), 0);
        assert_eq!(pool.idle_count(addr_b), 1);
    }

    #[test]
    fn client_with_connection_pool_builder() {
        let config = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let pool = Arc::new(ConnectionPool::with_defaults());

        let client = FragmentClient::new(config).with_connection_pool(Arc::clone(&pool));

        assert!(client.pool.is_some());
        // The pool Arc should be shared.
        assert_eq!(Arc::strong_count(&pool), 2);
    }

    #[test]
    fn client_without_pool_has_none() {
        let config = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let client = FragmentClient::new(config);
        assert!(client.pool.is_none());
    }

    #[test]
    fn pool_with_defaults_uses_default_max() {
        let pool = ConnectionPool::with_defaults();
        assert_eq!(pool.max_idle_per_host, DEFAULT_MAX_IDLE_PER_HOST);
    }

    #[test]
    fn pool_handles_different_addresses_independently() {
        let pool = ConnectionPool::new(4);
        let addr_x = "10.1.0.1:9100";
        let addr_y = "10.1.0.2:9100";

        pool.put(addr_x, make_dummy_pooled_conn());
        pool.put(addr_y, make_dummy_pooled_conn());
        pool.put(addr_y, make_dummy_pooled_conn());

        assert_eq!(pool.idle_count(addr_x), 1);
        assert_eq!(pool.idle_count(addr_y), 2);

        assert!(pool.take(addr_x).is_some());
        assert_eq!(pool.idle_count(addr_x), 0);
        assert_eq!(pool.idle_count(addr_y), 2);
    }

    #[test]
    fn pool_key_distinguishes_plain_and_tls_clients() {
        let plain = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok"));
        let tls = FragmentClientConfig::new("localhost:9100", AuthToken::new("tok")).with_tls(
            TlsClientConfig {
                ca_cert_path: "/tmp/ca.pem".to_owned(),
                client_cert_path: None,
                client_key_path: None,
            },
        );
        assert_ne!(connection_pool_key(&plain), connection_pool_key(&tls));
    }

    #[test]
    fn pooled_retry_classifies_transport_io_only() {
        assert!(is_retryable_pooled_connection_error(&DbError::protocol(
            "read header: connection reset by peer",
        )));
        assert!(is_retryable_pooled_connection_error(&DbError::protocol(
            "write frame: broken pipe",
        )));
        assert!(!is_retryable_pooled_connection_error(&DbError::protocol(
            "unexpected response payload: FragmentResult(Error)",
        )));
        assert!(!is_retryable_pooled_connection_error(&DbError::internal(
            "remote fragment error: duplicate key value",
        )));
    }
}
