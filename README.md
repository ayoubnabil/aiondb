AionDB
======

AionDB is a Rust database. It speaks the PostgreSQL wire protocol, stores
rows on disk through its own storage engine, and adds vector and graph
access paths on top of the same catalog.

Status: v0.3. Alpha. Not for production data.

The current development branch is v0.4 (planner, joins, graph execution,
memory use).


Quick start
-----------

Prebuilt images:

    git clone https://github.com/ayoubnabil/aiondb.git
    cd aiondb
    cp quickstart.env .env
    docker compose --profile studio up

Studio: http://127.0.0.1:8082

psql:

    source .env
    PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
    psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} \
          dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"

From source:

    cargo build --release -p aiondb-server --bin aiondb
    target/release/aiondb --version


What works today
----------------

* Tables, predicates, joins, transactions, indexes, PostgreSQL catalogs.
* PostgreSQL wire protocol. psql and common drivers connect.
* `VECTOR(n)` and `HALFVEC(n)` columns, pgvector-compatible casts.
* HNSW and IVF-flat indexes. L2, cosine, inner product, L1.
* Node labels and edge labels over ordinary tables.
* Health, metrics, doctor, upgrade, dump, restore.
* Docker Compose for server and Studio.
* Reproducible benchmark harnesses under `benchmarks/`.


What does not work
------------------

See `docs/content/documentation/evaluate/limitations.md`.


Documentation
-------------

* Getting started: `docs/content/documentation/start/getting-started.md`
* Installation: `docs/content/documentation/start/installation.md`
* SQL reference: `docs/content/documentation/query/sql.md`
* Vector reference: `docs/content/documentation/query/vector-reference.md`
* Graph and vector: `docs/content/documentation/query/graph-and-vector.md`
* Architecture: `docs/content/documentation/learn/architecture.md`
* Operations: `docs/content/documentation/manage/operations.md`
* Benchmarks: `docs/content/documentation/evaluate/benchmarks.md`

Build the docs site locally:

    python3 docs/build.py
    python3 docs/build.py --serve


Repository layout
-----------------

    crates/aiondb-server          pgwire server and HTTP control surface
    crates/aiondb-optimizer       logical and physical planning
    crates/aiondb-executor        query execution
    crates/aiondb-storage-engine  storage, HNSW, IVF, indexes
    crates/aiondb-vector          vector planner/runtime integration
    crates/aiondb-graph           graph data structures and paths
    docs/                         documentation site sources
    benchmarks/                   reproducible benchmark harnesses


License
-------

Business Source License 1.1. See `LICENSE`, `COMMERCIAL-LICENSE.md`,
`NOTICE`, `THIRD_PARTY_LICENSES.md`.

Each release converts to Apache License 2.0 on the change date stated in
`LICENSE`, or four years after the release, whichever comes first.


Contributing
------------

See `CONTRIBUTING.md`, `GOVERNANCE.md`, `SECURITY.md`.

Sign off every commit:

    git commit -s


Community
---------

Discord: https://discord.gg/v9gwAFS7Yp
Website: https://aiondb.xyz/
