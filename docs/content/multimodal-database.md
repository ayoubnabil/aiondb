---
title: Multimodal Database
seo_title: Multimodal database for SQL, graph, and vector search | AionDB
description: AionDB is a multimodal database in Rust that combines SQL tables, graph relationships, and vector search through a PostgreSQL-compatible engine.
lang: en
order: 2
---

# Multimodal database for SQL, graph, and vector search

AionDB is a multimodal database, also known as a multi-model database, designed to keep relational data, graph relationships, and vector embeddings in one Rust engine.

The practical goal is to avoid copying the same application data into a SQL database, a graph database, and a vector database when the product needs hybrid queries.

## Why a multimodal database?

Modern applications often need several data shapes at once:

- tables for users, documents, tickets, products, events, and business state;
- graph relationships for exploring connections between those records;
- vectors for semantic search, recommendations, RAG, and private AI assistants.

AionDB exposes those models through one PostgreSQL-compatible system. Tables stay the source of truth, while graph and vector queries can operate over the same records.

## Hybrid queries

AionDB targets queries that combine SQL filtering, graph traversal, and vector similarity:

```sql
MATCH (u:User)-[:WROTE]->(d:Document)-[:CITES]->(ref:Document)
WHERE d.kind = 'runbook'
RETURN d.title,
       ref.title,
       l2_distance(d.embedding, '[0.1,0.8,0.2]') AS distance
ORDER BY distance ASC
LIMIT 5;
```

This is useful for knowledge bases, product catalogs, support tooling, private copilots, and applications that need both structured business context and semantic retrieval.

## PostgreSQL-compatible tooling

AionDB speaks the PostgreSQL wire protocol. The goal is to preserve a familiar developer workflow with `psql`, pgAdmin, migrations, ORMs, and standard drivers when the required features are compatible.

## Project status

AionDB is alpha software. It does not claim to replace PostgreSQL in production today. The narrower claim is that it gives teams an experimental engine for evaluating a multimodal database that combines SQL, graph, and vector search without adding multiple separate services.

Start with the [documentation](/documentation/) and the [reproducible benchmarks](/documentation/evaluate/benchmarks.html).
