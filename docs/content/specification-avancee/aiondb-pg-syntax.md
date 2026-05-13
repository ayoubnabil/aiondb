---
title: aiondb-pg-syntax
order: 44
---

# aiondb-pg-syntax

Pure SQL scanning and parsing helpers for PostgreSQL-compatible syntax. Every function here operates on `&str` (or parser tokens) and is free of engine, catalog, and storage coupling. The compatibility logic that builds on top lives in `aiondb-pg-compat`, which re-exports each module from here.

## cargo

```toml
[dependencies]
aiondb-pg-syntax = { path = "../aiondb-pg-syntax" }
```

## modules

| module | purpose |
|---|---|
| `scan` | low-level scanning primitives (`trim_compat_statement`, `skip_sql_whitespace`, `consume_word_ci`, `parse_compat_identifier`, `parse_compat_uint`, `parse_compat_bool`, ...). |
| `do_scan` | scanning primitives for DO blocks and PL/pgSQL: dollar-quote handling and top-level keyword search. |
| `do_parsers` | parsers for DO-block content (statement splitting, `SELECT INTO` targets, array-assign rewrite, record-field validation). |
| `parsed_commands` | parsers for PG-compatible DDL the native parser does not yet handle (`ALTER INDEX`, `ALTER VIEW`, `ALTER TYPE ATTRIBUTE`, `CREATE`/`DROP CAST`, ...). |
| `prepare` | parsers for `PREPARE`, `EXECUTE`, and `DEALLOCATE`. |
| `preparse` | byte-level extractors run before the parser (dollar-quoted literals, `CURRENT OF`, `CREATE SCHEMA AUTHORIZATION`). |
| `rule_parsers` | parsers and SQL reconstructors for `CREATE RULE` and `DROP RULE`. |
| `type_ref` | parsers for PG-compatible type references in routine and cast signatures. |

## key types

| item | description |
|---|---|
| `WITH_DML_RULE_ERROR_PREFIX` | tag prefix used by the WITH-DML rewrite to flag rewritten errors. |
| `ParsedCompatTypeRef` | parsed type reference from a routine or cast signature. |
| `ParsedCompatCast`, `ParsedCompatCastMethod`, `ParsedCompatDropCast` | parsed `CREATE CAST` / `DROP CAST` records. |
| `ParsedCompatDropTypeOrDomain`, `ParsedCompatObjectName`, `ParsedCompatAlterRoleRename` | parsed records for various `DROP` and `ALTER ROLE ... RENAME` variants. |
| `ParsedCompatDropRoutine`, `CompatDropRoutineKind` | parsed `DROP FUNCTION`/`PROCEDURE`/`ROUTINE`. |
| `CompatDeallocateTarget` | parsed `DEALLOCATE` target (named or `ALL`). |

## example

```rust
use aiondb_pg_syntax::scan::{parse_compat_identifier, trim_compat_statement};

assert_eq!(trim_compat_statement("SELECT 1;"), "SELECT 1");

let mut cursor = 0;
let ident = parse_compat_identifier("my_table rest", &mut cursor);
assert_eq!(ident.as_deref(), Some("my_table"));
```
