# AionDB Local Package Install Notes

These notes apply to the local archive produced by `make package-local`.

## Inspect the Artifact

```bash
tar -tzf aiondb-local-<os>-<arch>.tar.gz
sha256sum -c aiondb-local-<os>-<arch>.tar.gz.sha256
```

If the archive came from `make release-local`, verify the whole artifact bundle
first:

```bash
sha256sum -c SHA256SUMS
```

From a source checkout, the same bundle can be checked with the stricter
release verifier:

```bash
make release-verify RELEASE_VERIFY_DIR=/path/to/aiondb-local-<os>-<arch>
```

If `aiondb-local-<os>-<arch>.manifest.json` is available, inspect the release
metadata:

```bash
python3 -m json.tool aiondb-local-<os>-<arch>.manifest.json
```

If `aiondb-local-<os>-<arch>.dependencies.json` is available, inspect the
dependency inventory generated from `Cargo.lock`:

```bash
python3 -m json.tool aiondb-local-<os>-<arch>.dependencies.json
```

If `aiondb-local-<os>-<arch>.spdx.json` is available, inspect the SPDX SBOM
generated from `Cargo.lock`:

```bash
python3 -m json.tool aiondb-local-<os>-<arch>.spdx.json
```

If `aiondb-local-<os>-<arch>.files.sha256` is available, verify extracted
contents:

```bash
mkdir -p verify
tar -xzf aiondb-local-<os>-<arch>.tar.gz -C verify
cd verify
sha256sum -c ../aiondb-local-<os>-<arch>.files.sha256
```

## Run Locally

```bash
export AIONDB_BOOTSTRAP_USER=admin
export AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!'
./aiondb/aiondb --ephemeral
```

In another shell:

```bash
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=5432 dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable" \
  -f aiondb/integrations/psql-smoke.sql
```

## systemd Evaluation

Review the service and environment template before installing them:

```bash
less aiondb/packaging/systemd/aiondb.service
less aiondb/packaging/systemd/aiondb.env.example
```

For a Linux service evaluation:

1. create an `aiondb` system user and group;
2. create `/var/lib/aiondb/data`;
3. copy `aiondb` to `/usr/local/bin/aiondb`;
4. copy `aiondb/packaging/systemd/aiondb.service` to `/etc/systemd/system/aiondb.service`;
5. copy `aiondb/packaging/systemd/aiondb.env.example` to `/etc/aiondb/aiondb.env`;
6. replace placeholders and restrict `/etc/aiondb/aiondb.env` permissions;
7. install TLS files if `AIONDB_PGWIRE_TLS_MODE=require`;
8. run `aiondb doctor --data-dir /var/lib/aiondb/data` before upgrades.

The packaged systemd profile is conservative by default: pgwire and
observability bind to loopback, TLS is required in the environment template,
and the unencrypted-storage override is commented out.

## Kubernetes Evaluation

Review the single-node Kubernetes profile before applying it:

```bash
less aiondb/packaging/kubernetes/aiondb.yaml
less aiondb/packaging/kubernetes/aiondb-production.yaml
```

For a local cluster evaluation:

1. review the image tag, or build and load `aiondb:local` if you are testing unpublished local changes;
2. replace `CHANGE_ME_BEFORE_EVALUATION` in the Secret template;
3. review the storage request and class defaults;
4. apply `aiondb/packaging/kubernetes/aiondb.yaml`;
5. connect through the `aiondb` Service on pgwire port `5432`.

The Service publishes only pgwire. Readiness and liveness probes use `/readyz`
and `/livez` on the pod observability port, which is not exposed as a Service.
For a production-like single-node deployment, start from
`aiondb/packaging/kubernetes/aiondb-production.yaml` instead. It requires
bootstrap placeholders to be replaced, TLS PEM material to be mounted from a
Secret, and keeps the unencrypted-storage override disabled.
