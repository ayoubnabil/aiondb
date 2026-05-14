---
title: Tutorial
order: 15
---

# Tutorial

This tutorial builds a small knowledge-base dataset. It uses ordinary SQL tables, graph labels over those tables, and vector similarity functions on the same records.

You will create:

- documents with embeddings;
- concepts mentioned by those documents;
- graph labels over the same tables;
- SQL queries that mix joins and vector scoring.

## Start a local server

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
cargo run -p aiondb-server --bin aiondb -- --ephemeral
```

Connect with `psql`:

```bash
psql "host=127.0.0.1 port=5432 dbname=default user=dev password=ReplaceWithLongUniquePassword42! sslmode=disable"
```

## Create the schema

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

CREATE TABLE doc_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE TABLE doc_mentions (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE TABLE query_vectors (
    id INT NOT NULL,
    label TEXT,
    embedding VECTOR(2)
);
```

The schema is still relational. Graph and vector behavior is added without moving data out of these tables.

## Add graph labels

```sql
CREATE NODE LABEL doc ON docs;
CREATE NODE LABEL concept ON concepts;
CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;
CREATE EDGE LABEL mentions_concept ON doc_mentions SOURCE doc TARGET concept;
```

Node labels map rows to graph nodes. Edge labels map rows in edge tables to graph relationships. The default edge endpoint column names are `source_id` and `target_id`.

## Insert data

```sql
INSERT INTO docs VALUES
    (1, 'Incident Response Playbook', 'runbook', '[0.0,0.0]'),
    (2, 'Pager Escalation Guide', 'guide', '[1.0,0.0]'),
    (3, 'Postmortem Template', 'template', '[0.2,0.8]'),
    (4, 'Database Recovery Runbook', 'runbook', '[0.9,0.1]'),
    (5, 'Hiring Handbook', 'policy', '[5.0,5.0]');

INSERT INTO concepts VALUES
    (10, 'incident-response', 'topic'),
    (20, 'oncall', 'topic'),
    (30, 'database', 'topic');

INSERT INTO doc_links VALUES
    (1, 2, 'supports'),
    (1, 3, 'explains'),
    (1, 4, 'references'),
    (2, 4, 'depends_on'),
    (3, 4, 'references');

INSERT INTO doc_mentions VALUES
    (1, 10),
    (1, 20),
    (2, 10),
    (2, 20),
    (3, 10),
    (4, 10),
    (4, 30);

INSERT INTO query_vectors VALUES
    (1, 'incident_ops', '[1.0,0.0]'),
    (2, 'postmortem', '[0.0,1.0]');
```

## Query with SQL joins

Find runbooks connected to the `database` concept:

```sql
SELECT d.id, d.title
FROM docs d
JOIN doc_mentions dm ON dm.source_id = d.id
JOIN concepts c ON c.id = dm.target_id
WHERE c.name = 'database'
ORDER BY d.id;
```

Expected result:

```text
 id |          title
----+-------------------------
  4 | Database Recovery Runbook
```

## Query by vector similarity

Rank documents by distance from the `incident_ops` query vector:

```sql
SELECT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist
FROM docs d
JOIN query_vectors q ON q.label = 'incident_ops'
ORDER BY dist ASC
LIMIT 3;
```

The closest rows should be the documents whose embeddings are nearest to `[1.0,0.0]`, with `Pager Escalation Guide` and `Database Recovery Runbook` near the top.

Cosine distance is also available:

```sql
SELECT d.id, d.title, cosine_distance(d.embedding, q.embedding) AS dist
FROM docs d
JOIN query_vectors q ON q.label = 'postmortem'
ORDER BY dist ASC
LIMIT 3;
```

## Mix relationship filters and vector scoring

Find documents that mention `incident-response`, then rank them by semantic distance:

```sql
SELECT d.id, d.title, l2_distance(d.embedding, q.embedding) AS dist
FROM docs d
JOIN doc_mentions dm ON dm.source_id = d.id
JOIN concepts c ON c.id = dm.target_id
JOIN query_vectors q ON q.label = 'incident_ops'
WHERE c.name = 'incident-response'
ORDER BY dist ASC
LIMIT 5;
```

This is the core idea behind AionDB: the application can keep canonical state in tables while still expressing graph-like relationships and vector search over the same data.

## Try a graph pattern

When graph labels are present, Cypher-style graph queries can address the labeled tables:

```sql
MATCH (d:doc)-[:related_doc]->(next:doc)
RETURN d.title, next.title
LIMIT 10;
```

Graph support is still evolving in v0.1. If a graph query does not work for your pattern, rewrite it as explicit SQL joins and file a reduced repro.

## Clean up

For an ephemeral server, stop the process to discard the dataset. For persistent local data, drop the tutorial tables explicitly:

```sql
DROP TABLE doc_mentions;
DROP TABLE doc_links;
DROP TABLE query_vectors;
DROP TABLE concepts;
DROP TABLE docs;
```
