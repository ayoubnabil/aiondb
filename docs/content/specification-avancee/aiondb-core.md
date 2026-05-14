---
title: aiondb-core
order: 10
---

# aiondb-core

Shared runtime types used by every other crate. Defines the value model, column data types, identifiers, errors with `SQLSTATE` codes, and a few cross-cutting helpers (text encoding, network address types, checksums, temporal formatting). It has no internal dependency on the rest of the workspace; everything else depends on it.

## Cargo

```toml
[dependencies]
aiondb-core = { path = "../aiondb-core" }
```

## Modules

| Module | Purpose |
|---|---|
| `value` | Runtime tagged value (`Value`) and `VectorValue`. |
| `data_type` | Column/expression type descriptor (`DataType`). |
| `row` | `Row` wrapper around `Vec<Value>`. |
| `error` | `DbError`, `DbResult<T>`, `SqlState`, `ErrorReport`. |
| `ids` | Strongly typed identifier wrappers (`TxnId`, `RelationId`, ...). |
| `numeric` | `NumericValue` (arbitrary precision) and `IntervalValue`. |
| `temporal` | `PgDate`, `DateStyleSetting`, `TimeZoneSetting`, `DateOrder`. |
| `network` | `MacAddr`, `MacAddr8`. |
| `pg_lsn` | `PgLsnValue` (Postgres log sequence number). |
| `tid` | `TidValue` (Postgres tuple identifier). |
| `text_utils` | `escape_sql_literal`, `hex_encode`, `pg_array_unescape_quoted`. |
| `checksum` | Page/buffer checksum helpers. |
| `convert` | Value-to-value conversions used by the cast paths. |
| `fk` | `FkAction`, `FkMatchType` enums for foreign key descriptors. |
| `identity` | `IdentitySpec`, `IdentityOptions`, `IdentityGeneration`. |
| `pg_compat` | Constants and OIDs that match Postgres defaults. |
| `vector_limits` | Bounds for vector index parameters. |
| `vector_storage` | Layout helpers for stored vectors. |
| `bounded_io` | Length-bounded readers/writers used at the engine boundary to keep adversarial peer input from exhausting memory. |
| `replication_fs` | Filesystem helpers (atomic file create, directory fsync, tmp-then-rename) used by the replication metadata writers. |
| `sql_trace` | SQL trace-id propagation helpers and trace-id types. |
| `trace_context` | Per-task tracing context, including the active trace-id slot used by background workers. |

## Values

`Value` is the runtime tagged union for every column value. Variants (definition in `crates/aiondb-core/src/value.rs`):

```text
Null  Int  BigInt  Real  Double  Numeric  Money  Text  Boolean  Blob
Timestamp  TimestampTz  Date  LargeDate  Time  TimeTz  Interval
Uuid  Tid  PgLsn  Jsonb  MacAddr  MacAddr8  Vector  Array
```

`Blob` holds raw bytes (`Vec<u8>`). `LargeDate` carries `PgDate` for years outside the range representable by `time::Date`. `Tid` and `PgLsn` exist so PostgreSQL system columns and system functions can be returned without lossy conversions.

```rust
use aiondb_core::Value;

let v = Value::Int(42);
assert!(matches!(v, Value::Int(_)));

let v_text = Value::Text("hello".to_string());
let v_null = Value::Null;
```

## Rows

```rust
use aiondb_core::{Row, Value};

let row = Row::new(vec![Value::Int(1), Value::Text("alice".to_string())]);
assert_eq!(row.len(), 2);
for v in row.iter() {
    println!("{:?}", v);
}
```

## Data types

`DataType` is the static type descriptor used by the planner and the executor.

```rust
use aiondb_core::DataType;

let int_type = DataType::Int;
let varchar = DataType::VarChar { length: 64 };
```

Variants include `Int`, `BigInt`, `Real`, `Double`, `Numeric`, `Boolean`, `Text`, `Char { length }`, `VarChar { length }`, `Date`, `Time`, `TimeTz`, `Timestamp`, `TimestampTz`, `Interval`, `Uuid`, `Jsonb`, `MacAddr`, `MacAddr8`, and `Vector { ... }`.

## Errors

Every fallible API returns `DbResult<T> = Result<T, DbError>`. Each `DbError` carries an explicit `SqlState`.

```rust
use aiondb_core::{DbError, SqlState};

let err = DbError::new(SqlState::UndefinedTable, "relation \"users\" does not exist");
assert_eq!(err.sqlstate.code(), "42P01");
```

Convenience constructors group errors by category:

```rust
use aiondb_core::{DbError, SqlState};

let _ = DbError::syntax_error("missing ;");
let _ = DbError::authentication_error("invalid password");
let _ = DbError::not_authorized("permission denied for table users");
let _ = DbError::transaction_error(SqlState::SerializationFailure, "could not serialize");
```

`SqlState::code()` returns the five-character Postgres SQLSTATE string. `SqlState::from_code(...)` parses it back.

## Identifiers

Strongly typed wrappers over `u64`. Each one is `Copy`, `Debug`, and `serde`-friendly. The macro defines them uniformly:

```rust
use aiondb_core::{RelationId, TxnId};

let table_id = RelationId::new(42);
let txn = TxnId::new(7);
assert_eq!(table_id.get(), 42);
```

Available identifiers: `TxnId`, `TenantId`, `DatabaseId`, `SchemaId`, `RelationId`, `IndexId`, `ColumnId`, `SequenceId`, `TupleId`.

## Numerics

`NumericValue` provides arbitrary-precision decimals, used to back the SQL `NUMERIC` type. `IntervalValue` represents `INTERVAL` with separate months / days / microseconds components.

```rust
use aiondb_core::{IntervalValue, NumericValue};

let n: NumericValue = "1234.5678".parse().expect("valid numeric literal");
let interval = IntervalValue::from_components(1, 0, 0); // 1 month
let _ = (n, interval);
```

## Vectors

`VectorValue` carries the element type plus the dimensions. `vector_limits` exposes bounds applied by the planner:

```rust
use aiondb_core::{HNSW_BASELINE_EF_SEARCH, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K};

assert!(HNSW_BASELINE_EF_SEARCH <= HNSW_MAX_EF_SEARCH);
assert!(VECTOR_MAX_K > 0);
```

## Network types

```rust
use aiondb_core::{MacAddr, MacAddr8};

let mac = MacAddr::new([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]);
let mac8 = MacAddr8::new([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70]);
let _ = (mac, mac8);
```

## Text utilities

```rust
use aiondb_core::{escape_sql_literal, hex_encode};

assert_eq!(escape_sql_literal("o'reilly"), "'o''reilly'");
assert_eq!(hex_encode(b"AB"), "4142");
```

## Postgres compatibility constants

The `pg_compat` module exposes the OIDs and default settings AionDB returns to clients that introspect the server (for example, `pg_catalog`, `information_schema`, `client_encoding`, `server_version_num`).

```rust
use aiondb_core::{compat_server_version_num_string, COMPAT_PG_CATALOG_NAMESPACE_OID};

let _version = compat_server_version_num_string();
let _oid = COMPAT_PG_CATALOG_NAMESPACE_OID;
```
