---
title: aiondb-api
order: 51
---

# aiondb-api

Stable public-interface crate. Holds the small contract types that facades and external tooling depend on, so that they need not import engine internals directly. The crate is intentionally thin: most of its surface is re-exports from `aiondb-pg-compat`, plus a minimal `DatabaseSurface` trait.

## cargo

```toml
[dependencies]
aiondb-api = { path = "../aiondb-api" }
```

## modules

| Module | Purpose |
|---|---|
| `database` | `DatabaseSurface` trait, `DatabaseCapability`, `ExecutionOutcome`. |

The crate also re-exports `CompatCommand`, `CompatCommandFamily`, `CompatCommandHandler`, `PgCompatHooks`, `compat_error`, `compat_missing_object_notice`, and `CompatFailureKind` from `aiondb-pg-compat`.

## key types

| Type | Role |
|---|---|
| `DatabaseSurface` | Minimal trait describing what an engine must offer to back a facade. Defines an associated `Session` type, `execute_sql_outline`, `has_capability`, and `terminate_session`. |
| `DatabaseCapability` | Enum of optional features a host may report: `PreparedStatements`, `Copy`, `Notify`, `Replication`, `Backup`, `Vector`, `Graph`. |
| `ExecutionOutcome` | Per-statement summary used by facades to format `CommandComplete` tags. Variants: `Command { tag, rows_affected }` and `Rows { tag, columns, row_count }`. |
| `CompatCommand`, `CompatCommandFamily`, `CompatCommandHandler` | PostgreSQL compatibility command dispatch (re-exported). |
| `PgCompatHooks` | Hook surface used by the engine to delegate compat-only commands. |
| `CompatFailureKind`, `compat_error`, `compat_missing_object_notice` | Error helpers for compatibility paths. |

## example

```rust
use aiondb_api::ExecutionOutcome;

let outcome = ExecutionOutcome::Command {
    tag: "CREATE TABLE".to_owned(),
    rows_affected: 0,
};

assert_eq!(outcome.tag(), "CREATE TABLE");
assert_eq!(outcome.rows_affected(), 0);
```
