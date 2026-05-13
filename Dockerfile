# NOTE: pin the FROM tags by `@sha256:<digest>` for reproducible/supply-chain
# protected builds. See packaging/ for a release-pinned variant.
FROM rust:1.83-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release -p aiondb-server --bin aiondb

FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="AionDB" \
      org.opencontainers.image.description="Local evaluation image for the AionDB database server" \
      org.opencontainers.image.licenses="BUSL-1.1" \
      org.opencontainers.image.source="https://github.com/aiondb/aiondb" \
      org.opencontainers.image.documentation="https://github.com/aiondb/aiondb/tree/main/packaging"

RUN useradd --system --home /var/lib/aiondb --create-home --shell /usr/sbin/nologin aiondb \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini wget \
    && mkdir -p /var/lib/aiondb/data \
    && chown -R aiondb:aiondb /var/lib/aiondb \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/aiondb /usr/local/bin/aiondb

# SECURITY NOTE: this image is for LOCAL EVALUATION only.
#
#   * No default bootstrap credentials are baked in. Operators MUST provide
#     AIONDB_BOOTSTRAP_USER and AIONDB_BOOTSTRAP_PASSWORD at `docker run` time.
#   * The container binds pgwire on 0.0.0.0 inside the container so that
#     `docker run -p 5432:5432` can reach it from the host. Because TLS is not
#     auto-provisioned, AIONDB_ALLOW_PLAINTEXT_PUBLIC=1 is set so the server
#     starts. Plaintext credentials and queries on 5432 are NOT acceptable for
#     production. To harden: bind to 127.0.0.1 (drop the publish flag and use
#     `docker exec`), or mount TLS cert+key and set AIONDB_PGWIRE_TLS_MODE=require
#     with AIONDB_PGWIRE_TLS_CERT_PATH / AIONDB_PGWIRE_TLS_KEY_PATH.
#   * AIONDB_ALLOW_UNENCRYPTED_STORAGE=1 means data is written to disk
#     unencrypted; mount an encrypted volume in production-like deployments.
ENV AIONDB_PGWIRE_LISTEN_ADDR=0.0.0.0:5432
ENV AIONDB_OBSERVABILITY_BIND=127.0.0.1
ENV AIONDB_OBSERVABILITY_PORT=9187
ENV AIONDB_STORAGE_DATA_DIR=/var/lib/aiondb/data
ENV AIONDB_STORAGE_BACKEND=durable
ENV AIONDB_PGWIRE_TLS_MODE=disable
ENV AIONDB_ALLOW_PLAINTEXT_PUBLIC=1
ENV AIONDB_ALLOW_UNENCRYPTED_STORAGE=true

VOLUME ["/var/lib/aiondb/data"]
EXPOSE 5432

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 \
  CMD wget -q -O - http://127.0.0.1:9187/readyz >/dev/null || exit 1

USER aiondb
ENTRYPOINT ["tini", "--", "aiondb"]
CMD ["--data-dir", "/var/lib/aiondb/data", "--storage-backend", "durable"]
