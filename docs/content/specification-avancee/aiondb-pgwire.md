---
title: aiondb-pgwire
order: 53
---

# aiondb-pgwire

PostgreSQL wire protocol (v3) implementation. Provides a TCP listener, the startup and SSL request handshakes, the simple query protocol, the extended query protocol (`Parse` / `Bind` / `Describe` / `Execute` / `Sync`) including text and binary formats, and `SQLSTATE`-aware error mapping. The server is generic over any type implementing `aiondb_engine::PgWireEngine`.

## cargo

```toml
[dependencies]
aiondb-pgwire = { path = "../aiondb-pgwire" }
```

## modules

| Module | Purpose |
|---|---|
| `codec` | Low-level binary frame reader/writer. |
| `messages` | Typed frontend and backend message structs. |
| `connection` | Per-client connection state machine. |
| `server` | TCP listener and connection-spawning entry point. |
| `format` | `Value`-to-text serialization for the PostgreSQL text format. |
| `binary_format` | `Value`-to-bytes serialization for the binary format. |
| `engine_pool` | Bounded blocking dispatcher used to call into the engine from async tasks. |
| `replication` | Logical/physical replication endpoints. |
| `tls` | `TlsConfig` and acceptor construction (`validate_tls_config`, `build_tls_acceptor`). |

## key types

| Type | Role |
|---|---|
| `server::PgWireServer<E>` | The TCP server. Constructed with `PgWireServer::new` (TLS-aware) or `PgWireServer::new_plain`. |
| `server::PgWireConfig` | Listen address, port, connection limits, per-IP limits, startup and shutdown timeouts, idle timeout, auth-failure backoff, engine pool config, optional `TlsConfig`, `require_tls`, `fail_on_weak_rng`, `max_portals`. |
| `server::CancelRegistry` | Maps `(pid, secret_key)` to a `SessionHandle` for `CancelRequest` handling. |
| `server::ServerMetrics`, `server::ServerMetricsSnapshot` | Atomic counters for accepted connections, active connections, queries, startups, and authentication failures. |
| `server::ServerHealthState`, `server::ServerHealthSnapshot` | Server health view used by the observability HTTP endpoint. |
| `tls::TlsConfig`, `tls::validate_tls_config` | TLS material and validation. |
| `engine_pool::EnginePool` | Bounded blocking dispatcher used internally by `PgWireServer`. |

## example

```rust,no_run
use std::sync::Arc;
use aiondb_engine::EngineBuilder;
use aiondb_pgwire::server::{PgWireConfig, PgWireServer};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let engine = Arc::new(
        EngineBuilder::new_in_memory()
            .build()
            .expect("build engine"),
    );

    let config = PgWireConfig {
        bind_address: "127.0.0.1".into(),
        port: 5432,
        require_tls: false,
        ..Default::default()
    };

    let server = PgWireServer::new_plain(engine, config);
    let (_tx, rx) = watch::channel(false);
    server.start(rx).await
}
```
