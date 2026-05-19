---
title: Graph Reference
order: 42
---

# Graph Reference

Graph support maps relational tables into graph labels and relationships.

## Node labels

```sql
CREATE TABLE persons (
    id INT NOT NULL,
    name TEXT,
    age INT
);

CREATE NODE LABEL person ON persons;
```

A node label gives a table a graph identity. Rows remain ordinary table rows.

Use one node label when a table has one graph role. If a future model supports multiple labels or conditional labels, document the predicate and update behavior explicitly. For alpha releases, keep modeling simple enough that the equivalent SQL is obvious.

## Edge labels

```sql
CREATE TABLE friends (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    since INT
);

CREATE EDGE LABEL friends ON friends SOURCE person TARGET person;
```

The default edge table shape uses `source_id` and `target_id`. Additional edge properties can live in the same backing table.

Endpoint columns should be indexed for frequent traversals:

```sql
CREATE INDEX friends_source_idx ON friends (source_id);
CREATE INDEX friends_target_idx ON friends (target_id);
```

For directed relationships, `source_id` is the outgoing endpoint and `target_id` is the incoming endpoint. For undirected semantics, model both directions explicitly or query both directions intentionally.

## Querying graph data with SQL

Because graph data is backed by tables, SQL joins remain the most explicit query form:

```sql
SELECT p2.name AS friend
FROM persons p1
JOIN friends f ON f.source_id = p1.id
JOIN persons p2 ON p2.id = f.target_id
WHERE p1.name = 'Alice';
```

This query is the reference form for the graph pattern below. When validating graph support, run both versions on the same dataset.

## Cypher-style query shape

Graph patterns are represented with Cypher-style syntax where supported:

```sql
MATCH (p:person)-[:friends]->(f:person)
RETURN p.name, f.name
LIMIT 10;
```

Supported graph syntax is evolving. If a graph query does not work, rewrite it as SQL joins and keep the reduced graph repro for compatibility work.

## Path queries

AionDB supports bounded variable-length path shapes and shortest-path functions for the Cypher subset used by the executor:

```sql
MATCH p = shortestPath((a:person {id: 1})-[:friends*..5]->(b:person {id: 9}))
RETURN p;

MATCH p = allShortestPaths((a:person {id: 1})-[:friends*..5]->(b:person {id: 9}))
RETURN p;
```

Named paths render as alternating node and relationship values. `shortestPath` returns one path; `allShortestPaths` returns every shortest variant up to the configured result and memory limits.

`shortestPath` also supports multi-segment typed patterns, including combinations of fixed segments with at most one bounded variable-length segment. `allShortestPaths` remains limited to a single typed relationship segment.

Current limits:

- shortest-path functions require exactly two node patterns and one typed relationship pattern;
- path search should be bounded with an explicit maximum hop count for serious workloads;
- named paths can span fixed segments plus one bounded variable-length segment; patterns with more than one variable-length relationship are still not supported;
- deep traversals are subject to query deadline, result-row, workset, and memory limits.

## Graph algorithm procedures

Graph algorithms are exposed through `CALL graph.*` procedures over the current graph projection:

```sql
CALL graph.pageRank()
YIELD nodeId, score
RETURN nodeId, score
ORDER BY score DESC
LIMIT 10;

CALL graph.dijkstra(1, 9, 8, 'weight')
YIELD path, cost
RETURN path, cost;
```

The procedure registry includes traversal, shortest path, centrality, community, similarity, link-prediction, embedding, and structural algorithms. Names are case-insensitive and many procedures also accept `gds.*` aliases for Neo4j Graph Data Science-style migration experiments.

Projection data is derived from graph labels and adjacency indexes. The executor can reuse persisted projection cache entries, and it rebuilds from adjacency data when a cache entry is stale, corrupt, or from an unsupported version.

Treat procedure output as part of the alpha graph surface: pin the exact procedure name, arguments, yielded columns, data shape, and expected rows in tests before depending on it.

## Nullable endpoints

If an edge backing table allows nullable endpoints, decide whether those rows are valid relationships before relying on traversal behavior. In most application models, an edge with `source_id IS NULL` or `target_id IS NULL` should not produce a relationship.

Recommended schema:

```sql
CREATE TABLE assignments (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    assigned_at TEXT
);
```

Use nullable foreign-key-style columns for relational state only when the absence of a relationship is meaningful. Then validate how graph traversal treats those rows.

## Edge properties

Edge properties belong in the edge backing table:

```sql
CREATE TABLE follows (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    since_year INT,
    strength INT
);
```

This keeps relationship metadata queryable from SQL even when graph syntax is not used.

## Dropping labels

```sql
DROP EDGE LABEL friends;
DROP NODE LABEL person;
```

Dropping labels removes graph metadata. It does not drop the backing table data.

## Modeling guidance

- Keep canonical data in tables.
- Use graph labels to describe relationships, not to duplicate state.
- Index endpoint columns used by frequent traversals.
- Validate variable-length or multi-hop patterns before relying on performance.
- Prefer explicit result limits for path and algorithm queries that can grow quickly.
- Keep `EXPLAIN` output with benchmark artifacts when comparing graph plans.

## Evaluation checklist

For a graph workload, document:

- node tables and primary keys;
- edge tables and endpoint columns;
- expected direction of traversal;
- indexes on endpoint columns;
- whether endpoints can be null;
- equivalent SQL query for at least one important pattern;
- expected row count for a small fixture.
