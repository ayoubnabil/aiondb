//! Async TCP server for remote fragment execution.
//!
//! Listens for incoming fragment execution requests from other cluster
//! nodes, authenticates them, executes the query plan fragment using
//! the local executor, and returns the result.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aiondb_core::{DbError, DbResult};
use aiondb_executor::ExecutionResult;
use aiondb_plan::PhysicalPlan;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::auth::AuthToken;
use crate::protocol::{
    self, FragmentRequest, FragmentResponse, TransportEnvelope, TransportPayload,
    MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
};
use crate::tls::TlsServerConfig;

type CancelSender = tokio::sync::watch::Sender<bool>;
const CANCEL_MAP_SHARDS: usize = 32;
/// Maximum number of in-flight reaper tasks watching aborted blocking jobs.
/// Each reaper holds a `JoinHandle` for 5s after abort so we can log
/// runaway CPU-bound tasks. Bounding prevents accumulation under a burst
/// of timeouts/cancellations.
const MAX_CONCURRENT_REAPERS: usize = 256;
/// Hard cap on how long `run` will wait for active connections to drain
/// before returning after a shutdown signal.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct CancelRegistry {
    /// Each shard holds a map keyed by `(request_id, cancel_key)`. Two
    /// authenticated peers may legitimately pick the same `request_id`,
    /// so the registry must distinguish them by the cancel-authorization
    /// key the coordinator generated for each fragment. Otherwise a
    /// duplicate insert overwrites the live cancel-watcher of a
    /// concurrent peer, and the eventual `remove` from one peer evicts
    /// the *other* peer's entry — leaving its still-running fragment
    /// uncancelable.
    shards: Vec<Mutex<HashMap<(u64, u64), CancelSender>>>,
}

impl CancelRegistry {
    fn new() -> Self {
        Self {
            shards: (0..CANCEL_MAP_SHARDS)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
        }
    }

    fn shard_index(request_id: u64) -> usize {
        usize::try_from(request_id % u64::try_from(CANCEL_MAP_SHARDS).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn shard(&self, request_id: u64) -> &Mutex<HashMap<(u64, u64), CancelSender>> {
        &self.shards[Self::shard_index(request_id)]
    }

    fn insert(&self, request_id: u64, sender: CancelSender, key: u64) {
        let mut shard = self
            .shard(request_id)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shard.insert((request_id, key), sender);
    }

    fn remove(&self, request_id: u64, key: u64) {
        let mut shard = self
            .shard(request_id)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shard.remove(&(request_id, key));
    }

    /// Return the cancel sender only when the presented `key` matches a
    /// registered `(request_id, key)` entry. Returns `None` when no such
    /// pairing exists — the caller cannot tell whether the request_id is
    /// merely unknown or was registered under a different key.
    fn clone_sender_if_authorized(&self, request_id: u64, key: u64) -> Option<CancelSender> {
        let shard = self
            .shard(request_id)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shard.get(&(request_id, key)).cloned()
    }

    #[cfg(test)]
    fn contains(&self, request_id: u64, key: u64) -> bool {
        let shard = self
            .shard(request_id)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shard.contains_key(&(request_id, key))
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.shards.iter().all(|shard| {
            shard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        })
    }
}

// ---------------------------------------------------------------------------
// Trait: local fragment execution bridge
// ---------------------------------------------------------------------------

/// Trait for executing physical plan fragments on the local node.
/// Implemented by the engine to bridge the transport with query execution.
pub trait FragmentExecutor: Send + Sync {
    /// Execute a physical plan fragment with the given transaction context
    /// and resource limits. Returns the execution result or an error.
    ///
    /// # Errors
    ///
    /// Returns an execution error when planning, resource enforcement, or
    /// fragment execution fails on the local node.
    fn execute_plan(
        &self,
        plan: &PhysicalPlan,
        txn_id: u64,
        isolation: &str,
        snapshot: Option<crate::protocol::FragmentSnapshot>,
        shard_id: Option<u32>,
        max_result_rows: u64,
        max_result_bytes: u64,
        max_memory_bytes: u64,
        max_temp_bytes: u64,
        deadline: Option<Instant>,
    ) -> DbResult<ExecutionResult>;
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the fragment transport server.
pub struct FragmentServerConfig {
    /// Address to bind (e.g. `"0.0.0.0:9100"`).
    pub listen_addr: String,
    /// Shared secret used to authenticate incoming requests.
    pub auth_token: AuthToken,
    /// Optional TLS configuration. When `None`, connections are plaintext.
    pub tls: Option<TlsServerConfig>,
    /// Maximum number of concurrent connections.
    pub max_connections: u32,
    /// Per-request timeout applied to fragment execution.
    pub request_timeout: Duration,
    /// Maximum number of fragment executions running concurrently. When all
    /// permits are in use, new requests block until a slot frees up. Defaults
    /// to `max_connections` when `None`.
    pub max_concurrent_executions: Option<u32>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Async TCP server that accepts fragment execution requests.
pub struct FragmentServer {
    config: FragmentServerConfig,
    executor: Arc<dyn FragmentExecutor>,
    /// Per-request cancel senders keyed by `request_id`.
    cancel_map: Arc<CancelRegistry>,
    /// Limits the number of fragment executions that can run concurrently.
    /// This prevents CPU/memory exhaustion from too many parallel plans.
    execution_semaphore: Arc<Semaphore>,
    /// Bounds the number of background reaper tasks watching aborted
    /// blocking jobs.
    reaper_semaphore: Arc<Semaphore>,
}

impl FragmentServer {
    /// Create a new server with the given configuration and executor.
    pub fn new(config: FragmentServerConfig, executor: Arc<dyn FragmentExecutor>) -> Self {
        let max_exec = config
            .max_concurrent_executions
            .unwrap_or(config.max_connections) as usize;
        Self {
            execution_semaphore: Arc::new(Semaphore::new(max_exec)),
            reaper_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_REAPERS)),
            config,
            executor,
            cancel_map: Arc::new(CancelRegistry::new()),
        }
    }

    /// Run the server until the shutdown signal fires.
    ///
    /// Binds to `listen_addr`, accepts connections, and spawns a task for
    /// each one. The loop exits when `shutdown` transitions to `true`.
    ///
    /// # Errors
    ///
    /// Returns an error if the TCP listener cannot be bound.
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> DbResult<()> {
        let listener = TcpListener::bind(&self.config.listen_addr)
            .await
            .map_err(|e| {
                DbError::internal(format!("failed to bind {}: {e}", self.config.listen_addr))
            })?;

        info!(addr = %self.config.listen_addr, "fragment transport server listening");

        if !self.config.auth_token.is_empty() && self.config.tls.is_none() {
            warn!(
                addr = %self.config.listen_addr,
                "fragment transport: auth token configured but TLS disabled \u{2014} \
                 token will be transmitted in plaintext over the wire"
            );
        }

        // Defence-in-depth: when TLS is enabled, require mTLS (`client_ca_path`)
        // unless the operator explicitly opts into peer-cert-less mode. The
        // shared `auth_token` alone is not enough — any TCP/TLS-reachable
        // client can forge the token if it leaks (audit fragment-transport F2).
        if let Some(tls_cfg) = self.config.tls.as_ref() {
            if tls_cfg.client_ca_path.is_none()
                && std::env::var_os("AIONDB_FRAGMENT_ALLOW_NO_MTLS").is_none()
            {
                return Err(DbError::internal(
                    "fragment transport: TLS configured without client_ca_path. \
                     Enable mTLS or set AIONDB_FRAGMENT_ALLOW_NO_MTLS=1 to opt out.",
                ));
            }
        }

        let tls_acceptor = self
            .config
            .tls
            .as_ref()
            .map(crate::tls::build_tls_acceptor)
            .transpose()
            .map_err(|e| DbError::internal(format!("TLS setup failed: {e}")))?;

        let active_count: Arc<std::sync::atomic::AtomicU32> =
            Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("fragment server shutting down");
                        break;
                    }
                }

                accept_result = listener.accept() => {
                    let (stream, peer) = match accept_result {
                        Ok(pair) => pair,
                        Err(e) => {
                            warn!(error = %e, "accept failed");
                            continue;
                        }
                    };

                    if !try_reserve_connection(&active_count, self.config.max_connections) {
                        warn!(peer = %peer, max = self.config.max_connections, "max connections reached, dropping");
                        drop(stream);
                        continue;
                    }

                    let executor = Arc::clone(&self.executor);
                    let cancel_map = Arc::clone(&self.cancel_map);
                    let auth_token = self.config.auth_token.clone();
                    let request_timeout = self.config.request_timeout;
                    let tls_acceptor = tls_acceptor.clone();
                    let counter = Arc::clone(&active_count);
                    let exec_semaphore = Arc::clone(&self.execution_semaphore);
                    let reaper_semaphore = Arc::clone(&self.reaper_semaphore);

                    tokio::spawn(async move {
                        debug!(peer = %peer, "accepted connection");

                        let result = Self::handle_connection(
                            stream,
                            tls_acceptor,
                            &auth_token,
                            request_timeout,
                            &executor,
                            &cancel_map,
                            &exec_semaphore,
                            &reaper_semaphore,
                        )
                        .await;

                        if let Err(e) = &result {
                            warn!(peer = %peer, error = %e, "connection handler error");
                        }

                        counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                        debug!(peer = %peer, "connection closed");
                    });
                }
            }
        }

        // Graceful drain: wait for in-flight connections to close before
        // returning. Capped by `DRAIN_TIMEOUT` so a stuck handler cannot
        // block shutdown indefinitely.
        let drain_deadline = Instant::now() + DRAIN_TIMEOUT;
        loop {
            let active = active_count.load(std::sync::atomic::Ordering::Acquire);
            if active == 0 {
                break;
            }
            if Instant::now() >= drain_deadline {
                warn!(
                    active,
                    "fragment server drain timeout — exiting with live connections"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Connection handler
    // -----------------------------------------------------------------------

    /// Handle a single inbound connection.
    ///
    /// Optionally upgrades to TLS, then loops over inbound envelopes until
    /// the peer disconnects or a fatal protocol error occurs.
    async fn handle_connection(
        stream: tokio::net::TcpStream,
        tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
        auth_token: &AuthToken,
        request_timeout: Duration,
        executor: &Arc<dyn FragmentExecutor>,
        cancel_map: &Arc<CancelRegistry>,
        exec_semaphore: &Arc<Semaphore>,
        reaper_semaphore: &Arc<Semaphore>,
    ) -> DbResult<()> {
        // ---- optional TLS upgrade (with handshake timeout) ----
        if let Some(acceptor) = tls_acceptor {
            let tls_stream = tokio::time::timeout(request_timeout, acceptor.accept(stream))
                .await
                .map_err(|_| DbError::internal("TLS handshake timed out"))?
                .map_err(|e| DbError::internal(format!("TLS handshake failed: {e}")))?;

            Self::handle_framed(
                tls_stream,
                auth_token,
                request_timeout,
                executor,
                cancel_map,
                exec_semaphore,
                reaper_semaphore,
            )
            .await
        } else {
            Self::handle_framed(
                stream,
                auth_token,
                request_timeout,
                executor,
                cancel_map,
                exec_semaphore,
                reaper_semaphore,
            )
            .await
        }
    }

    /// Process envelopes on an already-established stream.
    ///
    /// Loops on `read_envelope` so a single connection can serve many
    /// requests back-to-back. Returns `Ok(())` on a clean peer disconnect
    /// (read EOF) and `Err` only on fatal protocol/auth errors that we have
    /// reported back to the peer.
    async fn handle_framed<S>(
        mut stream: S,
        auth_token: &AuthToken,
        request_timeout: Duration,
        executor: &Arc<dyn FragmentExecutor>,
        cancel_map: &Arc<CancelRegistry>,
        exec_semaphore: &Arc<Semaphore>,
        reaper_semaphore: &Arc<Semaphore>,
    ) -> DbResult<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let envelope = match protocol::read_envelope(&mut stream).await {
                Ok(env) => env,
                Err(e) => {
                    debug!(error = %e, "framed: read failed, closing connection");
                    return Ok(());
                }
            };

            // ---- version negotiation ----
            // Accept any version in the inclusive range
            // [MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION]. With both bounds
            // currently equal to 1 this is a strict equality check, but the
            // shape lets a future bump of `PROTOCOL_VERSION` extend the
            // accepted window without changing the call sites.
            if envelope.version < MIN_PROTOCOL_VERSION || envelope.version > PROTOCOL_VERSION {
                let resp = TransportEnvelope {
                    version: PROTOCOL_VERSION,
                    auth_token: String::new(),
                    payload: TransportPayload::FragmentResult(FragmentResponse::Error {
                        request_id: 0,
                        message: format!(
                            "protocol version unsupported: server accepts \
                             [{MIN_PROTOCOL_VERSION}..={PROTOCOL_VERSION}], got {}",
                            envelope.version,
                        ),
                        sql_state: Some(String::from("08006")),
                    }),
                };
                let _ = protocol::write_envelope(&mut stream, &resp).await;
                return Err(DbError::protocol("protocol version unsupported"));
            }

            // ---- auth ----
            // Route through `validate_request_auth` so an empty server token
            // or empty client token is rejected explicitly. The bare
            // `AuthToken::validate` is a constant-time byte compare and would
            // otherwise treat `("", "")` as a valid match — a silent auth
            // bypass if the operator misconfigured an empty shared secret.
            if let Err(auth_err) =
                crate::auth::validate_request_auth(auth_token, &envelope.auth_token)
            {
                let resp = TransportEnvelope {
                    version: PROTOCOL_VERSION,
                    auth_token: String::new(),
                    payload: TransportPayload::FragmentResult(FragmentResponse::Error {
                        request_id: 0,
                        message: String::from("authentication failed"),
                        sql_state: Some(String::from("28000")),
                    }),
                };
                let _ = protocol::write_envelope(&mut stream, &resp).await;
                return Err(auth_err);
            }

            // ---- dispatch payload ----
            let dispatch_result: DbResult<()> = match envelope.payload {
                TransportPayload::ExecuteFragment(req) => {
                    // Validate any wire-supplied MVCC snapshot at the trust
                    // boundary so the local engine never sees a snapshot
                    // with `xmin > xmax`, an `active` xid outside
                    // `[xmin, xmax)`, or an unbounded `active` set. An
                    // authenticated-but-compromised peer would otherwise be
                    // able to drive arbitrary visibility on this node.
                    if let Some(snapshot) = req.snapshot.as_ref() {
                        if let Err(error) = snapshot.validate() {
                            let resp = TransportEnvelope {
                                version: PROTOCOL_VERSION,
                                auth_token: String::new(),
                                payload: TransportPayload::FragmentResult(
                                    FragmentResponse::Error {
                                        request_id: req.request_id,
                                        message: error.to_string(),
                                        sql_state: Some(String::from("08P01")),
                                    },
                                ),
                            };
                            let _ = protocol::write_envelope(&mut stream, &resp).await;
                            return Err(error);
                        }
                    }

                    // Acquire an execution permit before spawning the blocking
                    // task. This caps the number of plans running concurrently
                    // and protects the node from CPU/memory exhaustion.
                    let _exec_permit = exec_semaphore
                        .acquire()
                        .await
                        .map_err(|_| DbError::internal("execution semaphore closed"))?;

                    Self::handle_execute(
                        &mut stream,
                        *req,
                        request_timeout,
                        executor,
                        cancel_map,
                        reaper_semaphore,
                    )
                    .await
                }
                TransportPayload::CancelFragment(cancel) => {
                    Self::handle_cancel(
                        &mut stream,
                        cancel.request_id,
                        cancel.cancel_key,
                        cancel_map,
                    )
                    .await
                }
                TransportPayload::FragmentResult(_) => {
                    let resp = TransportEnvelope {
                        version: PROTOCOL_VERSION,
                        auth_token: String::new(),
                        payload: TransportPayload::FragmentResult(FragmentResponse::Error {
                            request_id: 0,
                            message: String::from("unexpected FragmentResult from client"),
                            sql_state: Some(String::from("08P01")),
                        }),
                    };
                    let _ = protocol::write_envelope(&mut stream, &resp).await;
                    Err(DbError::protocol("unexpected FragmentResult payload"))
                }
            };

            // Any dispatch error indicates the stream is no longer trustworthy
            // (write failure, semaphore closed, unexpected payload). Close the
            // connection rather than risk a desynchronised follow-up envelope.
            if let Err(e) = dispatch_result {
                debug!(error = %e, "framed: dispatch failed, closing connection");
                return Err(e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Execute handler
    // -----------------------------------------------------------------------

    // NOTE: Per-request memory limits are enforced by the executor itself
    // via `max_memory_bytes` in the execution context, not by the transport
    // layer.  The transport only enforces timeout, cancellation, and
    // concurrency limits.

    async fn handle_execute<S>(
        stream: &mut S,
        req: FragmentRequest,
        request_timeout: Duration,
        executor: &Arc<dyn FragmentExecutor>,
        cancel_map: &Arc<CancelRegistry>,
        reaper_semaphore: &Arc<Semaphore>,
    ) -> DbResult<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let request_id = req.request_id;
        let cancel_key = req.cancel_key;

        // Register a cancel watch for this request.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        cancel_map.insert(request_id, cancel_tx, cancel_key);

        // Convert epoch-ms deadline to Instant.
        let deadline = deadline_from_epoch_ms(req.deadline_epoch_ms);

        // Execute the plan in a blocking task so we do not starve the
        // async runtime.
        let exec = Arc::clone(executor);
        let plan = req.plan;
        let txn_id = req.txn_id;
        let isolation = req.isolation;
        let snapshot = req.snapshot;
        let shard_id = req.shard_id;
        let max_rows = req.max_result_rows;
        let max_bytes = req.max_result_bytes;
        let max_memory = req.max_memory_bytes;
        let max_temp = req.max_temp_bytes;

        let mut handle = tokio::task::spawn_blocking(move || {
            exec.execute_plan(
                &plan, txn_id, &isolation, snapshot, shard_id, max_rows, max_bytes, max_memory,
                max_temp, deadline,
            )
        });

        // Wait for the result, honouring timeout and cancellation.
        //
        // The `&mut handle` form keeps ownership so we can abort the
        // blocking task when cancel or timeout wins the race.
        let response = tokio::select! {
            biased;

            () = wait_for_cancel(cancel_rx) => {
                handle.abort();
                info!(request_id, event = "cancel", "fragment execution canceled");
                debug!(request_id, "aborted blocking task after cancel");
                FragmentResponse::Error {
                    request_id,
                    message: String::from("query canceled"),
                    sql_state: Some(String::from("57014")),
                }
            }

            () = tokio::time::sleep(request_timeout) => {
                handle.abort();
                info!(request_id, event = "timeout", "fragment execution timed out");
                debug!(request_id, "aborted blocking task after timeout");
                FragmentResponse::Error {
                    request_id,
                    message: String::from("fragment execution timed out"),
                    sql_state: Some(String::from("57014")),
                }
            }

            join_result = &mut handle => {
                match join_result {
                    Ok(Ok(result)) => FragmentResponse::Success { request_id, result },
                    Ok(Err(db_err)) => {
                        let report = db_err.report();
                        FragmentResponse::Error {
                            request_id,
                            message: report.message.clone(),
                            sql_state: Some(report.sqlstate.code().to_owned()),
                        }
                    }
                    Err(join_err) => FragmentResponse::Error {
                        request_id,
                        message: format!("executor task panicked: {join_err}"),
                        sql_state: Some(String::from("XX000")),
                    },
                }
            }
        };

        // If we canceled/timed-out but the blocking task is still running,
        // spawn a bounded background reaper to log runaway CPU-bound work.
        // `abort()` on a `spawn_blocking` task only takes effect when the
        // blocking thread yields; pure CPU work may outlive the signal.
        // The reaper holds the `JoinHandle` for a 5 s grace period so we
        // can warn if the task never completes. When the reaper-pool cap
        // (`MAX_CONCURRENT_REAPERS`) is exhausted we drop the handle
        // immediately rather than risk an unbounded fan-out under burst.
        if !handle.is_finished() {
            if let Ok(permit) = Arc::clone(reaper_semaphore).try_acquire_owned() {
                let reaper_request_id = request_id;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if !handle.is_finished() {
                        warn!(
                            request_id = reaper_request_id,
                            "fragment task still running 5s after abort \u{2014} dropping handle"
                        );
                    }
                    drop(handle);
                    drop(permit);
                });
            } else {
                warn!(
                    request_id,
                    "reaper pool saturated \u{2014} dropping handle without grace period"
                );
                drop(handle);
            }
        }

        // Remove the cancel entry. Scope the remove by `(request_id,
        // cancel_key)` so a concurrent peer that picked the same
        // `request_id` does not lose its still-active cancel-watcher
        // when this fragment finishes.
        cancel_map.remove(request_id, cancel_key);

        let resp_envelope = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: String::new(),
            payload: TransportPayload::FragmentResult(response),
        };

        protocol::write_envelope(stream, &resp_envelope)
            .await
            .map_err(|e| DbError::protocol(format!("failed to write response: {e}")))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cancel handler
    // -----------------------------------------------------------------------

    async fn handle_cancel<S>(
        stream: &mut S,
        request_id: u64,
        cancel_key: u64,
        cancel_map: &Arc<CancelRegistry>,
    ) -> DbResult<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        if let Some(tx) = cancel_map.clone_sender_if_authorized(request_id, cancel_key) {
            let _ = tx.send(true);
            debug!(request_id, "cancel signal sent");
        } else {
            debug!(
                request_id,
                "cancel request for unknown/completed fragment or key mismatch"
            );
        }

        let resp = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: String::new(),
            payload: TransportPayload::FragmentResult(FragmentResponse::CancelAck { request_id }),
        };

        protocol::write_envelope(stream, &resp)
            .await
            .map_err(|e| DbError::protocol(format!("failed to write cancel ack: {e}")))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an optional epoch-millisecond deadline to an `Instant`.
///
/// If the deadline has already passed, returns `Instant::now()`. If the
/// supplied epoch is too far in the future to represent as an `Instant`,
/// returns `None`; the transport-level request timeout still bounds execution.
fn deadline_from_epoch_ms(epoch_ms: Option<u64>) -> Option<Instant> {
    let epoch_ms = epoch_ms?;
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let target = Duration::from_millis(epoch_ms);
    if let Some(remaining) = target.checked_sub(now_epoch) {
        Instant::now().checked_add(remaining)
    } else {
        Some(Instant::now())
    }
}

fn try_reserve_connection(
    active_count: &std::sync::atomic::AtomicU32,
    max_connections: u32,
) -> bool {
    active_count
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |current| {
                if current >= max_connections {
                    None
                } else {
                    current.checked_add(1)
                }
            },
        )
        .is_ok()
}

/// Wait until the cancel watch fires or the sender is dropped.
async fn wait_for_cancel(mut rx: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            // Sender dropped -- request finished normally; park forever so
            // the select branch never fires.
            std::future::pending::<()>().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_construction() {
        let config = FragmentServerConfig {
            listen_addr: String::from("127.0.0.1:9100"),
            auth_token: AuthToken::new("secret-token"),
            tls: None,
            max_connections: 64,
            request_timeout: Duration::from_secs(30),
            max_concurrent_executions: None,
        };

        assert_eq!(config.listen_addr, "127.0.0.1:9100");
        assert!(config.tls.is_none());
        assert_eq!(config.max_connections, 64);
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert!(config.max_concurrent_executions.is_none());
    }

    #[test]
    fn deadline_future_epoch_produces_later_instant() {
        let future_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let future_ms = u64::try_from(future_ms)
            .unwrap_or(u64::MAX)
            .saturating_add(10_000);
        let instant = deadline_from_epoch_ms(Some(future_ms));
        assert!(instant.is_some());
        assert!(instant.unwrap() > Instant::now());
    }

    #[test]
    fn deadline_past_epoch_produces_now_instant() {
        let past_ms = 1_000u64; // well in the past
        let instant = deadline_from_epoch_ms(Some(past_ms));
        assert!(instant.is_some());
        // The returned instant should be approximately now (or just before).
        let diff = Instant::now().duration_since(instant.unwrap());
        assert!(diff < Duration::from_secs(1));
    }

    #[test]
    fn deadline_none_produces_none() {
        assert!(deadline_from_epoch_ms(None).is_none());
    }

    #[test]
    fn deadline_too_far_future_produces_none() {
        assert!(deadline_from_epoch_ms(Some(u64::MAX)).is_none());
    }

    #[test]
    fn try_reserve_connection_enforces_limit_without_overshoot() {
        let active = std::sync::atomic::AtomicU32::new(1);

        assert!(!try_reserve_connection(&active, 1));
        assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn try_reserve_connection_rejects_counter_overflow() {
        let active = std::sync::atomic::AtomicU32::new(u32::MAX);

        assert!(!try_reserve_connection(&active, u32::MAX));
        assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), u32::MAX);
    }

    #[test]
    fn cancel_map_insert_and_remove() {
        let map = CancelRegistry::new();

        let key = 0xdead_beef;
        let (tx, rx) = tokio::sync::watch::channel(false);
        map.insert(42, tx, key);

        assert!(map.contains(42, key));
        assert!(!*rx.borrow());

        assert!(map.clone_sender_if_authorized(42, 0).is_none());
        // Right key: returns sender; signal cancel.
        let _ = map.clone_sender_if_authorized(42, key).unwrap().send(true);
        assert!(*rx.borrow());

        map.remove(42, key);
        assert!(!map.contains(42, key));
    }

    /// Regression: two authenticated peers may legitimately pick the same
    /// `request_id` for their independent fragments. Pre-fix the cancel
    /// registry was keyed by the bare `request_id`, so the second
    /// `insert` overwrote the first peer's `cancel_tx`, and either
    /// peer's eventual `remove(request_id)` evicted the *other* peer's
    /// entry — leaving the still-running fragment uncancelable.
    ///
    /// After the fix the registry is keyed by `(request_id, cancel_key)`
    /// so concurrent peers' entries coexist and lookups/removals stay
    /// scoped to the matching cancel-authorization key.
    #[test]
    fn cancel_map_request_id_collision_keeps_concurrent_peer_entry_intact() {
        let map = CancelRegistry::new();
        let (tx_a, _rx_a) = tokio::sync::watch::channel(false);
        let (tx_b, _rx_b) = tokio::sync::watch::channel(false);
        let key_a: u64 = 0xAAAA_AAAA_AAAA_AAAA;
        let key_b: u64 = 0xBBBB_BBBB_BBBB_BBBB;

        // Peer A registers a long-running fragment with request_id=42.
        map.insert(42, tx_a, key_a);
        // Peer B (different authenticated peer) reuses the same
        // request_id but with its own cancel_key.
        map.insert(42, tx_b, key_b);

        // Both peers can still address their own fragments.
        assert!(map.clone_sender_if_authorized(42, key_a).is_some());
        assert!(map.clone_sender_if_authorized(42, key_b).is_some());

        // Peer A's fragment finishes and removes its own entry; peer B's
        // entry must remain so peer B can still cancel its own fragment.
        map.remove(42, key_a);
        assert!(!map.contains(42, key_a));
        assert!(map.contains(42, key_b));
        assert!(map.clone_sender_if_authorized(42, key_b).is_some());
    }

    /// POC: a peer who guesses `request_id` but does not know the
    #[test]
    fn cancel_map_rejects_wrong_key() {
        let map = CancelRegistry::new();
        let (tx, rx) = tokio::sync::watch::channel(false);
        map.insert(7, tx, 0x1234);

        assert!(map.clone_sender_if_authorized(7, 0x9999).is_none());
        assert!(!*rx.borrow(), "wrong-key cancel must not signal");

        // Right key - accepted.
        let _ = map
            .clone_sender_if_authorized(7, 0x1234)
            .unwrap()
            .send(true);
        assert!(*rx.borrow(), "right-key cancel must signal");
    }

    #[tokio::test]
    async fn handle_framed_serves_multiple_envelopes_back_to_back() {
        // Regression: handle_framed used to read a single envelope and then
        // return, which made the client-side connection pool useless because
        // the server would close the socket after every request. The loop
        // version must serve back-to-back envelopes on the same stream and
        // exit cleanly when the peer drops the write half.
        struct NoopExecutor;

        impl FragmentExecutor for NoopExecutor {
            fn execute_plan(
                &self,
                _plan: &PhysicalPlan,
                _txn_id: u64,
                _isolation: &str,
                _snapshot: Option<crate::protocol::FragmentSnapshot>,
                _shard_id: Option<u32>,
                _max_result_rows: u64,
                _max_result_bytes: u64,
                _max_memory_bytes: u64,
                _max_temp_bytes: u64,
                _deadline: Option<Instant>,
            ) -> DbResult<ExecutionResult> {
                Ok(ExecutionResult::command("SELECT"))
            }
        }

        use crate::protocol::{CancelRequest, FragmentResponse};

        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let auth = AuthToken::new("shared-secret");
        let executor: Arc<dyn FragmentExecutor> = Arc::new(NoopExecutor);
        let cancel_map = Arc::new(CancelRegistry::new());
        let exec_semaphore = Arc::new(Semaphore::new(4));

        let reaper_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REAPERS));
        let auth_clone = auth.clone();
        let executor_clone = Arc::clone(&executor);
        let cancel_map_clone = Arc::clone(&cancel_map);
        let exec_semaphore_clone = Arc::clone(&exec_semaphore);
        let reaper_semaphore_clone = Arc::clone(&reaper_semaphore);
        let server_handle = tokio::spawn(async move {
            FragmentServer::handle_framed(
                server,
                &auth_clone,
                Duration::from_secs(5),
                &executor_clone,
                &cancel_map_clone,
                &exec_semaphore_clone,
                &reaper_semaphore_clone,
            )
            .await
        });

        // Send three cancel envelopes back-to-back on the same stream.
        for request_id in [11u64, 22, 33] {
            let req = TransportEnvelope {
                version: PROTOCOL_VERSION,
                auth_token: auth.as_str().to_owned(),
                payload: TransportPayload::CancelFragment(CancelRequest {
                    request_id,
                    cancel_key: 0,
                }),
            };
            protocol::write_envelope(&mut client, &req).await.unwrap();

            let resp = protocol::read_envelope(&mut client).await.unwrap();
            match resp.payload {
                TransportPayload::FragmentResult(FragmentResponse::CancelAck {
                    request_id: ack_id,
                }) => assert_eq!(ack_id, request_id, "ack must match request"),
                other => panic!("unexpected response: {other:?}"),
            }
        }

        // Closing the client-half should cause the server-side read to fail
        // with EOF; handle_framed must then return Ok cleanly.
        drop(client);
        let result = tokio::time::timeout(Duration::from_secs(1), server_handle)
            .await
            .expect("handle_framed must terminate after peer drop")
            .expect("task must not panic");
        assert!(
            result.is_ok(),
            "handle_framed should return Ok on clean EOF, got {result:?}"
        );
    }

    #[tokio::test]
    async fn handle_framed_passes_shard_id_to_executor() {
        struct CapturingExecutor {
            seen_shard_id: Arc<Mutex<Option<u32>>>,
        }

        impl FragmentExecutor for CapturingExecutor {
            fn execute_plan(
                &self,
                _plan: &PhysicalPlan,
                _txn_id: u64,
                _isolation: &str,
                _snapshot: Option<crate::protocol::FragmentSnapshot>,
                shard_id: Option<u32>,
                _max_result_rows: u64,
                _max_result_bytes: u64,
                _max_memory_bytes: u64,
                _max_temp_bytes: u64,
                _deadline: Option<Instant>,
            ) -> DbResult<ExecutionResult> {
                *self
                    .seen_shard_id
                    .lock()
                    .expect("captured shard lock should not be poisoned") = shard_id;
                Ok(ExecutionResult::command("SELECT"))
            }
        }

        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let auth = AuthToken::new("shared-secret");
        let seen_shard_id = Arc::new(Mutex::new(None));
        let executor: Arc<dyn FragmentExecutor> = Arc::new(CapturingExecutor {
            seen_shard_id: Arc::clone(&seen_shard_id),
        });
        let cancel_map = Arc::new(CancelRegistry::new());
        let exec_semaphore = Arc::new(Semaphore::new(4));
        let reaper_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REAPERS));

        let auth_clone = auth.clone();
        let executor_clone = Arc::clone(&executor);
        let cancel_map_clone = Arc::clone(&cancel_map);
        let exec_semaphore_clone = Arc::clone(&exec_semaphore);
        let reaper_semaphore_clone = Arc::clone(&reaper_semaphore);
        let server_handle = tokio::spawn(async move {
            FragmentServer::handle_framed(
                server,
                &auth_clone,
                Duration::from_secs(5),
                &executor_clone,
                &cancel_map_clone,
                &exec_semaphore_clone,
                &reaper_semaphore_clone,
            )
            .await
        });

        let req = FragmentRequest {
            request_id: 99,
            plan: PhysicalPlan::InternalNoOp {
                tag: "SELECT".to_owned(),
                notice: None,
            },
            txn_id: 7,
            isolation: "ReadCommitted".to_owned(),
            max_result_rows: 10_000,
            max_result_bytes: 8 * 1024 * 1024,
            max_memory_bytes: 64 * 1024 * 1024,
            max_temp_bytes: 256 * 1024 * 1024,
            snapshot: None,
            deadline_epoch_ms: None,
            shard_id: Some(42),
            cancel_key: 0,
        };
        let envelope = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: auth.as_str().to_owned(),
            payload: TransportPayload::ExecuteFragment(Box::new(req)),
        };

        protocol::write_envelope(&mut client, &envelope)
            .await
            .unwrap();
        let response = protocol::read_envelope(&mut client).await.unwrap();
        assert!(matches!(
            response.payload,
            TransportPayload::FragmentResult(FragmentResponse::Success { request_id: 99, .. })
        ));
        assert_eq!(
            *seen_shard_id
                .lock()
                .expect("captured shard lock should not be poisoned"),
            Some(42)
        );

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), server_handle)
            .await
            .expect("handle_framed must terminate after peer drop")
            .expect("task must not panic")
            .expect("handle_framed should return Ok on clean EOF");
    }

    #[test]
    fn server_construction() {
        struct NoopExecutor;

        impl FragmentExecutor for NoopExecutor {
            fn execute_plan(
                &self,
                _plan: &PhysicalPlan,
                _txn_id: u64,
                _isolation: &str,
                _snapshot: Option<crate::protocol::FragmentSnapshot>,
                _shard_id: Option<u32>,
                _max_result_rows: u64,
                _max_result_bytes: u64,
                _max_memory_bytes: u64,
                _max_temp_bytes: u64,
                _deadline: Option<Instant>,
            ) -> DbResult<ExecutionResult> {
                Ok(ExecutionResult::command("SELECT"))
            }
        }

        let config = FragmentServerConfig {
            listen_addr: String::from("127.0.0.1:0"),
            auth_token: AuthToken::new("tok"),
            tls: None,
            max_connections: 8,
            request_timeout: Duration::from_secs(5),
            max_concurrent_executions: Some(4),
        };

        let server = FragmentServer::new(config, Arc::new(NoopExecutor));
        assert!(server.cancel_map.is_empty());
        // Semaphore should respect the explicit limit (4), not max_connections (8).
        assert_eq!(server.execution_semaphore.available_permits(), 4);
        assert_eq!(
            server.reaper_semaphore.available_permits(),
            MAX_CONCURRENT_REAPERS
        );
    }

    /// Regression: an unconfigured (empty) shared secret on the server must
    /// not silently authenticate an unauthenticated peer that also sends an
    /// empty token. The bare `AuthToken::validate` is a constant-time byte
    /// compare and treats `("", "")` as a match, so we route through
    /// `validate_request_auth` to reject empty tokens on either side.
    #[tokio::test]
    async fn handle_framed_rejects_empty_server_and_empty_client_token() {
        struct NoopExecutor;
        impl FragmentExecutor for NoopExecutor {
            fn execute_plan(
                &self,
                _plan: &PhysicalPlan,
                _txn_id: u64,
                _isolation: &str,
                _snapshot: Option<crate::protocol::FragmentSnapshot>,
                _shard_id: Option<u32>,
                _max_result_rows: u64,
                _max_result_bytes: u64,
                _max_memory_bytes: u64,
                _max_temp_bytes: u64,
                _deadline: Option<Instant>,
            ) -> DbResult<ExecutionResult> {
                Ok(ExecutionResult::command("SELECT"))
            }
        }

        use crate::protocol::{CancelRequest, FragmentResponse};

        let (mut client, server) = tokio::io::duplex(8 * 1024);
        let auth = AuthToken::new(""); // empty server secret (misconfig)
        let executor: Arc<dyn FragmentExecutor> = Arc::new(NoopExecutor);
        let cancel_map = Arc::new(CancelRegistry::new());
        let exec_semaphore = Arc::new(Semaphore::new(4));
        let reaper_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REAPERS));

        let auth_clone = auth.clone();
        let executor_clone = Arc::clone(&executor);
        let cancel_map_clone = Arc::clone(&cancel_map);
        let exec_semaphore_clone = Arc::clone(&exec_semaphore);
        let reaper_semaphore_clone = Arc::clone(&reaper_semaphore);
        let server_handle = tokio::spawn(async move {
            FragmentServer::handle_framed(
                server,
                &auth_clone,
                Duration::from_secs(5),
                &executor_clone,
                &cancel_map_clone,
                &exec_semaphore_clone,
                &reaper_semaphore_clone,
            )
            .await
        });

        // Unauthenticated peer sends an empty auth_token alongside a
        // legitimate-looking cancel request. With the pre-fix bare
        // `AuthToken::validate("", "")` this would have passed.
        let req = TransportEnvelope {
            version: PROTOCOL_VERSION,
            auth_token: String::new(),
            payload: TransportPayload::CancelFragment(CancelRequest {
                request_id: 1,
                cancel_key: 0,
            }),
        };
        protocol::write_envelope(&mut client, &req).await.unwrap();

        // The server must answer with `28000` and close the stream.
        let resp = protocol::read_envelope(&mut client).await.unwrap();
        match resp.payload {
            TransportPayload::FragmentResult(FragmentResponse::Error {
                request_id: 0,
                ref sql_state,
                ref message,
            }) => {
                assert_eq!(sql_state.as_deref(), Some("28000"));
                assert!(message.contains("authentication failed"));
            }
            other => panic!("expected auth error envelope, got {other:?}"),
        }

        let join = tokio::time::timeout(Duration::from_secs(1), server_handle)
            .await
            .expect("server task must finish");
        let result = join.expect("task must not panic");
        assert!(
            result.is_err(),
            "server must return Err on auth failure, got {result:?}"
        );
    }
}
