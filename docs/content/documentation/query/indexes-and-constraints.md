---
title: Indexes and Constraints
order: 37
---

# Indexes and Constraints

Indexes and constraints are catalog objects used by planning, execution, and data validation.

Constraints express correctness. Indexes express access paths. They often overlap, but they should be evaluated separately: a query can be correct without the right index, and an index can improve speed without enforcing an invariant.

## Primary keys and uniqueness

```sql
CREATE TABLE users (
    id INT PRIMARY KEY,
    email TEXT UNIQUE,
    name TEXT NOT NULL
);
```

Use primary keys and unique constraints to express application invariants. In v0.1, validate conflict and error behavior with your client stack before relying on PostgreSQL-identical messages.

Conflict behavior should be part of driver testing:

```sql
INSERT INTO users VALUES (1, 'a@example.com', 'Ada');
INSERT INTO users VALUES (1, 'b@example.com', 'Grace');
```

Record SQLSTATE and transaction state after the failure. ORMs often depend on both.

## Foreign-key style modeling

Relational relationships can be expressed with ordinary columns and joins:

```sql
CREATE TABLE tickets (
    id INT PRIMARY KEY,
    assigned_to INT
);

CREATE TABLE employees (
    id INT PRIMARY KEY,
    name TEXT
);
```

Graph edge labels can then describe how relationship tables map into graph traversal.

For v0.1 graph evaluation, keep foreign-key-style relationships visible in SQL. That lets you compare graph traversal against an ordinary join.

Recommended relationship pattern:

```sql
CREATE TABLE ticket_assignments (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    assigned_at TEXT
);

CREATE INDEX ticket_assignments_source_idx ON ticket_assignments (source_id);
CREATE INDEX ticket_assignments_target_idx ON ticket_assignments (target_id);
```

## B-tree style indexes

```sql
CREATE INDEX tickets_assigned_to_idx ON tickets (assigned_to);
CREATE INDEX tickets_status_priority_idx ON tickets (status, priority);
```

Use ordinary indexes for selective filters and joins. Check plans when a query is unexpectedly slow.

Index columns that appear in:

- equality predicates;
- join predicates;
- graph edge endpoints;
- common sort keys;
- highly selective filters.

Composite indexes should follow the query shape. For `WHERE status = 'open' AND priority = 'high'`, an index on `(status, priority)` is more useful than two unrelated indexes when that predicate is common.

## HNSW vector indexes

```sql
CREATE TABLE docs (
    id INT,
    embedding VECTOR(4)
);

CREATE INDEX docs_embedding_hnsw ON docs USING hnsw (embedding);
```

Vector columns require `USING hnsw` for vector indexing. Query metric and index metric must match for the optimizer to use an HNSW path.

Keep an exact brute-force vector query as a correctness reference before judging indexed vector behavior.

## Dropping indexes

```sql
DROP INDEX docs_embedding_hnsw;
```

Dropping an index changes future planning. Re-run representative queries after index changes.

## Constraint checklist

For every important table, decide:

- which column identifies a row;
- which values must be unique;
- which columns may not be null;
- which relationships are represented by ordinary columns;
- which constraints your application currently enforces outside the database.

Document anything left to application code. That prevents a future reader from assuming the database enforces an invariant that is only implicit.

## Index checklist

For every important query, record:

- filter columns;
- join columns;
- sort columns;
- expected result size;
- whether the query touches graph endpoints or vector columns;
- indexes expected to help.

If a benchmark depends on an index, include the index DDL with the benchmark output.
