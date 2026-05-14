---
title: aiondb-embedded
order: 52
---

# aiondb-embedded

Synchronous in-process facade around `aiondb-engine`. Targets applications that want to embed the database without speaking the PostgreSQL wire protocol. The embedded profile installs `AllowAllAuthorizer` and permits anonymous local users, so it is suitable for tests, tooling, and single-tenant in-process use.

## cargo

```toml
[dependencies]
aiondb-embedded = { path = "../aiondb-embedded" }
```

## modules

The crate is a single `lib.rs` module; it re-exports the pieces needed for normal embedded use.

| Re-export | From |
|---|---|
| `RuntimeConfig` | `aiondb-config` |
| `Credential`, `DbResult`, `Engine`, `IsolationLevel`, `PortalBatch`, `PreparedStatementDesc`, `QueryEngine`, `ResultColumn`, `SessionHandle`, `SessionInfo`, `StatementResult`, `Value` | `aiondb-engine` |

## key types

| Type | Role |
|---|---|
| `Database<E: QueryEngine>` | Owns an `Arc<E>`. Built via `Database::in_memory`, `Database::open`, `Database::open_with_config`, `Database::open_with_profile`, or `Database::new` to wrap a custom engine. |
| `OpenProfile` | Selects the storage profile: `InMemory`, `Durable { data_dir }`, `DurableWithConfig { data_dir, runtime_config }`. |
| `ConnectOptions` | Startup options: `database`, `credential`, optional `application_name`. `ConnectOptions::anonymous` builds a local-anonymous credential. |
| `Connection<E>` | Opened with `Database::connect` or `Database::connect_anonymous`. Exposes `identity`, `execute`, `prepare`, and `transaction`. Drops by terminating the session. |
| `PreparedStatement<E>` | Returned from `Connection::prepare`. Exposes `descriptor`, `execute`, and `resume`. Drops by closing the named statement. |

## example

```rust,no_run
use aiondb_embedded::{Database, IsolationLevel};

fn main() -> aiondb_embedded::DbResult<()> {
    let db = Database::in_memory()?;
    let conn = db.connect_anonymous("default", "app")?;

    conn.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT);")?;

    conn.transaction(IsolationLevel::ReadCommitted, |tx| {
        tx.execute("INSERT INTO t VALUES (1, 'hello');")?;
        tx.execute("INSERT INTO t VALUES (2, 'world');")?;
        Ok(())
    })?;

    let _ = conn.execute("SELECT * FROM t;")?;
    Ok(())
}
```
