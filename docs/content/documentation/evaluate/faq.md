---
title: FAQ
order: 92
---

# FAQ

## Is AionDB a PostgreSQL fork?

No. AionDB is a new Rust database engine with a PostgreSQL wire surface and PostgreSQL-compatible behavior where implemented.

That means PostgreSQL clients can be useful, but PostgreSQL behavior is not inherited automatically. Parser support, catalog behavior, system functions, transaction semantics, and protocol details are implemented by AionDB itself.

## Is AionDB production-ready?

No. v0.1 is an alpha for evaluation, inspection, and feedback.

Use it where data can be recreated. Do not use it as the only durable copy of important production data unless you have done your own failure, backup, restore, and driver validation.

Internal testing, fuzzing, and compatibility validation are promising, but that still does not meet the bar for a public production-ready claim.

The project will only claim production readiness after at least one month of continuous testing and fuzzing on the release line being shipped.

## Why PostgreSQL wire protocol?

PostgreSQL wire compatibility lets existing tools, drivers, and workflows connect without requiring a custom client protocol.

It is also a forcing function. Real drivers expose protocol edge cases quickly: prepared statements, startup parameters, error codes, type formats, portals, command tags, and transaction state.

## Why combine SQL, graph, and vector?

Many application workloads keep relational records, relationships, and embeddings side by side. AionDB queries all three from one engine and one catalog, so the same row does not have to be replicated into a graph database and a vector database.

The target is not to replace every specialized database immediately. The target is to reduce duplicate state for hybrid workloads where a row also has relationships and an embedding.

## Can I use AionDB as an embedded database?

Yes. The embedded Rust API exposes an in-process database surface using the same engine.

Embedded mode is useful for local applications, tests, and tools that want database behavior without running a separate process. Server mode is still the right path for validating PostgreSQL drivers and network clients.

## Can I use AionDB as a hosted database service?

Not without a separate commercial license. AionDB's BSL additional use grant
allows production, embedding, redistribution, and internal services, but it does
not allow offering AionDB as a commercial hosted database service, managed
database service, or DBaaS to third parties.

## Does every version become open source later?

Yes. Each BSL release converts to Apache License 2.0 on the change date stated
in `LICENSE`, or on the fourth anniversary of that release, whichever comes
first.

## Should I migrate production data to v0.1?

No. Use v0.1 for evaluation and prototypes. Keep data reproducible from source fixtures or imports.

## What should I try first?

Start with a workload that is small but representative:

- create a schema with real table shapes;
- load enough rows to make query plans matter;
- run the exact driver or ORM you care about;
- include at least one SQL query, one relationship query, and one vector-ranking query if those are relevant;
- record the commands so the test can be repeated.

Avoid starting with a huge migration. A small complete repro teaches more than a large unclear failure.

## What is the main technical risk?

The main v0.1 risk is not one isolated missing feature. It is breadth: SQL compatibility, pgwire details, graph planning, vector indexing, storage, transactions, observability, and recovery all interact. AionDB needs focused evaluation because bugs can appear at the boundaries.

## How should I compare it to PostgreSQL or vector databases?

Compare on a real workload, not a slogan. Disclose protocol, schema, data size, indexes, durability, and raw query text. If a competing database lacks SQL joins or uses a different graph model, write the closest equivalent query and explain the difference.

## How should I report a bug?

Provide the smallest SQL script that reproduces it, plus the AionDB commit hash, expected behavior, actual behavior, and client driver if relevant.

Good reports include:

- server command;
- client command;
- schema;
- inserts;
- failing query;
- expected output;
- actual output or error;
- whether the same script works on another database.
