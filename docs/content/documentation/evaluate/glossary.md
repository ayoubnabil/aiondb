---
title: Glossary
order: 94
---

# Glossary

## Catalog

The metadata store for tables, columns, indexes, graph labels, edge labels, and related database objects.

The catalog is the source of truth used by binding and planning. If the catalog says a table, label, or index does not exist, the query should fail before execution.

## Binder

The query stage that resolves names and types against the catalog. It turns unresolved syntax into a semantically checked statement.

## Catalog introspection

Queries that inspect metadata instead of application rows. ORMs use catalog introspection to discover tables, columns, indexes, constraints, and types.

## Command tag

The PostgreSQL-style completion string returned after statements such as `INSERT`, `UPDATE`, `DELETE`, or `CREATE TABLE`. Some drivers and tools inspect command tags.

## Durable mode

Server mode that writes persistent state under a data directory. In v0.1 this is for local evaluation unless the workload owner validates failure behavior independently.

## Edge label

A graph catalog object that maps rows in a backing table to relationships between node labels.

The default edge table convention uses `source_id` and `target_id` endpoint columns.

## Embedded mode

Using AionDB directly inside a Rust process instead of connecting over the network.

Embedded mode is useful for local applications and tests, but server mode is still required when validating pgwire drivers.

## Extended protocol

The PostgreSQL wire flow where a client sends parse, bind, execute, and sync messages instead of one simple query string. Many drivers use this path for prepared statements.

## Graph label

A catalog label that lets rows in a relational table participate in graph queries.

## Hybrid query

A query that combines more than one data access style, such as SQL filters plus graph relationships plus vector distance scoring.

## HNSW

Hierarchical Navigable Small World index. A common approximate nearest-neighbor index used for vector search.

## Node label

A graph catalog object that maps rows from a backing table to graph nodes.

## Pgwire

The PostgreSQL frontend/backend wire protocol used by `psql` and PostgreSQL drivers.

## Prepared statement

A query prepared once and executed later, often with parameters. Many PostgreSQL drivers use prepared statements or extended protocol automatically.

## SQLSTATE

A five-character PostgreSQL error code. Applications should prefer SQLSTATE over parsing human-readable error messages.

## Simple query protocol

The PostgreSQL wire flow where a client sends one query string and receives responses. It is easier to test than extended protocol but does not cover all driver behavior.

## Storage backend

The implementation selected to hold database state. v0.1 exposes in-memory and durable local evaluation paths, with additional backend names available for focused testing.

## Transaction state

The session state after `BEGIN`, `COMMIT`, `ROLLBACK`, or an error inside a transaction. Drivers often depend on PostgreSQL-style transaction state behavior.

## WAL

Write-ahead log. The durability log used to recover committed changes after restart or crash.

## Vector

A fixed-dimension numeric embedding stored in a `VECTOR(N)` column and queried with distance functions such as `l2_distance` and `cosine_distance`.

## Vector index

An index used to accelerate vector similarity search. HNSW is the current approximate-nearest-neighbor index shape documented for evaluation.
