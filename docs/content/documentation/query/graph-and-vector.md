---
title: Graph and Vector
order: 40
---

# Graph and Vector

Graph and vector features run in the same engine as SQL. Hybrid queries do not need a separate graph or vector store for application state.

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

Labels describe tables. They do not create a separate graph store. A row is still a row. A node label says rows from a table can be addressed as graph nodes. An edge label says rows from another table can be read as relationships.

SQL stays as the fallback and correctness reference. If a graph query is unclear, the equivalent SQL join should still work.

## Edge labels on existing relational columns

Edge labels are moving toward pointing at existing foreign-key-style columns instead of requiring duplicate edge tables. The intended model:

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

The relational column stays the source of truth. Graph traversal works against it directly.

Duplicate edge tables create application friction. If `tickets.assigned_to` is already the relationship, forcing a `ticket_employee_edges` table introduces write duplication, triggers, or eventual inconsistency.

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

Vector functions and indexes handle embeddings stored beside application records. A typical workload starts from a vector predicate or nearest-neighbor ordering, then joins or traverses to related records.

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

Filtering can change the best plan. The engine may filter first and score fewer vectors, or use a vector index first and filter the candidate set afterward.

## Hybrid queries

A single plan should combine selective SQL predicates, graph traversal, vector similarity, and ordinary joins/projections. Validate hybrid graph/vector support on your workload and read the plans when comparing behavior.

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
