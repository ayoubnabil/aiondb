---
title: Multimodal Database
seo_title: Multimodal database for SQL, graph, and vector search | AionDB
description: AionDB is a multimodal database in Rust that combines SQL tables, graph relationships, and vector search through a PostgreSQL-compatible engine.
lang: en
order: 2
---

# Multimodal database for SQL, graph, and vector search

AionDB is a multimodal (multi-model) database in Rust. Relational data,
graph relationships, and vector embeddings live in one engine.

The point is to stop copying the same application data into a SQL
database, a graph database, and a vector database whenever a query
needs all three.

## Why a multimodal database?

Many applications need several data shapes at once:

- tables for users, documents, tickets, products, events, business state;
- graph edges for connections between those records;
- vectors for semantic search, recommendations, RAG, AI assistants.

AionDB exposes the three through one PostgreSQL-compatible engine.
Tables remain the source of truth. Graph and vector queries operate
over the same rows.

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

Useful for knowledge bases, product catalogs, support tooling, private
copilots, and any application that needs structured business context
alongside semantic retrieval.

## PostgreSQL-compatible tooling

AionDB speaks the PostgreSQL wire protocol so `psql`, pgAdmin,
migrations, ORMs, and standard drivers keep working when the features
they touch are supported.

## Project status

AionDB is alpha software. It does not replace PostgreSQL in production
today. The narrower claim: it gives teams an engine for evaluating a
multimodal database that combines SQL, graph, and vector search without
running three separate services next to each other.

Start with the [documentation](/documentation/) and the [reproducible benchmarks](/documentation/evaluate/benchmarks.html).
