# AionDB Packaging

This directory contains operator-facing packaging templates. They are intentionally small and local-first because v0.1 is an alpha release line.

## Local binary archive

Build a release binary and archive the minimal public files:

```bash
make package-local
```

The archive is written under `target/` and includes:

- `aiondb`;
- `README.md`;
- `LICENSE`;
- `COMMERCIAL-LICENSE.md`;
- `NOTICE`;
- `THIRD_PARTY_LICENSES.md`;
- `SECURITY.md`;
- `GOVERNANCE.md`;
- `integrations/README.md`;
- `integrations/psql-smoke.sql`;
- `packaging/INSTALL.md`;
- `packaging/README.md`;
- `packaging/kubernetes/aiondb.yaml`;
- `packaging/kubernetes/aiondb-production.yaml`;
- `packaging/systemd/aiondb.service`;
- `packaging/systemd/aiondb.env.example`.

The tarball normalizes file order, owner/group metadata, mtimes, and gzip
headers so the archive is reproducible for the same package inputs.

It also writes:

- `target/aiondb-local-<os>-<arch>.tar.gz.sha256`;
- `target/aiondb-local-<os>-<arch>.files.sha256`;
- `target/aiondb-local-<os>-<arch>.manifest`;
- `target/aiondb-local-<os>-<arch>.manifest.json`;
- `target/aiondb-local-<os>-<arch>.spdx.json`.

Generate the deterministic dependency inventory from `Cargo.lock`:

```bash
make dependency-inventory
```

Generate the deterministic SPDX JSON SBOM from `Cargo.lock`:

```bash
make spdx-sbom
```

Verify the local archive and checksum:

```bash
make package-verify
```

Check that the archive is reproducible for unchanged inputs:

```bash
make package-reproducible
```

The text manifest records the archive name, `aiondb --version` output, git commit if available, worktree dirty status, archive path, checksum file path, content checksum file path, JSON manifest path, dependency inventory path, SPDX SBOM path, inline archive SHA256, inline dependency inventory SHA256, and inline SPDX SBOM SHA256. The JSON manifest mirrors that metadata, links to the dependency inventory and SPDX SBOM, and lists each packaged file with its SHA256 digest for release tooling.

`make package-verify` checks that the text manifest, JSON manifest, archive checksum, content checksum file, and archive contents agree. It also extracts the archive, verifies every file against `target/aiondb-local-<os>-<arch>.files.sha256`, and verifies the packaged `aiondb --version` and `aiondb --help` first-run surface. These files are not signed provenance yet; they are the minimum local evidence needed before adding signing.

Collect the verified files into a publication directory:

```bash
make release-local
```

This writes `target/release-artifacts/aiondb-local-<os>-<arch>/` with the
archive, archive checksum, content checksum file, text manifest, JSON manifest,
dependency inventory, SPDX SBOM, `SHA256SUMS`, and a short `README.txt` with
verification commands.

Verify an existing release artifact directory:

```bash
make release-verify RELEASE_VERIFY_DIR=target/release-artifacts/aiondb-local-<os>-<arch>
```

This runs `scripts/verify_release_bundle.py`, which checks the bundle file set,
`SHA256SUMS`, archive checksum, legacy manifest, JSON manifest, dependency
inventory, SPDX SBOM, content checksum file, and the tarball's actual file
digests.

Validate the deployment profiles:

```bash
make deployment-validate
```

This always runs the static AionDB Docker, Compose, Kubernetes, and systemd
policy check. It also rejects untagged or `latest` Docker/Compose/Kubernetes
image references so local release candidates do not silently drift across
builds. When Docker is available, it also runs `docker compose config`.
`make docker-validate` is kept as a compatibility alias.

Run the local product-surface smoke gate:

```bash
make product-smoke
```

This is the shortest release-candidate check that exercises docs, local docs
links, packaging, GitHub Actions pin policy, deployment profile validation,
storage compatibility, observability routes, CLI dump/restore, formatting,
workspace compilation, and the local release artifact bundle. It also runs
`make package-reproducible`.

## Container

Start AionDB with the browser dashboard:

```bash
cp .env.example .env
$EDITOR .env
docker compose --profile studio up
```

Open AionDB Studio at `http://127.0.0.1:8082`. It connects automatically to
the bundled service.

Connect from a terminal with:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"
```

The image also runs without Compose:

```bash
docker run --rm -it \
  -p 127.0.0.1:5432:5432 \
  -e AIONDB_BOOTSTRAP_USER=admin \
  -e AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
  -v aiondb-data:/var/lib/aiondb/data \
  ghcr.io/ayoubnabil/aiondb:main
```

Stop the Compose server:

```bash
docker compose down
```

Delete the local data volume:

```bash
docker compose down -v
```

The compose profile exposes AionDB on PostgreSQL wire port `5432` and AionDB
Studio on `8082`. Observability stays on loopback inside the container and
drives the container healthcheck through `/readyz`. The local image requires
the bootstrap user and password to come from `.env`, disables pgwire TLS, and
sets `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true` for local evaluation only. Compose
pulls `ghcr.io/ayoubnabil/aiondb:main` and
`ghcr.io/ayoubnabil/aiondb-studio:main` by default. Override `AIONDB_IMAGE` or
`AIONDB_STUDIO_IMAGE` in `.env` to use a pinned release tag, SHA tag, or fork
image. For production-like tests, set explicit secrets, mount
`/var/lib/aiondb/data` on encrypted storage, and remove that override.

To run only the database server without the dashboard:

```bash
docker compose up
```

Build local images only when changing Dockerfiles or testing unpublished fork
images:

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml --profile studio up --build
```

## systemd

`systemd/aiondb.service` is a starting point for a single-node Linux service. Copy `systemd/aiondb.env.example` to `/etc/aiondb/aiondb.env`, restrict permissions, replace secrets, and review the data directory, user, TLS, and storage encryption policy before using it outside a lab.

## Kubernetes

`kubernetes/aiondb.yaml` is a single-node evaluation profile for local
clusters. `kubernetes/aiondb-production.yaml` is a stricter production-like
starting point for single-node deployments that require TLS and mounted
certificate material. Both define a ConfigMap, Secret templates, ClusterIP
Service for pgwire on `5432`, and StatefulSet with a persistent volume claim.
The Service does not publish observability; kubelet probes use `/readyz` and
`/livez` on the pod's observability port. Replace the Secret placeholders
before applying either profile.
