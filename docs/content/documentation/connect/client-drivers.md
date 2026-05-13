---
title: Client Drivers
order: 56
---

# Client Drivers

AionDB's server surface is PostgreSQL wire protocol, so PostgreSQL clients are the natural integration path.

## psql

```bash
psql "host=127.0.0.1 port=5432 dbname=default user=dev password=DevPassword42! sslmode=disable"
```

For local anonymous-style evaluation with no bootstrap credentials, some setups may allow connecting to `default` directly. Prefer bootstrap credentials when testing driver behavior.

Useful smoke test:

```sql
SELECT 1;
CREATE TABLE driver_smoke (id INT PRIMARY KEY, body TEXT);
INSERT INTO driver_smoke VALUES (1, 'ok');
SELECT id, body FROM driver_smoke WHERE id = 1;
```

If this fails, fix the connection path before testing an ORM. Driver-level failures are easier to understand before generated SQL enters the picture.

## Rust

Use PostgreSQL crates against the server, or use the embedded API when you want in-process execution.

Embedded:

```rust
use aiondb_embedded::Database;

let db = Database::in_memory()?;
let conn = db.connect_anonymous("default", "app")?;
conn.execute("SELECT 1;")?;
```

When testing a PostgreSQL Rust driver, validate both simple query and prepared statement paths. Some drivers use extended protocol by default, so a query that works in `psql` can still expose a protocol issue in application code.

## Python, Go, Node.js, Java

Use the normal PostgreSQL driver for your language, then validate:

- startup parameters;
- prepared statements;
- binary vs text formats;
- transaction error behavior;
- type mapping;
- connection pooling behavior.

Recommended order:

1. connect and run `SELECT 1`;
2. create a table and insert one row;
3. run a parameterized query;
4. run a transaction with rollback;
5. run the application migration layer;
6. run a representative query set.

Keep the driver version in bug reports. Different versions can generate different startup parameters and prepared statement behavior.

## ORM guidance

ORMs generate SQL that can be wider than hand-written application SQL. Start with migrations disabled, run a small query set, then add generated DDL and migrations once basic query behavior is stable.

Keep every incompatibility as a reduced SQL repro. That is more useful than a framework-level failure report.

## Connection pooling

Connection pools can hide errors by retrying, resetting sessions, or changing transaction boundaries. During early compatibility work, test without a pool first. Add pooling only after direct connections are stable.

When enabling a pool, validate:

- session startup settings;
- idle transaction cleanup;
- connection reset SQL;
- prepared statement cache behavior;
- how errors are surfaced to the application.
