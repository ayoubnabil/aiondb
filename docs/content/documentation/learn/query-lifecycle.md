---
title: Query Lifecycle
order: 26
---

# Query Lifecycle

This page describes how a query moves through AionDB at a product level.

## 1. Client entry

A query enters through one of two public surfaces:

- PostgreSQL wire server.
- Embedded Rust API.

The goal is for both surfaces to use the same engine behavior.

For pgwire clients, the request may arrive through simple query flow or extended protocol flow. Extended protocol clients can split work into parse, bind, execute, and sync messages. Embedded callers enter through Rust APIs, but should still reach the same semantic engine path.

## 2. Parse

SQL or graph syntax is parsed into an internal statement representation. Unsupported syntax should fail early with a clear error.

Parser errors are syntax errors. They should not depend on table contents or runtime data. If a query fails here, reduce the input to the smallest statement that still fails.

## 3. Bind and type check

Names are resolved against the catalog. Expression types are checked, parameters are assigned types when possible, and invalid references are rejected.

Binding is where `SELECT missing_column FROM t` should fail, where ambiguous names should be rejected, and where parameter or expression types become concrete enough for planning. DDL changes affect this stage because the catalog is the source of truth.

## 4. Logical planning

The planner builds a logical plan: scans, filters, joins, projections, aggregates, graph operations, and vector expressions.

A logical plan describes what must happen without committing to every physical detail. For example, a query may require a filter and a join, but the optimizer can still decide which table to access first or whether an index is useful.

## 5. Optimization

The optimizer chooses physical shapes where implemented. Examples include ordinary index access, join ordering, predicate pushdown, graph-specific planning, and vector index paths.

This is the stage to inspect when a query returns correct rows but is slower than expected. For hybrid workloads, the optimizer eventually needs to reason about SQL selectivity, graph degree, and vector candidate counts in one plan.

## 6. Execution

The executor runs the physical plan against catalog, transaction, storage, and WAL components. Result rows or command tags are returned to the caller.

Execution bugs usually appear as wrong rows, missing rows, incorrect command tags, transaction state problems, or storage inconsistencies. These bugs need small reproducible datasets, not only the failing query.

## 7. Client response

Pgwire clients receive PostgreSQL-style responses such as row descriptions, data rows, command tags, errors, and SQLSTATE codes. Embedded callers receive structured Rust results.

Client response shape matters because drivers often depend on details beyond rows. A driver may care about column metadata, command completion tags, SQLSTATE values, and whether the connection remains usable after an error.

## Example diagnosis

If this query fails:

```sql
SELECT d.title
FROM docs d
JOIN doc_mentions dm ON dm.source_id = d.id
WHERE dm.target_id = $1;
```

Classify the failure:

- syntax error near `JOIN`: parser issue;
- unknown table or column: binder/catalog issue;
- unsupported parameter type: bind/type issue;
- correct rows but slow: planning or optimizer issue;
- wrong rows: executor/storage issue;
- driver disconnect: pgwire response or protocol issue.

## Alpha guidance

When behavior is surprising, reduce it to one stage:

- parse error means syntax support;
- bind error means name, type, or catalog resolution;
- plan mismatch means optimizer or physical planning;
- wrong rows means executor or storage behavior;
- driver mismatch means pgwire or protocol compatibility.
