---
title: Presentation
order: 1
---

Presentation
============

AionDB is a Rust database. It speaks the PostgreSQL wire protocol and
exposes a Rust embedded API for in-process use. Rows live on disk in the
storage engine. Graph labels and vector columns sit on top of the same
catalog.


Scope
-----

AionDB is alpha. The v0.1 release is meant for technical evaluation:

* trying the query model;
* checking PostgreSQL wire behaviour with real clients;
* embedded use from Rust;
* running the local benchmarks;
* reading the internals.

It is not meant for production data, hosted multi-tenant service, or
compatibility-sensitive migrations from PostgreSQL.


Design notes
------------

* One execution path serves both the server and the embedded API.
* SQL is the stable base. Graph and vector access paths reuse the same
  catalog and storage.
* Speak enough PostgreSQL wire protocol to work with real drivers. Do
  not pretend to be PostgreSQL beyond that.
* Return an explicit error rather than silently emulating a feature that
  is not implemented.
* The pipeline is split into parser, planner, optimizer, executor,
  storage, catalog, and WAL. Each stage is replaceable.


What AionDB is not
------------------

AionDB is not a PostgreSQL fork. It will not match PostgreSQL on broad
SQL compatibility or operational maturity. It will not match DuckDB on
columnar analytics. It will not match a dedicated graph engine on deep
traversal workloads. Evaluate AionDB only if you actually need rows,
graph edges, and vectors in the same engine; otherwise use the
specialised tool.


Reporting issues
----------------

A useful bug report or benchmark result includes:

* commit hash;
* server command and configuration;
* client or driver version;
* schema and data volume;
* the exact query text;
* durability settings;
* raw output.

A reduced script that reproduces the behaviour is worth more than any
amount of prose.


Where to read next
------------------

* User-facing setup and concepts: [Documentation](/documentation/).
* Crate-level design notes: [Advanced Specification](/specification-avancee/).
