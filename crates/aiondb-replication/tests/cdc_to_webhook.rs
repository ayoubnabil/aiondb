//! Full CDC pipeline integration : KV write → CDC adapter →
//! ChangefeedBus → WebhookSink → HTTP receiver.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_replication::cdc_adapter::KvCdcAdapter;
use aiondb_replication::changefeed::{ChangefeedBus, ChangefeedConfig, ChangefeedFilter};
use aiondb_replication::webhook_sink::{WebhookSink, WebhookSinkConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;

struct TestWebhookServer {
    addr: String,
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    shutdown_tx: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

async fn start_test_server() -> TestWebhookServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let received = Arc::new(Mutex::new(Vec::new()));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let captured = Arc::clone(&received);
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                accept = listener.accept() => if let Ok((mut stream, _)) = accept {
                    let captured = Arc::clone(&captured);
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 16 * 1024];
                        let mut total = Vec::new();
                        loop {
                            match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    total.extend_from_slice(&buf[..n]);
                                    if let Some(body_start) = total
                                        .windows(4)
                                        .position(|w| w == b"\r\n\r\n")
                                        .map(|p| p + 4)
                                    {
                                        let header = &total[..body_start];
                                        let content_length = parse_content_length(header).unwrap_or(0);
                                        if total.len() >= body_start + content_length {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        captured.lock().unwrap().push(total);
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        let _ = stream.shutdown().await;
                    });
                }
            }
        }
    });
    TestWebhookServer {
        addr,
        received,
        shutdown_tx,
        handle,
    }
}

fn parse_content_length(header: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(header).ok()?;
    for line in text.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            return rest.parse().ok();
        }
    }
    None
}

#[tokio::test]
async fn kv_writes_propagate_through_cdc_to_webhook() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let g = MultiRaftGroupId::new(1);
    registry.create_group(g, 1).unwrap();
    registry.become_leader(g, &[]).unwrap();
    let engine = KvEngine::new(Arc::clone(&registry));

    let bus = ChangefeedBus::new(ChangefeedConfig::default());
    engine.set_observer(Arc::new(KvCdcAdapter::with_default_mapper(bus.clone())));

    let server = start_test_server().await;
    let sink = WebhookSink::spawn(
        &bus,
        ChangefeedFilter::all_tables(),
        WebhookSinkConfig {
            batch_size: 1,
            max_batch_delay: Duration::from_millis(10),
            retry_max: 1,
            retry_initial_delay: Duration::from_millis(5),
            request_timeout: Duration::from_millis(500),
            ..WebhookSinkConfig::new(server.addr.clone(), "/cdc")
        },
    );

    let total = Arc::new(AtomicUsize::new(0));
    for i in 0..5u8 {
        engine.put(g, vec![i], vec![i, 0xFF]).unwrap();
        total.fetch_add(1, Ordering::SeqCst);
    }
    time::sleep(Duration::from_millis(300)).await;

    let captured = server.received.lock().unwrap().clone();
    assert!(
        !captured.is_empty(),
        "webhook server should have received at least one POST"
    );
    let snap = sink.metrics().snapshot();
    assert!(snap.events_delivered >= 5, "snap: {snap:?}");

    sink.shutdown().await;
    let _ = server.shutdown_tx.send(true);
    let _ = server.handle.await;
}
