---
title: Presentation
order: 1
---

# Presentation

AionDB is a Rust database project built around a simple product thesis: relational data, graph relationships, and vector similarity should be queryable through one engine without forcing the application to keep three separate systems in sync.

It exposes a PostgreSQL-compatible wire surface for existing tools and a Rust embedded API for applications that want the engine in-process.

## Design goals

- Keep one canonical execution path for server and embedded usage.
- Speak enough PostgreSQL wire protocol to work with real clients and drivers.
- Make SQL the stable base, then add graph and vector primitives around the same catalog and storage.
- Prefer explicit errors over silent compatibility tricks.
- Keep the internal architecture inspectable by separating parser, planner, optimizer, executor, storage, catalog, and WAL responsibilities.

## Product thesis

The product thesis is not "one database wins every benchmark." The thesis is that many application teams already maintain the same logical dataset in several systems:

- PostgreSQL for relational state;
- a graph database or application-level edge tables for relationships;
- a vector database or extension for embeddings;
- custom glue code to keep them consistent.

AionDB asks whether that combination can be made simpler by starting from one SQL engine and adding graph/vector access around the same catalog and storage model.

## What makes it different

AionDB is not trying to be a PostgreSQL fork. It is a new engine with PostgreSQL-facing compatibility where that helps adoption. The long-term bet is a unified query system for application data that naturally mixes tables, relationships, and embeddings.

That tradeoff matters. AionDB will not beat mature specialized engines on every workload. PostgreSQL remains stronger for broad SQL compatibility and operational maturity. DuckDB remains stronger for columnar analytics. Dedicated graph engines remain stronger on deep graph traversal. The reason to evaluate AionDB is the combined surface, not a claim that every subsystem is already best in class.

## v0.1 scope

The v0.1 release is an alpha intended for technical evaluation. It is appropriate for:

- testing the query model;
- validating PostgreSQL wire behavior with clients;
- experimenting with embedded usage;
- running local benchmarks;
- reviewing the internals.

It is not yet positioned for production data, hosted multi-tenant service operation, or compatibility-sensitive database migrations.

## Evaluation standard

A useful evaluation should include:

- the exact commit;
- the server command;
- client or driver version;
- schema and data volume;
- query text;
- durability settings;
- raw output.

The most useful feedback is a reduced script that proves a behavior difference, performance issue, or documentation gap.

## Documentation map

Start with [Documentation](/documentation/) for user-facing setup and concepts. Use [Advanced Specification](/specification-avancee/) for crate-level design notes and implementation details.
