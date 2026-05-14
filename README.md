<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/theme/aiondb-logo-dark.png">
    <img src="docs/theme/aiondb-logo-light.png" alt="AionDB logo" width="160">
  </picture>
</p>

<h1 align="center">AionDB</h1>

<p align="center">
  PostgreSQL-compatible SQL, graph, and vector database written in Rust.
</p>

<p align="center">
  <a href="https://aiondb.xyz/">Website</a>
</p>

AionDB is a source-available database engine built around one practical idea:
application data should not need to be split across a relational database, a
graph database, and a vector store just to support modern workloads.

It keeps tables as the source of truth, exposes a PostgreSQL wire surface for
existing tools, and adds graph and vector capabilities in the same engine and
catalog.

Status: **v0.1 alpha**. AionDB is intended for evaluation, local experiments,
driver compatibility work, benchmarks, and architecture review. It is not yet
a production replacement for mature database systems.

## What It Provides

- PostgreSQL wire server for existing drivers, tools, and ORMs.
- Embedded Rust API for in-process use.
- SQL-first relational model.
- Graph node and edge labels over ordinary tables.
- Fixed-dimension vector columns, distance functions, and HNSW index DDL.
- Local benchmark and compatibility harnesses.

## Quick Start

Clone the repository, write a local `.env`, then start the prebuilt containers:

```bash
git clone https://github.com/ayoubnabil/aiondb.git
cd aiondb
cp quickstart.env .env
docker compose --profile studio up
```

Compose pulls prebuilt images from GitHub Container Registry by default, so this
path does not build AionDB locally. Every push to `main` publishes
`ghcr.io/ayoubnabil/aiondb:main` and
`ghcr.io/ayoubnabil/aiondb-studio:main`; use `docker-compose.build.yml` only
when changing Dockerfiles or testing unpublished images. After the first pull,
startup should only be container creation and health checks.

Open Studio at `http://127.0.0.1:8082`, or connect with `psql`:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"
```

Run a quick SQL smoke:

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

Build the release binary directly when developing AionDB itself:

```bash
cargo build --release -p aiondb-server --bin aiondb
target/release/aiondb --version
```

Build the local container images only when you need to test Dockerfile changes:

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml --profile studio up --build
```

## Product Shape

AionDB models graph and vector data over ordinary tables:

```sql
CREATE TABLE docs (
    id INT PRIMARY KEY,
    title TEXT,
    embedding VECTOR(2)
);

CREATE TABLE doc_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE NODE LABEL doc ON docs;
CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;

SELECT id, title, l2_distance(embedding, '[1.0,0.0]') AS dist
FROM docs
ORDER BY dist ASC
LIMIT 10;
```

## Documentation

Start here:

- [Getting Started](docs/content/documentation/start/getting-started.md)
- [Installation](docs/content/documentation/start/installation.md)
- [Tutorial](docs/content/documentation/start/tutorial.md)
- [Core Concepts](docs/content/documentation/learn/core-concepts.md)
- [Limitations](docs/content/documentation/evaluate/limitations.md)
- [Benchmarks](docs/content/documentation/evaluate/benchmarks.md)

Build the documentation site locally:

```bash
python3 docs/build.py
python3 docs/build.py --serve
```

## Operations Surface

The v0.1 local operations surface includes:

- `GET /livez`, `GET /healthz`, `GET /readyz`, `GET /metrics`, and `GET /info`
- `aiondb doctor --data-dir <path>`
- `aiondb upgrade --data-dir <path>`
- `aiondb dump` and `aiondb restore` (require `AIONDB_BOOTSTRAP_USER` and
  `AIONDB_BOOTSTRAP_PASSWORD` env vars; output and input paths are relative to
  `./backups/` in the current working directory, not to `--data-dir`)
- `make product-smoke`

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
