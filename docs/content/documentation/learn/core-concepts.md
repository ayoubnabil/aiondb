---
title: Core Concepts
order: 20
---

# Core Concepts

AionDB has one catalog and one SQL pipeline. Rows stay in tables. Graph traversal and vector search are extra access paths over the same rows, not separate stores. Application state is not duplicated into a graph database or a vector database next door.

## Mental model

AionDB starts from a relational database model:

- tables store rows;
- columns and constraints define shape;
- indexes accelerate lookup;
- transactions group changes;
- the catalog records database objects.

Graph labels and vector search are not separate storage products. They are catalog-level features over the same tables.

## Tables are the source of truth

Tables are the base storage abstraction. SQL DDL creates tables, columns, constraints, indexes, and other relational objects. Graph and vector features are layered on top of that same catalog instead of living in a separate database.

This matters for application design: the canonical row stays in one place. A ticket, document, customer, or employee should not need to be duplicated only so it can participate in a graph query or a similarity query.

## PostgreSQL wire, independent engine

AionDB speaks PostgreSQL wire protocol so existing tools can connect, but it is not a PostgreSQL fork. Compatibility is implemented feature by feature. Unsupported syntax should fail explicitly instead of pretending to work.

The consequence is that AionDB can feel familiar to PostgreSQL tools while still having different internals, different limits, and different performance characteristics. Treat compatibility as a measured surface, not a promise that every PostgreSQL feature exists.

## One engine, two ways to run it

The server and embedded API use the same engine path:

- server mode accepts network clients over pgwire;
- embedded mode runs in-process through a Rust API;
- tests and benchmarks exercise the same core behavior where possible.

Use server mode when you want normal database process boundaries. Use embedded mode when the application owns the process and wants local database behavior without a network hop.

## Graph labels

A node label maps rows from a table into a graph label:

```sql
CREATE NODE LABEL employee ON employees;
```

An edge label maps rows from an edge table into directed relationships:

```sql
CREATE EDGE LABEL reports_to ON employee_edges SOURCE employee TARGET employee;
```

In v0.1, the stable edge-table convention is `source_id` and `target_id`. Endpoint mapping over existing foreign-key columns is an architectural direction for reducing duplicate edge tables, but the public contract should be checked against the current [Graph Reference](/documentation/query/graph-reference.html).

## Vector columns

Vector columns store fixed-length numeric vectors:

```sql
CREATE TABLE docs (
    id INT PRIMARY KEY,
    title TEXT,
    embedding VECTOR(3)
);
```

Distance functions can rank rows directly from SQL. This keeps vector scoring close to the relational filters that decide which rows are eligible.

## Hybrid queries

Graph labels and vector operations are intended to live beside SQL. The practical target is hybrid application data: tickets assigned to employees, documents linked to entities, embeddings attached to records, and queries that need to mix those relationships.

For example, a knowledge-base workload can:

- filter documents with SQL predicates;
- traverse explicit document links;
- rank candidate documents by vector distance;
- return ordinary rows to a PostgreSQL client.

## Alpha contract

The v0.1 contract is intentionally narrow. AionDB is suitable for evaluation, but the on-disk format, unsupported SQL surface, graph/vector syntax, and operational tooling may still change.

The safest way to evaluate v0.1 is to keep the dataset reproducible, keep benchmark commands in version control, and avoid treating current disk files as a long-term migration target.
