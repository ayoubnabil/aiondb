---
title: SQL Statements
order: 32
---

# SQL Statements

This page summarizes the user-facing SQL statement families available in v0.1. PostgreSQL compatibility is implemented incrementally, so use this as a guide rather than a complete SQL grammar.

## Query statements

```sql
SELECT id, title
FROM docs
WHERE kind = 'runbook'
ORDER BY id
LIMIT 10;
```

Supported query features include ordinary projections, filters, ordering, limits, joins, aggregates, and selected window functions. Complex PostgreSQL syntax should be tested against your workload.

## Data definition

```sql
CREATE TABLE items (
    id INT PRIMARY KEY,
    name TEXT NOT NULL,
    embedding VECTOR(3)
);

CREATE INDEX items_name_idx ON items (name);
CREATE INDEX items_embedding_idx ON items USING hnsw (embedding);

DROP INDEX items_name_idx;
DROP TABLE items;
```

Common DDL paths include tables, indexes, views, sequences, node labels, and edge labels. Some `ALTER TABLE` paths are implemented, but alpha users should validate the exact operation they need.

## Data manipulation

```sql
INSERT INTO items VALUES (1, 'alpha', '[1.0,0.0,0.0]');
UPDATE items SET name = 'beta' WHERE id = 1;
DELETE FROM items WHERE id = 1;
```

`RETURNING`, defaults, `ON CONFLICT`, and COPY-related paths exist in the codebase, but compatibility details should be tested before relying on generated SQL from an ORM.

## Transactions

```sql
BEGIN;
SAVEPOINT before_change;
ROLLBACK TO SAVEPOINT before_change;
RELEASE SAVEPOINT before_change;
COMMIT;
```

See [Transactions](/documentation/query/transactions.html) for guidance.

## Roles and privileges

```sql
CREATE ROLE reader LOGIN;
GRANT SELECT ON items TO reader;
REVOKE SELECT ON items FROM reader;
DROP ROLE reader;
```

Roles, grants, revokes, and superuser-style paths are part of the security model. The v0.1 docs still recommend local evaluation rather than exposed production use.

## Graph DDL

```sql
CREATE NODE LABEL person ON persons;
CREATE EDGE LABEL friends ON friends SOURCE person TARGET person;

DROP EDGE LABEL friends;
DROP NODE LABEL person;
```

See [Graph Reference](/documentation/query/graph-reference.html).

## Explain

Use `EXPLAIN` and `EXPLAIN ANALYZE` where supported to inspect execution shape during performance work.

## Statement coverage map

The parser accepts a large surface area for PostgreSQL compatibility. Acceptance by the parser does not imply full execution semantics — for compatibility-only families the engine validates the AST and returns the matching command tag, but downstream behaviour is best-effort. Treat the table below as parser coverage; binder and executor support varies and must be validated against the workload.

| Family | Forms |
| --- | --- |
| Query | `SELECT`, set operations (`UNION` / `INTERSECT` / `EXCEPT`), `EXPLAIN [ANALYZE]` |
| DML | `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `COPY` |
| Schema | `CREATE TABLE`, `CREATE TABLE AS`, `CREATE INDEX`, `CREATE SEQUENCE`, `CREATE VIEW`, `CREATE SCHEMA`, `TRUNCATE`, the matching `DROP` and `ALTER` forms |
| Graph | `CREATE NODE LABEL`, `CREATE EDGE LABEL`, `DROP NODE LABEL`, `DROP EDGE LABEL`, Cypher statements |
| Roles & privileges | `CREATE ROLE`, `ALTER ROLE`, `ALTER ROLE … RENAME TO …`, `DROP ROLE`, `GRANT`, `REVOKE`, `DROP OWNED BY`, `REASSIGN OWNED BY` |
| Routines | `CREATE FUNCTION`, `DROP FUNCTION`, `CREATE PROCEDURE`, `DROP PROCEDURE`, `DROP ROUTINE`, `CREATE AGGREGATE`, `DROP AGGREGATE`, `CREATE OPERATOR`, `DROP OPERATOR` |
| Triggers & rules | `CREATE TRIGGER`, `ALTER TRIGGER … RENAME TO …`, `DROP TRIGGER`, `CREATE RULE`, `ALTER RULE`, `DROP RULE` |
| Types & domains | `CREATE TYPE`, `ALTER TYPE`, `DROP TYPE`, `CREATE DOMAIN`, `ALTER DOMAIN`, `DROP DOMAIN`, `CREATE CAST`, `DROP CAST` |
| Transactions | `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, `RELEASE SAVEPOINT`, `PREPARE TRANSACTION`, `COMMIT PREPARED`, `ROLLBACK PREPARED` |
| Locking | `LOCK [TABLE]`, advisory lock helpers |
| Async | `LISTEN`, `UNLISTEN`, `NOTIFY` |
| Cursors | `DECLARE`, `FETCH`, `MOVE`, `CLOSE` (placeholders dispatched to the compat layer) |
| PL/pgSQL | `DO` anonymous blocks, function bodies for `LANGUAGE plpgsql` |
| Database & tenancy | `CREATE DATABASE`, `ALTER DATABASE`, `DROP DATABASE`, `CREATE TENANT`, `DROP TENANT`, `SET TENANT` |
| Replication & publications | `CREATE PUBLICATION`, `ALTER PUBLICATION`, `DROP PUBLICATION`, `CREATE SUBSCRIPTION`, `ALTER SUBSCRIPTION`, `DROP SUBSCRIPTION` |
| Foreign data | `CREATE SERVER`, `ALTER SERVER`, `DROP SERVER`, `CREATE USER MAPPING`, `ALTER USER MAPPING`, `DROP USER MAPPING`, `CREATE FOREIGN TABLE`, `ALTER FOREIGN TABLE`, `DROP FOREIGN TABLE`, `CREATE FOREIGN DATA WRAPPER`, `ALTER FOREIGN DATA WRAPPER`, `DROP FOREIGN DATA WRAPPER` |
| Statistics & policies | `CREATE STATISTICS`, `ALTER STATISTICS`, `DROP STATISTICS`, `CREATE POLICY`, `ALTER POLICY`, `DROP POLICY` |
| Extensions & locale | `CREATE EXTENSION`, `DROP EXTENSION`, `CREATE COLLATION`, `ALTER COLLATION`, `DROP COLLATION`, `LOAD` (parsed and reported as completed, no dynamic loading) |
| Maintenance | `ANALYZE`, `VACUUM`, `CHECKPOINT`, `BACKUP`, `RESTORE`, `DISCARD { ALL \| TEMP \| PLANS \| SEQUENCES }` |
| Comments & labels | `COMMENT ON … IS …`, `SECURITY LABEL …` |
| Settings | `SET`, `SHOW`, `RESET`, `ALTER SYSTEM` |
| Tablespaces | `CREATE TABLESPACE`, `ALTER TABLESPACE`, `DROP TABLESPACE` |

When a statement is in the parser surface but a planner or executor path is missing, the engine returns the matching `0A000` (feature not supported) or `42704` (undefined object) SQLSTATE rather than silently succeeding.
