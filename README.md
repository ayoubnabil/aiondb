<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/theme/aiondb-logo-dark.png">
    <img src="docs/theme/aiondb-logo-light.png" alt="AionDB logo" width="170">
  </picture>
</p>

<h1 align="center">AionDB</h1>

<p align="center">
  <strong>PostgreSQL-compatible SQL, vector search, and graph queries in one Rust database.</strong>
</p>

<p align="center">
  <a href="https://aiondb.xyz/">Website</a>
  ·
  <a href="docs/content/documentation/start/getting-started.md">Getting Started</a>
  ·
  <a href="docs/content/documentation/query/vector-reference.md">Vector Reference</a>
  ·
  <a href="docs/content/documentation/evaluate/benchmark-results.md">Benchmark Results</a>
</p>

<p align="center">
  <a href="https://discord.gg/v9gwAFS7Yp">
    <img alt="Join Discord" src="https://img.shields.io/badge/Discord-Join%20the%20community-5865F2?logo=discord&logoColor=white">
  </a>
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-database-orange">
  <img alt="PostgreSQL wire" src="https://img.shields.io/badge/PostgreSQL-wire%20compatible-336791">
  <img alt="Vector search" src="https://img.shields.io/badge/vector-HNSW%20%2B%20IVF-4f46e5">
  <img alt="License" src="https://img.shields.io/badge/license-BSL%201.1-blue">
</p>

AionDB is built around a simple product thesis: modern applications should not
have to split the same data across PostgreSQL, a vector database, a graph
database, and a cache layer just to answer one intelligent query.

Status: **v0.3 vector update**. AionDB now brings PostgreSQL-facing SQL,
graph relationships, pgvector-style vector types, HNSW, IVF-flat, and
Qdrant-style filtered retrieval into one Rust engine.

The engine keeps tables as the source of truth, exposes a PostgreSQL-compatible
wire surface for existing tools, and adds vector and graph capabilities around
the same catalog and execution pipeline.

**Current release focus:** v0.3 vector update.

**Current development focus:** v0.4 general optimization: planner quality,
joins, graph execution, memory use, and large workload performance.

## Why AionDB

Most AI applications eventually need more than nearest-neighbor search:

- filter documents by tenant, permissions, timestamps, or business state;
- rank candidates by vector similarity;
- traverse relationships between records;
- keep the canonical data model queryable through SQL;
- avoid syncing the same objects into three different systems.

AionDB is designed for that shape.

```sql
CREATE TABLE documents (
    id INT PRIMARY KEY,
    tenant_id INT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    embedding VECTOR(768)
);

CREATE INDEX documents_embedding_hnsw
ON documents USING hnsw (embedding vector_cosine_ops);

SELECT id, title
FROM documents
WHERE tenant_id = 42
ORDER BY cosine_distance(embedding, $1) ASC
LIMIT 10;
```

That query stays relational, filterable, indexable, and vector-aware. The
application does not need a separate vector payload store just to keep metadata
beside embeddings.

## What You Get

| Area | AionDB surface |
| --- | --- |
| SQL | Tables, predicates, joins, functions, transactions, indexes, and PostgreSQL-style catalogs |
| PostgreSQL compatibility | PostgreSQL wire protocol, `psql`, common drivers, ORM-oriented compatibility work |
| Vector search | `VECTOR(n)`, `HALFVEC(n)`, pgvector-compatible casts/functions, HNSW, IVF-flat syntax, filtered top-k helpers |
| Hybrid search | SQL filters and vector ranking over the same rows |
| Graph | Node labels and edge labels over ordinary tables, graph traversal paths, graph-oriented execution |
| Operations | Health endpoints, metrics, doctor, upgrade, dump, restore, Docker Compose, reproducible benchmark harnesses |
| Engine | Rust workspace, embedded API, storage engine, optimizer, executor, pgwire server, docs site |

## Product Shape

AionDB is not a vector-only system and it is not a graph database bolted beside
SQL. It treats relational records, vector embeddings, and graph relationships as
different access paths over the same application state.

```sql
CREATE TABLE docs (
    id INT PRIMARY KEY,
    title TEXT,
    embedding VECTOR(3)
);

CREATE TABLE doc_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE NODE LABEL doc ON docs;
CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;

SELECT id, title, l2_distance(embedding, '[1.0,0.0,0.0]') AS distance
FROM docs
ORDER BY distance ASC
LIMIT 10;
```

The important part is not only syntax. The model lets one engine reason about
structured filters, vector ranking, and relationships together.

## Quick Start

Clone the repository, create a local environment file, then start AionDB and
Studio with prebuilt images:

```bash
git clone https://github.com/ayoubnabil/aiondb.git
cd aiondb
cp quickstart.env .env
docker compose --profile studio up
```

Open Studio:

```text
http://127.0.0.1:8082
```

Or connect with `psql`:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"
```

Run a quick smoke test:

```sql
CREATE TABLE tickets (
    id INT PRIMARY KEY,
    title TEXT,
    priority TEXT
);

INSERT INTO tickets VALUES
    (1, 'pgwire smoke test', 'high'),
    (2, 'embedded api check', 'normal');

SELECT id, title
FROM tickets
WHERE priority = 'high';
```

Compose pulls prebuilt images from GitHub Container Registry by default:

- `ghcr.io/ayoubnabil/aiondb:main`
- `ghcr.io/ayoubnabil/aiondb-studio:main`

Use the local Docker build file only when changing Dockerfiles or testing
unpublished images:

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml --profile studio up --build
```

For engine development, build the release binary directly:

```bash
cargo build --release -p aiondb-server --bin aiondb
target/release/aiondb --version
```

## Vector Search

AionDB accepts pgvector-style extension setup and vector DDL:

```sql
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE embeddings (
    id INT PRIMARY KEY,
    source TEXT,
    vec VECTOR(4)
);

CREATE INDEX embeddings_vec_hnsw
ON embeddings USING hnsw (vec vector_l2_ops);

SELECT id, source
FROM embeddings
ORDER BY vec <-> '[0.1,0.2,0.3,0.4]'
LIMIT 5;
```

The vector surface includes:

- fixed-dimension `VECTOR(n)` columns;
- pgvector-compatible casts and helper functions;
- HNSW and IVF-flat index syntax;
- L2, cosine, inner-product, and L1/manhattan distance support;
- filtered vector search helpers with Qdrant-style JSON filter options;
- query planner work for choosing between vector-first and filter-first plans.

Read the [Vector Reference](docs/content/documentation/query/vector-reference.md)
for exact syntax and compatibility details.

## Benchmarks

AionDB keeps benchmark harnesses in the repository so performance claims can be
tied to a command, commit, dataset, and machine.

```bash
benchmarks/run.sh --help
benchmarks/run.sh surreal-suite
```

The benchmark docs include SQL, graph, vector, full-text, and hybrid workloads,
plus generated result snapshots:

- [Benchmarks](docs/content/documentation/evaluate/benchmarks.md)
- [Benchmark Results](docs/content/documentation/evaluate/benchmark-results.md)
- [Benchmark Reproducibility](docs/content/documentation/evaluate/benchmark-reproducibility.md)

## Documentation

Start here:

- [Getting Started](docs/content/documentation/start/getting-started.md)
- [Installation](docs/content/documentation/start/installation.md)
- [Tutorial](docs/content/documentation/start/tutorial.md)
- [Example Workloads](docs/content/documentation/start/example-workloads.md)
- [Core Concepts](docs/content/documentation/learn/core-concepts.md)
- [What's New in v0.3](docs/content/documentation/project/whats-new-v0-3.md)
- [v0.3 Vector Performance](docs/content/documentation/evaluate/v0-3-vector-performance.md)
- [Architecture](docs/content/documentation/learn/architecture.md)
- [SQL Reference](docs/content/documentation/query/sql.md)
- [Graph and Vector](docs/content/documentation/query/graph-and-vector.md)
- [Vector Reference](docs/content/documentation/query/vector-reference.md)
- [Operations](docs/content/documentation/manage/operations.md)
- [Benchmarks](docs/content/documentation/evaluate/benchmarks.md)

Build the documentation site locally:

```bash
python3 docs/build.py
python3 docs/build.py --serve
```

## Operations Surface

The local operations surface includes:

- `GET /livez`, `GET /healthz`, `GET /readyz`, `GET /metrics`, and `GET /info`;
- `aiondb doctor --data-dir <path>`;
- `aiondb upgrade --data-dir <path>`;
- `aiondb dump` and `aiondb restore`;
- `make product-smoke`;
- Docker Compose profiles for server and Studio;
- benchmark harnesses under `benchmarks/`.

`aiondb dump` and `aiondb restore` require `AIONDB_BOOTSTRAP_USER` and
`AIONDB_BOOTSTRAP_PASSWORD`. Backup paths are relative to `./backups/` in the
current working directory.

## Repository Layout

```text
crates/
  aiondb-server          pgwire server and HTTP control surface
  aiondb-optimizer       logical and physical planning
  aiondb-executor        query execution
  aiondb-storage-engine  storage, HNSW, IVF, and index internals
  aiondb-vector          vector planner/runtime integration
  aiondb-graph           graph data structures and path support
docs/                    documentation site
benchmarks/              reproducible benchmark harnesses
```

## License

AionDB core is source-available under the [Business Source License 1.1](LICENSE)
with a project-specific Additional Use Grant.

- Production use, embedding, modification, and redistribution are allowed under
  the terms in `LICENSE`.
- Commercial hosted database service, managed database service, and DBaaS use
  require a separate commercial license unless the entity qualifies for the
  small-company grant defined in `LICENSE`.
- Each release converts to Apache License 2.0 on the change date stated in
  `LICENSE`, or on the fourth anniversary of that release, whichever comes
  first.

See [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md), [NOTICE](NOTICE), and
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for the rest of the license
surface.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), [GOVERNANCE.md](GOVERNANCE.md), and
[SECURITY.md](SECURITY.md).

Contributions must include a Developer Certificate of Origin sign-off:

```text
Signed-off-by: Your Name <you@example.com>
```

Use:

```bash
git commit -s
```
