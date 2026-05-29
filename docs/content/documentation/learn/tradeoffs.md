---
title: Tradeoffs
order: 79
---

# Tradeoffs

Every database architecture has costs. These are AionDB's.

## Where AionDB aims

- One engine for SQL, graph labels, and vector search.
- PostgreSQL wire access for existing clients.
- Embedded Rust access for in-process applications.
- Explicit catalog model for graph and vector metadata.
- A codebase a reader can inspect without learning a decades-old server.

The argument is not that AionDB beats mature systems on their home turf. The argument is that modern application data often carries three shapes at once: relational facts, relationships, and embeddings. AionDB treats that combination as a database model instead of an integration problem.

## Where mature databases are stronger today

PostgreSQL is stronger for broad SQL compatibility, extensions, operational maturity, and ecosystem depth.

Columnar analytical engines are stronger for scan-heavy analytics.

Dedicated graph engines are stronger for deep graph traversal and mature graph algorithms.

Dedicated vector systems may be stronger for large-scale approximate nearest-neighbor search, recall tuning, filtering, and compaction.

This is the baseline a user compares against. For ordinary OLTP with mature operational requirements, PostgreSQL is the more credible default. For pure analytics, DuckDB-style columnar execution is the more credible default. For vector retrieval at large scale, a dedicated vector system may be easier to tune.

## Where AionDB may be slower

Expect AionDB v0.1 to be weaker on:

- high-frequency single-row write workloads compared with mature WAL implementations;
- full PostgreSQL compatibility workloads;
- large analytical scans without a mature columnar path;
- deep graph traversals when adjacency/index layout is not optimized for the pattern;
- large vector datasets that need mature filtered ANN behavior.

## Workloads that make sense

AionDB is interesting when the application would otherwise wire several systems together:

- support tickets with owners, escalation paths, comments, and embeddings;
- product catalogs with relational attributes, entity links, and semantic search;
- knowledge bases where documents mention entities and are also vector-ranked;
- local-first applications that need an embedded database but want a server path later.

The relevant comparison is not raw speed. It is operational complexity, data duplication, consistency, and how much application code exists only to keep several stores synchronized.

## Workloads that do not make sense yet

Do not position v0.1 as the answer for:

- existing PostgreSQL applications that depend heavily on extensions;
- large scan-heavy analytics;
- graph algorithms over very large dense graphs;
- public hosted multi-tenant workloads;
- strict disaster-recovery requirements;
- applications that need a stable on-disk format today.

## Why evaluate it

Evaluate AionDB when the interesting part of the workload is the combination: relational state, relationships, and embeddings in one engine. The v0.1 question is not whether AionDB is the fastest database everywhere. The useful question is whether the model is worth building further.

## Comparison rule

When comparing AionDB to another database, state where AionDB loses. A credible comparison includes:

- one workload where AionDB is promising;
- one workload where the other system is clearly stronger;
- the protocol used by both systems;
- durability settings;
- data size and indexes;
- raw query text and output.
