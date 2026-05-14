---
title: aiondb-planner
order: 23
---

# aiondb-planner

Turns parser AST into a `LogicalPlan`. The planner binds names against the catalog, type-checks expressions, rewrites `pg_catalog` and `information_schema` references into virtual scans, and dispatches Cypher statements to a separate Cypher plan builder.

## cargo

```toml
[dependencies]
aiondb-planner = { path = "../aiondb-planner" }
```

## modules

| module | purpose |
|---|---|
| `binder` | Resolves identifiers against the catalog and produces `BoundStatement` variants. |
| `type_check` | Walks the bound AST, assigns `DataType`s to every expression, applies coercions. |
| `logical_builder` | Builds the actual `LogicalPlan` from typed/bound statements. |
| `pg_catalog` | Synthetic `pg_catalog` tables and the synthetic relation-id allocator. |
| `information_schema` | Synthetic `information_schema` tables. |

## key types

- `Planner` - top-level planner. Constructed with `Planner::new(catalog)`; entry point is `Planner::plan(PlanRequest)`.
- `PlanRequest<'a>` - input bundle: `statement`, `txn_id`, optional `default_schema`, `current_user`, `session_user`, `database_name`, `datestyle`, `timezone`.
- `StatementDescription` - output column metadata + parameter types for prepared statements.
- `ResultColumnOrigin` - tracks which catalog column a result column came from (for editable views and `RETURNING`).
- `Binder` and `BoundStatement` - name resolution layer. `BoundStatement` has one variant per supported statement kind (`Select`, `Insert`, `Update`, `Delete`, `Merge`, `Copy`, every DDL form, `Lock`, `Discard`, `CypherQuery`, ...).
- `LogicalBuilder` - builds the `LogicalPlan` from typed bound statements.
- `TypeChecker` - assigns types and nullability across the AST.

## entry points

| item | role |
|---|---|
| `Planner::new(catalog)` | Build a planner backed by a `CatalogReader`. |
| `Planner::plan(req)` | Bind, type-check and lower a `Statement` to `LogicalPlan`. |
| `type_check_expression`, `type_check_expression_with_relation`, `type_check_expression_with_relation_and_session_context` | Standalone expression type-checking helpers, used by other crates that need to type a single expression outside of a full statement. |
| `is_virtual_synthetic_relation(id)` | Returns `true` when `id` denotes a synthetic `pg_catalog` or `information_schema` relation. Engines use this to distinguish "no physical storage by design" from "real storage failure". |

## example

```rust
use std::sync::Arc;
use aiondb_catalog::CatalogReader;
use aiondb_parser::parse_sql;
use aiondb_planner::{PlanRequest, Planner};
use aiondb_core::TxnId;

fn plan_first(catalog: Arc<dyn CatalogReader>, sql: &str, txn: TxnId) {
    let stmts = parse_sql(sql).expect("parse");
    let planner = Planner::new(catalog);
    let req = PlanRequest {
        statement: &stmts[0],
        txn_id: txn,
        default_schema: Some("public".to_string()),
        current_user: Some("alice".to_string()),
        session_user: Some("alice".to_string()),
        database_name: Some("aiondb".to_string()),
        datestyle: None,
        timezone: None,
    };
    let logical = planner.plan(req).expect("plan");
    let _ = logical;
}
```
