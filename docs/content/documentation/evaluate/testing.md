---
title: Testing
order: 84
---

# Testing

AionDB includes several test and validation layers. This page is for users and contributors who want to understand the public testing story without reading crate internals.

## Rust tests

Run the workspace tests with Cargo when you need broad validation:

```bash
cargo test
```

For faster development, run the crate or package you touched.

Use focused tests while developing, then run broader tests before publishing changes. Parser, planner, executor, catalog, pgwire, and storage changes can affect each other even when the edit looks local.

## SQL fixtures

SQL fixtures live under `testing/sql/`. They cover areas such as:

- graph labels;
- vector columns and distance functions;
- transactions;
- protocol behavior;
- security;
- recovery;
- concurrency.

Small reproducible bugs should become SQL fixtures where possible.

## What a good fixture looks like

A good fixture is small enough to read in one screen:

```sql
CREATE TABLE t (id INT PRIMARY KEY, value TEXT);
INSERT INTO t VALUES (1, 'a'), (2, 'b');
SELECT value FROM t WHERE id = 2;
```

It should make the expected behavior obvious. If the fixture needs a large dataset, generate that dataset deterministically and document the seed or command.

## Compatibility suites

The repository includes compatibility-oriented tests for PostgreSQL-facing behavior and graph query behavior. Use these when changing parser, planner, executor, pgwire, or catalog behavior.

Compatibility tests should answer one of three questions:

- does AionDB accept the same client flow?
- does it return the same rows or error class?
- does it fail explicitly when unsupported?

Do not treat a passing connection test as full compatibility. Real applications usually depend on prepared statements, catalog queries, migrations, transaction recovery, and type mapping.

## Benchmarks are not tests

Benchmarks can catch performance regressions, but they do not replace correctness tests. A benchmark result is only useful after the query result is known to be correct.

Before recording a benchmark number, run a correctness query that proves both engines are operating on the same dataset. A faster wrong result is not a performance win.

## New test guidance

- Keep the setup small.
- State expected output or expected error.
- Avoid random behavior unless the seed is fixed.
- Prefer one behavioral point per fixture.
- Include the SQL text that failed in the bug report.

## Areas that deserve extra tests

Add coverage when touching:

- transaction error state and rollback;
- catalog DDL followed by immediate query execution;
- graph label creation and traversal;
- vector distance functions and index selection;
- pgwire startup, bind, execute, sync, and error paths;
- recovery after process termination;
- authentication and authorization checks.

These areas are where product features meet infrastructure. Regressions there are more damaging than isolated parser mistakes.
