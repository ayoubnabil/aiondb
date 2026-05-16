---
title: Graph and Vector
order: 40
---

# Graph and Vector

AionDB includes graph and vector features in the same engine as SQL. The goal is to make hybrid queries possible without duplicating application state into a separate graph or vector store.

## Graph model

Graph labels are catalog objects that describe how existing tables participate in graph queries.

Typical shape:

```sql
CREATE TABLE people (
    id INT PRIMARY KEY,
    name TEXT
);

CREATE TABLE knows (
    source_id INT,
    target_id INT
);

CREATE NODE LABEL Person ON people;
CREATE EDGE LABEL knows ON knows SOURCE Person TARGET Person;
```

The current alpha model is explicit. If a relationship is stored in a backing table, the graph label tells AionDB how to read that relationship. The storage layer maintains adjacency indexes for edge labels so traversal and graph procedures do not have to fall back to full edge-table scans in the common path.

## Why graph labels are catalog objects

The important design choice is that labels describe tables instead of creating a separate graph store. A row remains a row. A node label says that rows from a table can be addressed as graph nodes. An edge label says that rows from another table can be interpreted as relationships.

That keeps SQL as the fallback and correctness reference. If a graph query is unclear, the equivalent SQL join should still be possible.

## Edge labels on existing relational columns

AionDB is moving toward edge labels that can point at existing foreign-key-style columns instead of requiring duplicate edge tables. The intended model is:

```sql
CREATE TABLE tickets (
    id INT PRIMARY KEY,
    assigned_to INT
);

CREATE NODE LABEL Ticket ON tickets;
CREATE NODE LABEL Employee ON employees;

CREATE EDGE LABEL handled_by ON tickets
    SOURCE Ticket KEY (id)
    TARGET Employee KEY (assigned_to);
```

That design keeps the relational column as the source of truth while still enabling graph traversal.

This matters because duplicate edge tables create application friction. If `tickets.assigned_to` is already the true relationship, forcing the application to also maintain a `ticket_employee_edges` table introduces write duplication, triggers, or eventual inconsistency.

Check the graph reference and parser support before relying on endpoint mapping syntax in a release. The architectural direction is clear, but public syntax should match the current binary.

## Graph execution surface

The graph executor supports:

- Cypher-style `MATCH` over node and edge labels;
- bounded variable-length relationships;
- `shortestPath` and `allShortestPaths` over one typed relationship pattern;
- named path rendering for ordinary paths, `shortestPath`, and `allShortestPaths`;
- `CALL graph.*` algorithm procedures over the current graph projection;
- persisted graph projection cache reuse with rebuild on stale or invalid cache data;
- `EXPLAIN` graph-access lines that show whether the plan is using row-store fallback, traversal-store adjacency, or projection data.

The remaining alpha boundaries are mostly compatibility and evidence work: broader Cypher coverage, SQL/PGQ naming, graph-specific operational metrics, and reproducible Neo4j-class benchmark reports.

## Vector data

Vector functions and indexes are intended for embeddings stored beside application records. A typical workload starts from a vector predicate or nearest-neighbor ordering, then joins or traverses to related records.

Example shape:

```sql
SELECT id, title
FROM documents
ORDER BY l2_distance(embedding, '[1.0,0.0,0.0]')
LIMIT 10;
```

For indexed vector search, create an HNSW index on a `VECTOR(N)` column:

```sql
CREATE TABLE embeddings (
    id INT NOT NULL,
    doc_name TEXT,
    vec VECTOR(4)
);

CREATE INDEX embeddings_vec_idx ON embeddings USING hnsw (vec);
```

Exact operators, index behavior, and planner support are still part of the alpha surface. Benchmark your own workload before assuming vector recall, latency, or filtered search behavior.

## Filtering and vector ranking

A realistic query usually needs more than nearest neighbors:

```sql
SELECT id, title, l2_distance(embedding, '[1.0,0.0,0.0]') AS dist
FROM documents
WHERE workspace_id = 42
ORDER BY dist ASC
LIMIT 10;
```

This shape is important because filtering can change the best plan. The engine may need to decide between filtering first and scoring fewer vectors, or using a vector index first and filtering the candidate set afterward.

## Hybrid queries

The target is a single plan that can combine:

- selective SQL predicates;
- graph traversal;
- vector similarity;
- ordinary joins and projections.

Validate hybrid graph/vector support on the exact workload you care about and read query plans when comparing behavior.

## Correctness strategy

For every hybrid query, keep a simpler reference query:

- SQL joins for relationship correctness;
- brute-force vector distance for nearest-neighbor correctness;
- small datasets where expected rows can be inspected manually;
- explicit ordering when comparing result sets.

This prevents optimizer experiments from hiding semantic bugs.

## Performance strategy

Hybrid performance depends on data distribution. Record:

- number of rows in each table;
- number of edges per label;
- vector dimension;
- index definitions;
- selectivity of SQL filters;
- query text and limit;
- raw output and latency.

Without that context, graph/vector benchmark numbers are not meaningful.

Detailed syntax examples are split into [Graph Reference](/documentation/query/graph-reference.html) and [Vector Reference](/documentation/query/vector-reference.html).
