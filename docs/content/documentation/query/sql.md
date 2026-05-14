---
title: SQL
order: 30
---

# SQL

SQL is the primary language surface in AionDB. The project implements its own parser, binder, planner, optimizer, executor, catalog, storage, and WAL path.

## Basic DDL and DML

The common path is ordinary relational SQL:

```sql
CREATE TABLE employees (
    id INT PRIMARY KEY,
    name TEXT,
    team TEXT
);

INSERT INTO employees VALUES
    (1, 'Ada', 'storage'),
    (2, 'Grace', 'query');

SELECT id, name
FROM employees
WHERE team = 'query';
```

This is the surface to validate first. If simple table creation, inserts, predicates, projections, and joins do not work for your application shape, graph and vector features should wait until the relational baseline is fixed.

## Query shape

AionDB's SQL path is intended to cover the common shape of application queries:

- create tables and indexes;
- insert, update, delete, and select rows;
- filter with scalar expressions;
- join related tables;
- aggregate where supported;
- execute inside transactions;
- expose results over pgwire.

Unsupported syntax should return a clear error. Treat silent fallback or surprising PostgreSQL differences as bugs to reduce and report.

## Joins

Use SQL joins as the correctness baseline for graph-like data:

```sql
CREATE TABLE tickets (
    id INT PRIMARY KEY,
    title TEXT,
    assigned_to INT
);

CREATE TABLE employees (
    id INT PRIMARY KEY,
    name TEXT
);

SELECT t.id, t.title, e.name
FROM tickets t
JOIN employees e ON e.id = t.assigned_to
WHERE e.name = 'Ada';
```

If a graph pattern is supported for the same relationship, compare it against the SQL join result. This keeps graph evaluation grounded in table semantics.

## Transactions

AionDB supports explicit transaction statements on the implemented engine path:

```sql
BEGIN;
INSERT INTO employees VALUES (3, 'Linus', 'kernel');
COMMIT;
```

Use [Limitations](/documentation/evaluate/limitations.html) as the public contract before relying on behavior that is not covered by tests or examples.

## PostgreSQL compatibility

Compatibility is a goal, not a blanket claim. The server accepts PostgreSQL wire clients, and parts of PostgreSQL syntax and behavior are implemented. PostgreSQL has a very large surface area, including extensions, catalog behavior, protocol details, COPY modes, functions, types, and planner behavior. AionDB v0.1 should be evaluated against the exact workload you care about.

See [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html) for the compatibility contract.

For statement-level examples, see [SQL Statements](/documentation/query/sql-statements.html). For scalar functions, see [Functions](/documentation/query/functions.html).

## Parameterized queries

Most real clients use parameters, even when a human-written `psql` query does not. Validate parameter behavior with your driver:

```sql
SELECT id, name
FROM employees
WHERE team = $1;
```

Parameter typing, prepared statements, and binary/text formats are common places where driver behavior differs from a simple SQL console.

## Error behavior

Unsupported features should return explicit errors. Treat silent compatibility as a bug. If a query appears to succeed but behaves differently from PostgreSQL, reduce it to a small repro and add it to the compatibility suite.

## Practical evaluation script

For a first SQL compatibility check, keep one script that does all of this:

```sql
CREATE TABLE departments (id INT PRIMARY KEY, name TEXT);
CREATE TABLE employees (id INT PRIMARY KEY, name TEXT, department_id INT);

INSERT INTO departments VALUES (1, 'query'), (2, 'storage');
INSERT INTO employees VALUES (1, 'Ada', 1), (2, 'Grace', 1), (3, 'Linus', 2);

SELECT e.name, d.name AS department
FROM employees e
JOIN departments d ON d.id = e.department_id
WHERE d.name = 'query'
ORDER BY e.id;
```

Then run the same script through `psql`, through the driver you care about, and through any ORM layer. Differences between those three paths are useful compatibility data.
