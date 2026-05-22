---
title: Why AionDB beats SurrealDB by 113x on graph scans
seo_title: Why AionDB beats SurrealDB by 113x on graph scans | AionDB
description: AionDB is 113x faster than SurrealDB on the graph_multi_count benchmark. This breakdown explains the graph scan workload, the benchmark setup, and why multimodal databases matter for RAG apps.
date: 2026-05-21T00:00:00+02:00
author: AionDB Engineering
image: /aiondb-logo-light.png
tags: SurrealDB benchmark, graph database benchmark, multimodal database, embedded database, RAG database, vector search
order: 10
---

<div class="blog-post">
<header class="blog-post-hero">
<p class="blog-eyebrow">Graph benchmark</p>
<h1>Why AionDB beats SurrealDB by 113x on graph scans</h1>
<p class="blog-dek">AionDB reaches <strong>2,597.4 ops/s</strong> on the measured graph multi-count scan. SurrealDB reaches <strong>22.82 ops/s</strong> on the same benchmark family. That is roughly <strong>113x faster</strong> for AionDB on this graph-heavy workload.</p>
<div class="blog-meta">
<span>Published May 21, 2026</span>
<span>Benchmark: SurrealDB suite</span>
<span>Workload: graph_multi_count</span>
</div>
</header>

<section class="blog-proof-strip" aria-label="Benchmark summary">
<div>
<span>AionDB</span>
<strong>2,597.4 ops/s</strong>
</div>
<div>
<span>SurrealDB</span>
<strong>22.82 ops/s</strong>
</div>
<div>
<span>Result</span>
<strong>113x faster</strong>
</div>
</section>

## Short version

AionDB is built around a simple database idea: keep relational records, graph relationships, and vector embeddings in one engine instead of syncing the same data across separate systems.

That design matters for graph-heavy scans. In the current benchmark snapshot, the `graph_multi_count` workload is a clear win for AionDB because the query shape is exactly what AionDB is designed to run well: relationship-heavy counting over structured data.

For developers building RAG apps, knowledge graphs, AI agents, recommendations, fraud workflows, dependency maps, or entity relationship systems, this is the important takeaway:

- AionDB is not just a SQL database with a vector bolt-on.
- AionDB is not only a graph demo.
- AionDB is a multimodal embedded database path for SQL, graph, and vector workloads in one Rust engine.

## The benchmark result in plain English

The measured result is:

- AionDB: `2,597.4 ops/s`
- SurrealDB: `22.82 ops/s`
- Difference: about `113x`

In practical terms, this benchmark asks the database to scan connected data and count graph-shaped relationships. It is not a toy “hello world” query. It is the kind of operation that appears inside knowledge graph exploration, recommendation prefilters, access graphs, dependency maps, graph analytics, and RAG systems that need structured context before vector ranking.

The benchmark comes from the AionDB `surreal-suite` harness, which mirrors public SurrealDB 3 benchmark families by name. The comparison keeps protocol paths explicit:

- AionDB uses the PostgreSQL wire protocol.
- SurrealDB uses WebSocket JSON-RPC.
- The benchmark runs on durable local storage.

The full benchmark documentation is published in [Benchmark Results](/documentation/evaluate/benchmark-results.html), and the current vector release is covered in [v0.3 Vector Performance](/documentation/evaluate/v0-3-vector-performance.html).

## Why graph scans matter for RAG

Most RAG systems start simple: chunk documents, create embeddings, put vectors in a vector database, and retrieve the nearest matches.

That works until the product needs real business context:

- Which customer owns this document?
- Which runbooks depend on this service?
- Which incidents cite this deployment?
- Which users, teams, tickets, and documents are connected?
- Which vector matches are allowed by permissions, tenant rules, or graph relationships?

At that point, vector search alone is not enough. The application needs SQL filters, graph relationships, and vector ranking together.

That is the exact reason AionDB exists. Tables stay the source of truth. Graph labels and edge labels sit over ordinary tables. Vector columns and distance functions live beside the same records. The application does not have to copy the same data into PostgreSQL, SurrealDB, Neo4j, and a separate vector store just to answer one RAG query.

## Why AionDB wins this workload

The 113x result is not magic. It comes from workload fit.

AionDB is designed for graph-shaped work over structured records. The engine can treat graph labels, edge labels, relational rows, and vector values as parts of the same database model. For graph scans that count connected records, that is a strong shape.

The `graph_multi_count` workload rewards three things:

1. Fast access to relationship-shaped data.
2. Low overhead when counting graph matches.
3. A query path that does not force the application to stitch results across separate services.

AionDB’s model is intentionally boring in the right place: keep the core data as tables, then add graph and vector access paths around that same catalog. For this benchmark, boring is fast.

## More than one result

The 113x number is the headline because it is the cleanest graph-scan win. It is not the only graph-heavy result where AionDB is ahead of SurrealDB in the current snapshot.

On the same benchmark family:

- `graph_edge_filter`: AionDB is about 4.8x faster than SurrealDB.
- `graph_bidirectional`: AionDB is about 2.2x faster than SurrealDB.
- `graph_multi_count`: AionDB is about 113x faster than SurrealDB.

These are the workloads that matter when an application needs to filter, count, and explore connected data before doing higher-level ranking or analysis.

## Why this matters for a multimodal database

The database market has been split into separate boxes:

- PostgreSQL for relational data.
- Neo4j or SurrealDB for graph-shaped data.
- pgvector or a vector database for embeddings.
- DuckDB for local analytics.

That split creates real engineering cost. Data gets copied. Permissions drift. Indexes disagree. Pipelines break. RAG quality suffers because the vector result is detached from the actual business graph.

AionDB’s bet is different: one local engine for SQL, graph, and vector search.

That is why the phrase “multimodal embedded database” matters. AionDB is not trying to make developers choose between relational correctness, graph relationships, and vector retrieval. It is building toward one database surface where those workloads can meet.

## Reproduce the benchmark

The benchmark harness is in the repository. The current broad comparison can be run with:

```bash
SURREAL_SUITE_ROWS=2000 \
SURREAL_SUITE_WARMUP_SECONDS=3 \
SURREAL_SUITE_ITERATIONS=1 \
SURREAL_SUITE_DURATION_SECONDS=20 \
benchmarks/run.sh surreal-suite
```

The relevant published snapshot is the broader SurrealDB / pgstack matrix used by the v0.2 performance page:

- Run id: `full-all-20260512T192959Z`
- Engines: `aiondb`, `surrealdb`, `pgstack`
- Storage: durable local storage
- Workload family: SurrealDB-style CRUD, scan, graph, full-text, and vector tests

Read the full numbers in [Benchmark Results](/documentation/evaluate/benchmark-results.html).

## The product takeaway

If you are evaluating SurrealDB alternatives, graph database benchmarks, embedded RAG databases, or multimodal database engines, AionDB is worth testing because it combines three surfaces that usually live apart:

- SQL records.
- Graph relationships.
- Vector search.

The 113x graph-scan result is a signal that this architecture can pay off on real connected-data workloads.

Start here:

- [AionDB documentation](/documentation/)
- [Multimodal database overview](/multimodal-database.html)
- [Benchmark results](/documentation/evaluate/benchmark-results.html)
- [GitHub repository](https://github.com/ayoubnabil/aiondb)

</div>
