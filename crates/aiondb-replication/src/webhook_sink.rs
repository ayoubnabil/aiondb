//! HTTP webhook sink for [`crate::changefeed::ChangefeedBus`].
//!
//! Pulls `ChangefeedEvent`s from a bus subscription, batches them, and
//! POSTs each batch to a configured HTTP endpoint encoded as a JSON
//! array. Designed for the common "send CDC to a webhook / message
//! broker" use case without dragging in a hyper / reqwest dependency.
//!
//! # Wire format
//!
//! ```text
//! POST /<path> HTTP/1.1
//! Host: <host>
//! Content-Type: application/json
//! Content-Length: <N>
//!
//! [
//!   {"kind":"insert","commit_ts":42,"table":1,"tuple_id":...,"row":...},
//!   {"kind":"resolved","commit_ts":50}
//! ]
//! ```
//!
//! The receiver replies with any 2xx; anything else (or a TCP error)
//! triggers exponential-backoff retry until `retry_max` attempts are
//! exhausted, at which point the batch is dropped and a warn-level
//! `tracing` event is emitted. Higher layers can subscribe to
//! [`SinkMetrics`] for delivery / failure counters.
//!
//! # Delivery semantics
//!
//! - **At-least-once.** Retries can produce duplicates. Downstream
//!   consumers MUST deduplicate via `commit_ts` + `tuple_id` if they
//!   require exactly-once semantics.
//! - **In-order within a single sink.** Batches are POSTed strictly in
//!   the order events came off the bus.
//! - **Backpressure-aware.** Batches accumulate up to `batch_size` or
//!   `max_batch_delay` whichever comes first. A slow receiver does
//!   not stall the bus -- if the worker falls behind by more than the
//!   broadcast channel capacity, it is dropped and a fresh
//!   subscription must be obtained.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, trace, warn};

use crate::changefeed::{ChangefeedBus, ChangefeedEvent, ChangefeedFilter};

/// Default maximum events per HTTP POST.
pub const DEFAULT_BATCH_SIZE: usize = 256;

/// Default maximum delay before flushing a partial batch.
pub const DEFAULT_MAX_BATCH_DELAY: Duration = Duration::from_millis(250);

/// Default per-batch retry budget.
pub const DEFAULT_RETRY_MAX: usize = 5;

/// Default initial retry delay; doubles per attempt up to 30s.
pub const DEFAULT_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(200);

const MAX_WEBHOOK_ADDR_BYTES: usize = 512;
const MAX_WEBHOOK_HOST_HEADER_BYTES: usize = 512;
const MAX_WEBHOOK_PATH_BYTES: usize = 2048;
const MAX_WEBHOOK_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Configuration for one [`WebhookSink`] worker.
#[derive(Clone, Debug)]
pub struct WebhookSinkConfig {
    /// Target host and port, e.g. `"example.com:443"` or `"127.0.0.1:8080"`.
    pub addr: String,
    /// HTTP path including leading slash, e.g. `"/cdc"`.
    pub path: String,
    /// `Host` header value. Defaults to `addr` when empty.
    pub host_header: String,
    pub batch_size: usize,
    pub max_batch_delay: Duration,
    pub retry_max: usize,
    pub retry_initial_delay: Duration,
    /// Hard timeout for one HTTP attempt.
    pub request_timeout: Duration,
}

impl WebhookSinkConfig {
    pub fn new(addr: impl Into<String>, path: impl Into<String>) -> Self {
        let addr = addr.into();
        let host_header = addr.clone();
        Self {
            addr,
            path: path.into(),
            host_header,
            batch_size: DEFAULT_BATCH_SIZE,
            max_batch_delay: DEFAULT_MAX_BATCH_DELAY,
            retry_max: DEFAULT_RETRY_MAX,
            retry_initial_delay: DEFAULT_RETRY_INITIAL_DELAY,
            request_timeout: Duration::from_secs(5),
        }
    }
}

/// Atomically-updated sink delivery counters.
#[derive(Debug, Default)]
pub struct SinkMetrics {
    pub batches_attempted: AtomicU64,
    pub batches_delivered: AtomicU64,
    pub batches_dropped: AtomicU64,
    pub events_delivered: AtomicU64,
    pub retry_attempts: AtomicU64,
    pub bytes_sent: AtomicU64,
}

impl SinkMetrics {
    pub fn snapshot(&self) -> SinkMetricsSnapshot {
        SinkMetricsSnapshot {
            batches_attempted: self.batches_attempted.load(Ordering::Relaxed),
            batches_delivered: self.batches_delivered.load(Ordering::Relaxed),
            batches_dropped: self.batches_dropped.load(Ordering::Relaxed),
            events_delivered: self.events_delivered.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SinkMetricsSnapshot {
    pub batches_attempted: u64,
    pub batches_delivered: u64,
    pub batches_dropped: u64,
    pub events_delivered: u64,
    pub retry_attempts: u64,
    pub bytes_sent: u64,
}

/// Running webhook sink. Drop the handle to stop the worker.
pub struct WebhookSink {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    metrics: Arc<SinkMetrics>,
}

impl WebhookSink {
    /// Spawn a worker that subscribes to `bus` and forwards events to
    /// the HTTP endpoint described by `config`.
    pub fn spawn(bus: &ChangefeedBus, filter: ChangefeedFilter, config: WebhookSinkConfig) -> Self {
        let mut rx = bus.subscribe(filter);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let metrics = Arc::new(SinkMetrics::default());
        let metrics_clone = Arc::clone(&metrics);
        let handle = tokio::spawn(async move {
            run_sink(&mut rx, config, shutdown_rx, metrics_clone).await;
        });
        Self {
            handle,
            shutdown_tx,
            metrics,
        }
    }

    pub fn metrics(&self) -> Arc<SinkMetrics> {
        Arc::clone(&self.metrics)
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.handle.await;
    }
}

async fn run_sink(
    rx: &mut crate::changefeed::ChangefeedSubscription,
    config: WebhookSinkConfig,
    mut shutdown_rx: watch::Receiver<bool>,
    metrics: Arc<SinkMetrics>,
) {
    let mut batch: Vec<ChangefeedEvent> = Vec::with_capacity(config.batch_size.max(1));
    let mut deadline: Option<Instant> = None;

    loop {
        let sleep_for = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(60),
        };
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    if !batch.is_empty() {
                        let _ = deliver(&batch, &config, &metrics).await;
                    }
                    debug!("webhook sink shutdown");
                    return;
                }
            }
            recv = rx.recv() => match recv {
                Ok(ev) => {
                    batch.push(ev);
                    if deadline.is_none() {
                        deadline = Some(Instant::now() + config.max_batch_delay);
                    }
                    if batch.len() >= config.batch_size {
                        if let Err(err) = deliver(&batch, &config, &metrics).await {
                            warn!(error = %err, "webhook sink batch dropped after retries");
                            metrics.batches_dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        batch.clear();
                        deadline = None;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    warn!(skipped = skipped, "webhook sink lagged behind bus");
                }
                Err(RecvError::Closed) => {
                    if !batch.is_empty() {
                        let _ = deliver(&batch, &config, &metrics).await;
                    }
                    debug!("changefeed bus closed; webhook sink exits");
                    return;
                }
            },
            () = time::sleep(sleep_for), if deadline.is_some() => {
                if !batch.is_empty() {
                    if let Err(err) = deliver(&batch, &config, &metrics).await {
                        warn!(error = %err, "webhook sink batch dropped after retries");
                        metrics.batches_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    batch.clear();
                }
                deadline = None;
            }
        }
    }
}

async fn deliver(
    batch: &[ChangefeedEvent],
    config: &WebhookSinkConfig,
    metrics: &Arc<SinkMetrics>,
) -> io::Result<()> {
    let body = serde_json::to_vec(batch).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("webhook batch encode: {e}"),
        )
    })?;
    if body.len() > MAX_WEBHOOK_BODY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("webhook body exceeds {MAX_WEBHOOK_BODY_BYTES} bytes"),
        ));
    }
    let request = build_request(config, &body)?;
    let mut delay = config.retry_initial_delay;
    for attempt in 0..=config.retry_max {
        metrics.batches_attempted.fetch_add(1, Ordering::Relaxed);
        if attempt > 0 {
            metrics.retry_attempts.fetch_add(1, Ordering::Relaxed);
        }
        match time::timeout(config.request_timeout, send_one(&config.addr, &request)).await {
            Ok(Ok(())) => {
                metrics.batches_delivered.fetch_add(1, Ordering::Relaxed);
                metrics
                    .events_delivered
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
                metrics
                    .bytes_sent
                    .fetch_add(request.len() as u64, Ordering::Relaxed);
                trace!(
                    attempts = attempt + 1,
                    events = batch.len(),
                    "webhook batch delivered"
                );
                return Ok(());
            }
            Ok(Err(err)) => {
                trace!(attempt = attempt, error = %err, "webhook batch attempt failed");
                if attempt == config.retry_max {
                    return Err(err);
                }
            }
            Err(_elapsed) => {
                trace!(attempt = attempt, "webhook batch attempt timed out");
                if attempt == config.retry_max {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "webhook attempt timeout",
                    ));
                }
            }
        }
        time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
    Err(io::Error::other("retry budget exhausted"))
}

fn build_request(config: &WebhookSinkConfig, body: &[u8]) -> io::Result<Vec<u8>> {
    validate_request_target(config)?;
    let host = if config.host_header.is_empty() {
        config.addr.as_str()
    } else {
        config.host_header.as_str()
    };
    let mut req = Vec::with_capacity(body.len() + 256);
    req.extend_from_slice(b"POST ");
    req.extend_from_slice(config.path.as_bytes());
    req.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(b"\r\nUser-Agent: aiondb-changefeed/1\r\n");
    req.extend_from_slice(b"Content-Type: application/json\r\nContent-Length: ");
    req.extend_from_slice(body.len().to_string().as_bytes());
    req.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    req.extend_from_slice(body);
    Ok(req)
}

fn validate_request_target(config: &WebhookSinkConfig) -> io::Result<()> {
    if config.addr.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "webhook addr must not be empty",
        ));
    }
    validate_printable_ascii("webhook addr", &config.addr, MAX_WEBHOOK_ADDR_BYTES)?;
    if config.path.is_empty() || !config.path.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "webhook path must start with '/'",
        ));
    }
    validate_printable_ascii("webhook path", &config.path, MAX_WEBHOOK_PATH_BYTES)?;

    if !config.host_header.is_empty() {
        validate_printable_ascii(
            "webhook Host header",
            &config.host_header,
            MAX_WEBHOOK_HOST_HEADER_BYTES,
        )?;
    }
    Ok(())
}

fn validate_printable_ascii(label: &str, value: &str, max_len: usize) -> io::Result<()> {
    if value.len() > max_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} exceeds {max_len} bytes"),
        ));
    }
    if value.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} contains invalid HTTP characters"),
        ));
    }
    Ok(())
}

async fn send_one(addr: &str, request: &[u8]) -> io::Result<()> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(request).await?;
    stream.flush().await?;
    // Read at least the status line + headers to learn the response code.
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty response",
        ));
    }
    let head = &buf[..n];
    // Parse the status line: HTTP/1.x <code> <reason>\r\n
    let line_end = head.iter().position(|b| *b == b'\n').unwrap_or(head.len());
    let status_line = std::str::from_utf8(&head[..line_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 status line"))?;
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let code = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no status code"))?;
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(io::Error::other(format!("HTTP status {code}")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use aiondb_core::RelationId;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    use super::*;
    use crate::changefeed::{ChangefeedBus, ChangefeedConfig, ChangefeedEvent, ChangefeedFilter};

    struct TestServer {
        addr: String,
        captured: Arc<StdMutex<Vec<Vec<u8>>>>,
        #[allow(dead_code)]
        fail_first_n: Arc<std::sync::atomic::AtomicUsize>,
        shutdown_tx: watch::Sender<bool>,
        handle: JoinHandle<()>,
    }

    impl TestServer {
        async fn start(fail_first_n: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let captured = Arc::new(StdMutex::new(Vec::new()));
            let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
            let fail = Arc::new(std::sync::atomic::AtomicUsize::new(fail_first_n));
            let captured_clone = Arc::clone(&captured);
            let fail_clone = Arc::clone(&fail);
            let handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                return;
                            }
                        }
                        accept = listener.accept() => if let Ok((mut stream, _)) = accept {
                            let captured = Arc::clone(&captured_clone);
                            let fail = Arc::clone(&fail_clone);
                            tokio::spawn(async move {
                                let mut buf = vec![0u8; 16 * 1024];
                                let mut total = Vec::new();
                                loop {
                                    match stream.read(&mut buf).await {
                                        Ok(0) | Err(_) => break,
                                        Ok(n) => {
                                            total.extend_from_slice(&buf[..n]);
                                            // Crude: stop once we've seen body length.
                                            if let Some(body_start) =
                                                find_double_crlf(&total)
                                            {
                                                let content_length = parse_content_length(
                                                    &total[..body_start],
                                                )
                                                .unwrap_or(0);
                                                if total.len() >= body_start + content_length {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                let remaining = fail.load(std::sync::atomic::Ordering::SeqCst);
                                if remaining > 0 {
                                    fail.store(
                                        remaining - 1,
                                        std::sync::atomic::Ordering::SeqCst,
                                    );
                                    let _ = stream
                                        .write_all(
                                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                        )
                                        .await;
                                } else {
                                    captured.lock().unwrap().push(total);
                                    let _ = stream
                                        .write_all(
                                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                        )
                                        .await;
                                }
                                let _ = stream.shutdown().await;
                            });
                        }
                    }
                }
            });
            Self {
                addr,
                captured,
                fail_first_n: fail,
                shutdown_tx,
                handle,
            }
        }

        fn captured(&self) -> Vec<Vec<u8>> {
            self.captured.lock().unwrap().clone()
        }

        async fn shutdown(self) {
            let _ = self.shutdown_tx.send(true);
            let _ = self.handle.await;
        }
    }

    fn find_double_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
    }

    fn parse_content_length(header: &[u8]) -> Option<usize> {
        let text = std::str::from_utf8(header).ok()?;
        for line in text.split("\r\n") {
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                return rest.parse().ok();
            }
            if let Some(rest) = line.strip_prefix("content-length: ") {
                return rest.parse().ok();
            }
        }
        None
    }

    fn rel(n: u64) -> RelationId {
        RelationId::new(n)
    }

    fn insert_event(commit_ts: u64, table: RelationId) -> ChangefeedEvent {
        ChangefeedEvent::Insert {
            commit_ts,
            table,
            tuple_id: aiondb_core::TupleId::new(1),
            row: aiondb_core::Row::default(),
        }
    }

    #[tokio::test]
    async fn webhook_sink_delivers_batched_events() {
        let server = TestServer::start(0).await;
        let bus = ChangefeedBus::new(ChangefeedConfig::default());
        let config = WebhookSinkConfig {
            batch_size: 2,
            max_batch_delay: Duration::from_millis(50),
            retry_max: 0,
            ..WebhookSinkConfig::new(server.addr.clone(), "/cdc")
        };
        let sink = WebhookSink::spawn(&bus, ChangefeedFilter::all_tables(), config);

        bus.emit_autocommit(insert_event(1, rel(1)));
        bus.emit_autocommit(insert_event(2, rel(1)));
        bus.emit_autocommit(insert_event(3, rel(1)));
        // Wait long enough for both the size-triggered batch (events 1+2)
        // and the timer-triggered batch (event 3) to flush.
        time::sleep(Duration::from_millis(200)).await;

        let captured = server.captured();
        assert!(
            !captured.is_empty(),
            "server should have received at least one batch"
        );
        let snap = sink.metrics().snapshot();
        assert!(snap.batches_delivered >= 1);
        assert!(snap.events_delivered >= 2);
        sink.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn webhook_sink_retries_on_5xx_then_succeeds() {
        let server = TestServer::start(2).await; // fail first 2 attempts
        let bus = ChangefeedBus::new(ChangefeedConfig::default());
        let config = WebhookSinkConfig {
            batch_size: 1,
            max_batch_delay: Duration::from_millis(10),
            retry_max: 5,
            retry_initial_delay: Duration::from_millis(20),
            request_timeout: Duration::from_millis(500),
            ..WebhookSinkConfig::new(server.addr.clone(), "/cdc")
        };
        let sink = WebhookSink::spawn(&bus, ChangefeedFilter::all_tables(), config);

        bus.emit_autocommit(insert_event(1, rel(1)));
        // Let retries complete.
        time::sleep(Duration::from_millis(800)).await;

        let snap = sink.metrics().snapshot();
        assert!(snap.batches_delivered >= 1, "snap: {snap:?}");
        assert!(
            snap.retry_attempts >= 2,
            "expected at least 2 retries, got {snap:?}"
        );
        // Server should have observed exactly one captured (final) batch.
        let captured = server.captured();
        assert_eq!(captured.len(), 1, "exactly one successful POST");
        sink.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn webhook_sink_drops_batch_after_exhausting_retries() {
        let server = TestServer::start(100).await; // fail everything
        let bus = ChangefeedBus::new(ChangefeedConfig::default());
        let config = WebhookSinkConfig {
            batch_size: 1,
            max_batch_delay: Duration::from_millis(10),
            retry_max: 2,
            retry_initial_delay: Duration::from_millis(5),
            request_timeout: Duration::from_millis(200),
            ..WebhookSinkConfig::new(server.addr.clone(), "/cdc")
        };
        let sink = WebhookSink::spawn(&bus, ChangefeedFilter::all_tables(), config);

        bus.emit_autocommit(insert_event(1, rel(1)));
        time::sleep(Duration::from_millis(400)).await;

        let snap = sink.metrics().snapshot();
        assert_eq!(snap.batches_delivered, 0, "no batch should succeed");
        assert!(snap.batches_dropped >= 1, "snap: {snap:?}");
        sink.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn webhook_sink_respects_filter() {
        let server = TestServer::start(0).await;
        let bus = ChangefeedBus::new(ChangefeedConfig::default());
        let config = WebhookSinkConfig {
            batch_size: 1,
            max_batch_delay: Duration::from_millis(20),
            retry_max: 0,
            ..WebhookSinkConfig::new(server.addr.clone(), "/cdc")
        };
        // Only table 1.
        let filter = ChangefeedFilter::tables([rel(1)]);
        let sink = WebhookSink::spawn(&bus, filter, config);

        bus.emit_autocommit(insert_event(1, rel(1)));
        bus.emit_autocommit(insert_event(2, rel(2))); // filtered out
        bus.emit_autocommit(insert_event(3, rel(1)));
        time::sleep(Duration::from_millis(200)).await;

        let snap = sink.metrics().snapshot();
        assert_eq!(
            snap.events_delivered, 2,
            "filtered events must not reach the sink: {snap:?}"
        );
        sink.shutdown().await;
        server.shutdown().await;
    }

    #[test]
    fn build_request_includes_content_length_and_path() {
        let cfg = WebhookSinkConfig::new("127.0.0.1:9999", "/cdc/v1");
        let body = b"[]".to_vec();
        let req = build_request(&cfg, &body).expect("valid webhook request");
        let text = std::str::from_utf8(&req).unwrap();
        assert!(text.starts_with("POST /cdc/v1 HTTP/1.1"));
        assert!(text.contains("Content-Length: 2"));
        assert!(text.ends_with("[]"));
    }

    #[test]
    fn build_request_rejects_header_injection_in_path() {
        let cfg = WebhookSinkConfig::new("127.0.0.1:9999", "/cdc\r\nX-Evil: 1");
        let err = build_request(&cfg, b"[]").expect_err("CRLF path must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn build_request_rejects_header_injection_in_host() {
        let mut cfg = WebhookSinkConfig::new("127.0.0.1:9999", "/cdc");
        cfg.host_header = "primary\r\nX-Evil: 1".to_owned();
        let err = build_request(&cfg, b"[]").expect_err("CRLF host must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn build_request_rejects_relative_path() {
        let cfg = WebhookSinkConfig::new("127.0.0.1:9999", "cdc");
        let err = build_request(&cfg, b"[]").expect_err("relative path must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
