---
title: aiondb-dashboard
order: 55
---

# aiondb-dashboard

HTTP companion dashboard server. Embeds an in-memory `aiondb-engine` instance, exposes a small REST API for queries, and serves a token-based admin login. The dashboard is restricted to loopback binds; remote exposure must be terminated by an external TLS proxy.

By default, login requests that carry proxy forwarding headers are rejected. To place the dashboard behind a local HTTPS reverse proxy, operators must opt in with `AIONDB_DASHBOARD_TRUST_PROXY_TLS_HEADERS=true`, and the proxy must send `Forwarded: proto=https` or `X-Forwarded-Proto: https`.

This crate is legacy and is not the product dashboard. The product dashboard is
`AionDB Studio` under `integrations/aiondb-studio/`, an adapted pgweb fork that
uses PostgreSQL wire protocol. Use AionDB Studio for table browsing, schema
exploration, connection management, SQL, Cypher snippets, export, history, and
graph result preview.

## cargo

```toml
[dependencies]
aiondb-dashboard = { path = "../aiondb-dashboard" }
```

The crate also ships an `aiondb-dashboard` binary built from `src/main.rs`.

## modules

| Module | Purpose |
|---|---|
| `api` | Axum route handlers for the dashboard REST API. |
| `auth` | Session secret (`SessionSecret`) and in-memory `SessionStore`. |
| `server` | Server config, engine builder, `DashboardServer` lifecycle. |

## key types

| Type | Role |
|---|---|
| `DashboardConfig` | Bind address, port, `max_sessions`, `session_timeout`, `max_query_length`, `max_result_rows`, `query_timeout`. Defaults: `127.0.0.1:8080`, 64 sessions, 30 min idle, 64 KiB query limit, 10 000 row limit, 30 s query timeout. |
| `BootstrapAdmin` | Initial admin role created on startup. Holds `username` and `password`; `Debug` redacts the password. |
| `DashboardServer` | The dashboard runtime. Built with `DashboardServer::new`, started with `start(shutdown)`, populates the admin role through `bootstrap_admin`. |
| `build_dashboard_engine` | Builds the embedded in-memory `Arc<Engine>` used by the dashboard with the staging security profile and tightened session limits. |

## binary environment

The `aiondb-dashboard` binary reads:

| Variable | Effect |
|---|---|
| `AIONDB_DASHBOARD_BIND` | Bind address. Default: `127.0.0.1`. The binary refuses non-loopback binds. |
| `AIONDB_DASHBOARD_PORT` | Listen port. Default: `8080`. |
| `AIONDB_DASHBOARD_PROMETHEUS_UNAUTHENTICATED` | Enable unauthenticated `/api/metrics-prom`. Default: `false`. |
| `AIONDB_DASHBOARD_TRUST_PROXY_TLS_HEADERS` | Trust `Forwarded` / `X-Forwarded-Proto` from a local reverse proxy and treat `proto=https` as TLS for password login. Default: `false`. |
| `AIONDB_ADMIN_USER`, `AIONDB_ADMIN_PASSWORD` | Optional explicit bootstrap credentials. Both must be set together; otherwise the binary generates a random admin password and logs it on a TTY. |

Tracing is initialized via the standard `RUST_LOG`-style `EnvFilter`; defaults to `info` if no filter is set.

## example

Run the dashboard with explicit admin credentials:

```sh
AIONDB_ADMIN_USER=admin \
AIONDB_ADMIN_PASSWORD='AdminPassword42!' \
AIONDB_DASHBOARD_BIND=127.0.0.1 \
AIONDB_DASHBOARD_PORT=8080 \
aiondb-dashboard
```

## AionDB Studio

Run the pgweb-based pgwire dashboard adapted for AionDB:

```sh
make dashboard-studio
```

For the one-command container profile:

```sh
docker compose --profile studio up
```

Then open `http://127.0.0.1:8082`.

The implementation lives in `integrations/aiondb-studio/`. It is based on
pgweb and expects AionDB pgwire on `127.0.0.1:5432` by default.

Embed the dashboard in another binary:

```rust,no_run
use aiondb_dashboard::{
    build_dashboard_engine, BootstrapAdmin, DashboardConfig, DashboardServer,
};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DashboardConfig::default();
    let engine = build_dashboard_engine()?;
    let server = DashboardServer::new(engine, config);

    server
        .bootstrap_admin(&BootstrapAdmin {
            username: "admin".into(),
            password: "AdminPassword42!".into(),
        })
        .ok();

    let (_tx, rx) = watch::channel(false);
    server.start(rx).await?;
    Ok(())
}
```
