---
title: aiondb-pg-compat
order: 43
---

# aiondb-pg-compat

PostgreSQL compatibility layer. Houses the rewrites, parsers, registries, dispatch contracts, and error/message shaping that AionDB applies on top of its native engine to keep PostgreSQL clients and tooling working. The wire-protocol runner lives in `aiondb-pgwire` and the SQL engine itself lives in `aiondb-engine`. This crate is the dependency-light home for everything in between, and re-exports the pure scanners from `aiondb-pg-syntax`.

## cargo

```toml
[dependencies]
aiondb-pg-compat = { path = "../aiondb-pg-compat" }
```

## modules

| module | purpose |
|---|---|
| `advisory` | classify and parse `pg_advisory_*` calls. |
| `check_constraints` | parse inline CHECK constraints from `CREATE TABLE`. |
| `command` | `CompatCommand` baseline tied to `Statement::compat_tag`. |
| `compat_tag_matrix` | single source of truth for accepted compat tags and their guaranteed behaviour. |
| `cursor` | `DECLARE CURSOR`, `FETCH`, `MOVE`, `CLOSE`, and `WHERE CURRENT OF` rewrites. |
| `dispatch` | `PgCompatHooks` trait the engine implements to host the compat layer. |
| `disposition` | typed classification of statements handed to the compat layer. |
| `dml_validation` | AST analysis for the WITH-DML rule validator. |
| `error_catalog` | typed `CompatFailureKind` and stable mapping to `SqlState`. |
| `metrics` | per-`CompatCommand` counters and parse/bind/execute histograms. |
| `noop_validation` | reject unknown compat command tags before runtime dispatch. |
| `oidjoins` | detection and NOTICE extraction for the PG `oidjoins` regression probe. |
| `prepared` | PREPARE/EXECUTE helpers and type-name normalisation. |
| `privileges` | map between parser GRANT/REVOKE AST and catalog privilege descriptors. |
| `registries` | value types for engine-side compat registries (databases, role membership). |
| `rewrite` | pre-parse text rewrites and small post-parse fix-ups. |
| `roles` | parsers for `DROP OWNED BY`, `REASSIGN OWNED BY`, `DROP ROLE`. |
| `startup` | startup parameter whitelist and `options=...` tokenizer. |
| `state` | versioned compat state (`CompatMiscObjects`, `DomainDefs`, `CastDefs`, `RuleDefs`). |
| `statement_policy` | per-statement policy gates. |
| `type_tracking` | parsers for `CREATE TYPE`, `CREATE/ALTER/DROP DOMAIN`. |

The pure scanners and parsers from `aiondb-pg-syntax` are re-exported: `do_parsers`, `do_scan`, `parsed_commands`, `prepare`, `preparse`, `rule_parsers`, `scan`, `type_ref`.

## key types

| item | description |
|---|---|
| `PgCompatHooks` trait | engine contract used by compat dispatch (in `dispatch`). |
| `CompatCommand` | active typed compat-command baseline. |
| `CompatFailureKind` | typed compat error categories with stable `SqlState` mapping. |
| `CompatObjectFamily`, `CompatObjectKey` | keys used by the persisted compat state. |
| `COMPAT_STATE_SCHEMA_VERSION` | schema version of the compat state. |
| `OIDJOINS_EXPECTED_OUTPUT` | snapshot of the PG `oidjoins` expected output, shared with `pg-regress`. |
| `WITH_DML_RULE_ERROR_PREFIX` | re-export tag prefix used by the WITH-DML rewrite. |

## example

```rust
use aiondb_pg_compat::OIDJOINS_EXPECTED_OUTPUT;
use aiondb_pg_compat::scan::trim_compat_statement;

assert!(!OIDJOINS_EXPECTED_OUTPUT.is_empty());
assert_eq!(trim_compat_statement("SELECT 1;"), "SELECT 1");
```
