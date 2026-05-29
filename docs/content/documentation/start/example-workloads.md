---
title: Example Workloads
order: 16
---

# Example Workloads

These examples show the hybrid data model. They are small on purpose.

## Knowledge base search

Use SQL tables for documents and concepts, graph labels for relationships, and vectors for semantic ranking.

```sql
CREATE TABLE docs (
    id INT NOT NULL,
    title TEXT,
    kind TEXT,
    embedding VECTOR(2)
);

CREATE TABLE concepts (
    id INT NOT NULL,
    name TEXT,
    kind TEXT
);

CREATE TABLE doc_mentions (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE NODE LABEL doc ON docs;
CREATE NODE LABEL concept ON concepts;
CREATE EDGE LABEL mentions_concept ON doc_mentions SOURCE doc TARGET concept;
```

Query shape:

```sql
SELECT d.id, d.title, l2_distance(d.embedding, '[1.0,0.0]') AS dist
FROM docs d
JOIN doc_mentions dm ON dm.source_id = d.id
JOIN concepts c ON c.id = dm.target_id
WHERE c.name = 'incident-response'
ORDER BY dist ASC
LIMIT 10;
```

What this tests:

- SQL joins over relationship tables;
- vector ranking on the document table;
- concept filters before ranking;
- whether graph metadata can describe the same relationships.

Production-like extensions to test later:

- workspace or tenant filter;
- document visibility rules;
- larger embeddings;
- HNSW index on `docs.embedding`;
- endpoint indexes on `doc_mentions`.

## Product recommendations

Products can be filtered relationally, connected through graph edges, and ranked by vector similarity.

```sql
CREATE TABLE products (
    id INT NOT NULL,
    sku TEXT,
    title TEXT,
    category TEXT,
    price INT,
    embedding VECTOR(2)
);

CREATE TABLE product_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE NODE LABEL product ON products;
CREATE EDGE LABEL related_product ON product_links SOURCE product TARGET product;
```

Query shape:

```sql
SELECT p.id, p.title, p.price, l2_distance(p.embedding, '[0.85,0.15]') AS dist
FROM products p
WHERE p.category = 'audio'
ORDER BY dist ASC
LIMIT 5;
```

What this tests:

- ordinary filters such as category and price;
- vector similarity over product embeddings;
- relationship metadata for related products;
- ranking plus `LIMIT`.

Add a SQL join query for related products before testing graph syntax:

```sql
SELECT p2.id, p2.title, pl.relation
FROM products p1
JOIN product_links pl ON pl.source_id = p1.id
JOIN products p2 ON p2.id = pl.target_id
WHERE p1.sku = 'demo-sku';
```

## Workspace context

Projects, notes, and tasks are ordinary tables. Notes can link to other notes and tasks, while embeddings help rank nearby context.

```sql
CREATE TABLE notes (
    id INT NOT NULL,
    project_id INT NOT NULL,
    title TEXT,
    embedding VECTOR(2)
);

CREATE TABLE tasks (
    id INT NOT NULL,
    project_id INT NOT NULL,
    title TEXT,
    status TEXT,
    embedding VECTOR(2)
);

CREATE TABLE note_task_edges (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE NODE LABEL note ON notes;
CREATE NODE LABEL task ON tasks;
CREATE EDGE LABEL note_task ON note_task_edges SOURCE note TARGET task;
```

Query shape:

```sql
SELECT t.id, t.title, l2_distance(t.embedding, '[0.9,0.1]') AS dist
FROM tasks t
WHERE t.status = 'open'
ORDER BY dist ASC
LIMIT 10;
```

What this tests:

- tenant-style filtering through `project_id`;
- status filters;
- vector ranking over active tasks;
- graph-style links between notes and tasks.

For a serious evaluation, add:

- indexes on `project_id` and `status`;
- endpoint indexes on `note_task_edges`;
- a vector index on task embeddings;
- tests for project isolation.

## The modeling pattern

Small datasets do not need a new database. The point is the modeling pattern: one canonical set of tables, with graph and vector access paths over the same data.

## How to turn an example into a benchmark

1. Expand the schema only where the workload needs it.
2. Generate deterministic data.
3. Keep one SQL correctness query.
4. Add one graph query only after the SQL result is known.
5. Add one vector query with an exact brute-force reference.
6. Record indexes and row counts.

This produces a workload that readers can evaluate instead of a synthetic demo with unclear semantics.
