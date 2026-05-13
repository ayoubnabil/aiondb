---
title: Getting Started
order: 10
---

# Getting Started

This page gets AionDB running locally, creates a development user, connects with `psql`, and runs the first query. It is the shortest path for a local v0.1 evaluation.

## Requirements

- Docker Compose for the fastest prebuilt-image path.
- Rust toolchain compatible with the workspace only if you build from source.
- `psql` if you want to connect through the PostgreSQL wire protocol.
- Python 3.8 or newer only if you want to rebuild this documentation site.

## Get the source

Every command below runs from inside a checkout of the AionDB repository:

```bash
git clone https://github.com/ayoubnabil/aiondb.git
cd aiondb
```

## Fastest start

```bash
cp .env.example .env
$EDITOR .env
docker compose up
```

## Build From Source

```bash
cargo build --release -p aiondb-server --bin aiondb
```

During development, `cargo run` is usually enough:

```bash
cargo run -p aiondb-server --bin aiondb -- --help
```

## Start an in-memory server

Use `--ephemeral` for a clean local session. Data is stored in memory and disappears when the process exits.

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
cargo run -p aiondb-server --bin aiondb -- --ephemeral
```

`AIONDB_BOOTSTRAP_USER` and `AIONDB_BOOTSTRAP_PASSWORD` create a local development role at startup. Keep this pattern for driver tests because it exercises authentication instead of relying on anonymous local behavior.

## Connect

```bash
psql "host=127.0.0.1 port=5432 dbname=default user=dev password=DevPassword42! sslmode=disable"
```

Run a small SQL query:

```sql
CREATE TABLE tickets (
    id INT PRIMARY KEY,
    title TEXT,
    priority TEXT
);

INSERT INTO tickets VALUES
    (1, 'pgwire smoke test', 'high'),
    (2, 'embedded api check', 'normal');

SELECT id, title FROM tickets WHERE priority = 'high';
```

Expected result:

```text
 id |       title
----+-------------------
  1 | pgwire smoke test
```

## Persistent local data

Use a data directory for durable local testing:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
cargo run -p aiondb-server --bin aiondb -- --data-dir ./data/aiondb
```

Persistent storage expects filesystem-level encryption for production-like setups. `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true` is a development override.

## Embedded Rust

Use the embedded API when the database should run in the same process as a Rust application:

```rust
use aiondb_embedded::Database;

fn main() -> aiondb_embedded::DbResult<()> {
    let db = Database::in_memory()?;
    let conn = db.connect_anonymous("default", "app")?;

    conn.execute("CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);")?;
    conn.execute("INSERT INTO notes VALUES (1, 'hello from embedded mode');")?;
    let _rows = conn.execute("SELECT id, body FROM notes;")?;

    Ok(())
}
```

## Next steps

- [Core Concepts](/documentation/learn/core-concepts.html) explains the data model.
- [Tutorial](/documentation/start/tutorial.html) walks through SQL, graph labels, and vector scoring together.
- [Interfaces](/documentation/connect/interfaces.html) covers server and embedded usage.
- [Limitations](/documentation/evaluate/limitations.html) lists v0.1 boundaries.
