---
title: Interfaces
order: 50
---

# Interfaces

AionDB exposes the same engine through a PostgreSQL wire server and an embedded Rust API.

## Server mode

Run the server:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
cargo run -p aiondb-server --bin aiondb -- --ephemeral
```

Useful server options:

```bash
aiondb --ephemeral
aiondb --data-dir ./data/aiondb
aiondb --storage-backend durable
```

Important environment variables:

```bash
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432
AIONDB_STORAGE_BACKEND=durable
AIONDB_STORAGE_DATA_DIR=./data/aiondb
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true
AIONDB_BOOTSTRAP_USER=admin
AIONDB_BOOTSTRAP_PASSWORD='StrongPassword42!'
```

`AIONDB_ALLOW_UNENCRYPTED_STORAGE=true` is a development override. Persistent production-like data should live on encrypted storage.

Use server mode when testing:

- PostgreSQL drivers;
- network clients;
- authentication behavior;
- pgwire protocol compatibility;
- command-line configuration;
- observability endpoints.

Server mode is the public integration path for applications that already speak PostgreSQL wire protocol.

## Embedded Rust

Use the embedded API when the database should run in the same process as the application:

```rust
use aiondb_embedded::Database;

fn main() -> aiondb_embedded::DbResult<()> {
    let db = Database::in_memory()?;
    let conn = db.connect_anonymous("default", "app")?;

    conn.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT);")?;
    conn.execute("INSERT INTO t VALUES (1, 'hello');")?;
    let _rows = conn.execute("SELECT * FROM t;")?;

    Ok(())
}
```

Durable embedded usage opens a data directory:

```rust
let db = Database::open("./data/aiondb")?;
```

Use embedded mode when:

- the application is written in Rust;
- you want local database behavior without a separate server process;
- tests need an in-memory database;
- the process should own startup and shutdown directly.

Embedded mode should still be evaluated against server mode for semantic differences. If the same SQL behaves differently, keep a reduced repro.

## Client expectations

PostgreSQL clients can connect over pgwire, but not every PostgreSQL feature is implemented. Test the exact driver behavior you need, especially extended protocol, prepared statements, COPY, type mapping, and transaction behavior.

## Choosing a surface

| Requirement | Prefer |
| --- | --- |
| Existing PostgreSQL driver | Server mode |
| ORM compatibility testing | Server mode |
| Local Rust application | Embedded mode |
| Network boundary | Server mode |
| Lowest local integration overhead | Embedded mode |
| Benchmarking pgwire overhead | Server mode |

## Shutdown guidance

For ephemeral evaluation, stopping the process discards data. For persistent evaluation, stop the process cleanly when possible and keep the data directory for recovery debugging if a crash occurs.
