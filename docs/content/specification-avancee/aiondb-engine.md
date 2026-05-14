---
title: aiondb-engine
order: 50
---

# aiondb-engine

Top-level orchestration crate. Wires the parser, planner, optimizer, executor, eval, catalog, storage, security, and transaction subsystems behind a single `Engine` value. Facade crates (`aiondb-embedded`, `aiondb-pgwire`, `aiondb-dashboard`) hold an `Arc<Engine>` and call into it via narrow traits exposed from `engine::api`.

## cargo

```toml
[dependencies]
aiondb-engine = { path = "../aiondb-engine" }
```

## modules

| Module | Purpose |
|---|---|
| `builder` | `EngineBuilder` constructors (`new_in_memory`, `new_durable`, `new_durable_with_config`, `new_with_config`, `for_testing`). |
| `config` | `EngineConfig` and `session_limits_from_config`. |
| `engine` | The `Engine` value plus the narrow facade traits in `engine::api`. |
| `prepared` | Prepared statement and portal types (`PreparedStatementDesc`, `PortalBatch`, `StatementResult`, `ResultColumn`). |
| `session` | `SessionHandle`, `SessionInfo`, `SessionLimits`. |

Internal modules (`auth_audit`, `catalog_authorizer`, `catalog_auth`, `params`) are not part of the public surface.

## key types

| Type | Role |
|---|---|
| `Engine` | Concrete engine implementation. Built via `EngineBuilder`, used through `Arc<Engine>`. |
| `EngineBuilder` | Builder for `Engine`. Selects the storage profile and lets callers override authenticator, authorizer, rate limiter, transaction manager, snapshot oracle, lock manager, catalog, sequences, storage DDL/DML, and fragment dispatcher. |
| `QueryEngine` | Aggregate trait implemented by `Engine`. Bundles startup, simple query, extended protocol, transactions, replication, session control, and wire compatibility. |
| `PgWireEngine` | Marker trait used by `aiondb-pgwire` to constrain the engine type. |
| `QuerySimpleSql`, `QueryExtendedProtocol`, `QueryTransactions`, `QueryStartup`, `QuerySessionControl`, `QueryReplication`, `QueryWireCompatibility` | Narrow traits in `engine::api` that facades depend on instead of `QueryEngine` when only one capability is needed. |
| `StartupParams`, `StartupAuthentication` | Inputs to `QueryStartup::startup`. |
| `SessionHandle`, `SessionInfo`, `SessionLimits` | Per-connection session state. |
| `PreparedStatementDesc`, `PortalBatch`, `ResultColumn`, `ResultColumnOrigin`, `StatementResult`, `PortalDescription` | Extended protocol results. |
| `EngineMetrics`, `EngineMetricsSnapshot` | Engine-level counters. |
| `ReplicationIdentity`, `EngineReplicationSeedManifest`, `install_replication_seed` | Replication seed installation. |
| `SqlStatementWireMetadata`, `WireStateCleanupHint` | Hints returned to the wire layer. |
| `AuthAuditQuery`, `AuthAuditRecord` | Authentication audit log query API. |

## re-exports

`aiondb-engine` re-exports a curated subset from its dependencies for callers that want to avoid pulling additional crates directly:

| From | Items |
|---|---|
| `aiondb-core` | `DataType`, `DbError`, `DbResult`, `ErrorReport`, `Row`, `SqlState`, `Value`. |
| `aiondb-security` | `AccessRequest`, `AccessTarget`, `Action`, `AllowAllAuthorizer`, `AuthRateLimiter`, `AuthenticatedIdentity`, `Authenticator`, `Authorizer`, `Credential`, `FileBackedAuthRateLimiter`, `InMemoryAuthRateLimiter`, `ScramVerifier`, `SecretBytes`, `SecretString`, `TransportInfo`, `TransportKind`. |
| `aiondb-tx` | `IsolationLevel`. |

## example

```rust,no_run
use std::sync::Arc;
use aiondb_engine::{
    AllowAllAuthorizer, Credential, EngineBuilder, QueryEngine,
    StartupParams, TransportInfo, TransportKind,
};

fn main() -> aiondb_engine::DbResult<()> {
    let engine = EngineBuilder::new_in_memory()
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()?;

    let (session, _info) = engine.startup(StartupParams {
        database: "default".into(),
        application_name: Some("demo".into()),
        options: Default::default(),
        credential: Credential::Anonymous { user: "alice".into() },
        transport: TransportInfo { kind: TransportKind::InProcess },
    })?;

    let _ = engine.execute_sql(&session, "CREATE TABLE t (id INT);")?;
    engine.terminate(session)?;
    Ok(())
}
```
