---
title: Migration Guide
order: 81
---

# Migration Guide

AionDB v0.1 is not a drop-in production migration target. Use this guide for evaluation migrations: taking a small schema or workload and seeing how much runs.

## Start with schema only

Begin with a reduced schema:

```sql
CREATE TABLE users (
    id INT PRIMARY KEY,
    name TEXT NOT NULL
);
```

Avoid importing every extension, trigger, function, index type, and migration artifact at once. Add them in layers.

A good first migration target is one schema slice: a few related tables, their key indexes, and the queries that matter. The goal is to find compatibility gaps early, not to move the whole application in one step.

## Replace unsupported extensions

PostgreSQL extensions are not automatically available. For evaluation:

- replace extension-backed UUID or crypto behavior with explicit application-side values;
- replace unsupported text search features with simpler predicates;
- test JSON and array operators before using them broadly;
- test procedural code separately.

Keep a list of replacements. For each unsupported feature, decide whether it is:

- unnecessary for the evaluation;
- replaceable in application code;
- blocked until AionDB supports it;
- better left on PostgreSQL.

## Load data from SQL fixtures

For v0.1, prefer reproducible SQL fixtures:

```sql
INSERT INTO users VALUES (1, 'Ada'), (2, 'Grace');
```

Only move to large imports after the schema and queries are known to work.

For larger data, import a representative subset first. Include edge cases: nulls, long text, duplicate-like values, high-degree relationships, empty vectors if allowed by your model, and rows that should violate constraints.

## Validate generated SQL

ORMs often generate catalog queries, type casts, `RETURNING`, savepoints, and migration DDL. Run the generated SQL directly and reduce failures.

If an ORM migration fails, separate it into:

- connection issue;
- catalog introspection issue;
- unsupported DDL;
- unsupported type;
- unsupported transaction/savepoint behavior;
- query result mismatch.

## Compare behavior

For each workload:

1. Run on PostgreSQL.
2. Save expected output.
3. Run on AionDB.
4. Compare rows, errors, SQLSTATE, and transaction behavior.
5. Add a reduced repro for every mismatch.

## Migration scorecard

Track each feature as one of:

| Status | Meaning |
| --- | --- |
| Works | Runs with expected result. |
| Works with rewrite | Equivalent behavior exists with SQL or schema changes. |
| Blocked | No acceptable rewrite for the workload. |
| Not tested | Do not assume compatibility. |

This makes the evaluation honest and prevents a single successful demo from being mistaken for a full migration.

## Do not migrate production data to v0.1

Keep AionDB v0.1 migrations as evaluation work. The on-disk format and compatibility surface are still alpha.
