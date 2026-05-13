---
title: Installation
order: 11
---

# Installation

AionDB v0.1 is distributed as source, a release binary you can build locally, and prebuilt container images for local evaluation.

## Get the source

Every command on this page runs from inside a checkout of the AionDB repository. Clone it first:

```bash
git clone https://github.com/ayoubnabil/aiondb.git
cd aiondb
```

## Fastest Start

```bash
cp .env.example .env
$EDITOR .env
docker compose --profile studio up
```

Open AionDB Studio at `http://127.0.0.1:8082`. The Studio container connects
automatically to the `aiondb` service through pgwire.

For terminal access, use another shell:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"
```

Run the smoke SQL file:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable" \
  -f integrations/psql-smoke.sql
```

Stop the server:

```bash
docker compose down
```

Delete the local data volume:

```bash
docker compose down -v
```

The compose profile exposes PostgreSQL wire protocol on `5432` and AionDB
Studio on `8082`. It uses the bootstrap credentials you place in `.env`,
disables pgwire TLS, and enables unencrypted local storage so it works
immediately on developer machines. Replace every `CHANGE_ME` placeholder in
`.env`, including the Studio database URL, before first start.

By default Compose pulls prebuilt images from GitHub Container Registry:
`ghcr.io/ayoubnabil/aiondb:main` and
`ghcr.io/ayoubnabil/aiondb-studio:main`. Set `AIONDB_IMAGE` or
`AIONDB_STUDIO_IMAGE` in `.env` to use a pinned release tag, a SHA tag, or an
image published from a fork.

To run only the database server without the dashboard:

```bash
docker compose up
```

## Docker Without Compose

```bash
docker run --rm -it \
  -p 127.0.0.1:5432:5432 \
  -e AIONDB_BOOTSTRAP_USER=admin \
  -e AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
  -v aiondb-data:/var/lib/aiondb/data \
  ghcr.io/ayoubnabil/aiondb:main
```

## Build From Source

```bash
cargo build --release -p aiondb-server --bin aiondb
target/release/aiondb --version
```

Build local Docker images only when changing Dockerfiles or testing a fork that
has not published images yet:

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml --profile studio up --build
```

Run an in-memory server:

```bash
AIONDB_BOOTSTRAP_USER=admin \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
target/release/aiondb --ephemeral
```

Run with persistent local storage:

```bash
AIONDB_BOOTSTRAP_USER=admin \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
target/release/aiondb --data-dir ./data/aiondb --storage-backend durable
```

## Local Archive

Create a minimal binary archive:

```bash
make package-local
```

The archive lands under `target/` and contains the `aiondb` binary, public README and license files, governance and security notes, `integrations/psql-smoke.sql`, `packaging/INSTALL.md`, `packaging/kubernetes/aiondb.yaml`, `packaging/kubernetes/aiondb-production.yaml`, `packaging/systemd/aiondb.service`, and `packaging/systemd/aiondb.env.example`. It is meant for local evaluation and release-candidate checks, not a signed production distribution.

## Notes

Observability stays bound to loopback inside the container and is used by the container healthcheck through `/readyz`. This matches the v0.1 policy that observability should not be exposed directly.

For production-like tests, set explicit secrets, mount the data volume on encrypted storage, and remove `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true`.

## Kubernetes Profile

The local Kubernetes evaluation profile lives at
`packaging/kubernetes/aiondb.yaml`. A stricter production-like starting point
lives at `packaging/kubernetes/aiondb-production.yaml`. Both define a
single-replica StatefulSet, persistent volume claim, ConfigMap, Secret
templates, and ClusterIP Service for pgwire on `5432`. The evaluation profile
keeps TLS disabled and enables unencrypted local storage. The production-like
profile requires TLS, mounts explicit certificate material, and leaves
unencrypted storage disabled. Observability is used for `/readyz` and `/livez`
probes but is not published as a Service.

Before applying it, replace the Secret placeholder, review the image tag, and
review the storage class defaults. If you are testing an unpublished local
build, build or load `aiondb:local` into the cluster and update the StatefulSet
image accordingly.

## systemd Template

The service template lives at `packaging/systemd/aiondb.service`.

Before using it:

- create an `aiondb` system user;
- create `/var/lib/aiondb/data`;
- copy `packaging/systemd/aiondb.env.example` to `/etc/aiondb/aiondb.env`;
- restrict `/etc/aiondb/aiondb.env` permissions and replace secrets;
- decide whether persistent storage is on encrypted media;
- run `aiondb doctor --data-dir /var/lib/aiondb/data` before upgrades.

## Smoke Test

```bash
PGPASSWORD='ReplaceWithLongUniquePassword42!' \
psql "host=127.0.0.1 port=5432 dbname=default user=admin sslmode=disable" \
  -f integrations/psql-smoke.sql
```

If this fails, fix the server command, environment, or network path before testing graph, vector, ORM, or benchmark behavior.
