#!/usr/bin/env python3
"""Validate the local AionDB deployment profiles without Docker.

This is a policy check, not a full Dockerfile, Compose, or systemd parser. It
verifies the deployment contracts that matter for v0.1:

- pgwire is exposed on loopback host port 5432;
- observability stays on loopback and is not published by compose;
- Studio is exposed on loopback host port 8082;
- Dockerfile and compose health checks use /readyz;
- durable data is mounted at /var/lib/aiondb/data;
- the evaluation compose profile keeps TLS disabled and declares the local
  bootstrap user/password.
- Dockerfile and Compose images use explicit non-latest tags or digests;
- the default Compose profile pulls prebuilt release images, while the local
  build override owns Docker build contexts;
- .dockerignore excludes local secrets, agent state, and build/runtime outputs;
- Dockerfile publishes OCI image labels for registry/evaluator metadata;
- Kubernetes evaluation profile uses a StatefulSet, PVC, pgwire Service, and
  liveness/readiness probes without publishing observability;
- Kubernetes evaluation profile declares resource requests/limits and graceful
  termination settings;
- the systemd profile runs as the aiondb user with durable storage and basic
  service hardening.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DOCKERFILE = ROOT / "Dockerfile"
DOCKERIGNORE = ROOT / ".dockerignore"
COMPOSE = ROOT / "docker-compose.yml"
COMPOSE_BUILD = ROOT / "docker-compose.build.yml"
DOTENV_EXAMPLE = ROOT / ".env.example"
SYSTEMD_SERVICE = ROOT / "packaging" / "systemd" / "aiondb.service"
SYSTEMD_ENV_EXAMPLE = ROOT / "packaging" / "systemd" / "aiondb.env.example"
KUBERNETES_PROFILE = ROOT / "packaging" / "kubernetes" / "aiondb.yaml"
KUBERNETES_PRODUCTION_PROFILE = ROOT / "packaging" / "kubernetes" / "aiondb-production.yaml"
DOCKER_FROM_RE = re.compile(r"^FROM\s+([^\s]+)(?:\s+AS\s+\S+)?\s*$", re.IGNORECASE | re.MULTILINE)
COMPOSE_IMAGE_RE = re.compile(r"^\s*image:\s*([^\s#]+)\s*$", re.MULTILINE)
KUBERNETES_IMAGE_RE = re.compile(r"^\s*image:\s*([^\s#]+)\s*$", re.MULTILINE)
MAX_PROFILE_INPUT_BYTES = 4 * 1024 * 1024


def require(condition: bool, message: str, errors: list[str]) -> None:
    if not condition:
        errors.append(message)


def has_explicit_non_latest_reference(image: str) -> bool:
    if "@sha256:" in image:
        return True
    image_name = image.rsplit("/", 1)[-1]
    if ":" not in image_name:
        return False
    tag = image_name.rsplit(":", 1)[1]
    return bool(tag) and tag != "latest"


def read_text_limited(path: Path, max_bytes: int = MAX_PROFILE_INPUT_BYTES) -> str:
    if not path.is_file():
        raise ValueError(f"{path}: must be a regular file")
    size = path.stat().st_size
    if size > max_bytes:
        raise ValueError(f"{path}: exceeds maximum size of {max_bytes} bytes")
    with path.open("rb") as handle:
        data = handle.read(max_bytes + 1)
    if len(data) > max_bytes:
        raise ValueError(f"{path}: exceeds maximum size of {max_bytes} bytes")
    return data.decode("utf-8")


def main() -> int:
    errors: list[str] = []
    try:
        dockerfile = read_text_limited(DOCKERFILE)
        dockerignore = read_text_limited(DOCKERIGNORE)
        compose = read_text_limited(COMPOSE)
        compose_build = read_text_limited(COMPOSE_BUILD)
        dotenv_example = read_text_limited(DOTENV_EXAMPLE)
        systemd_service = read_text_limited(SYSTEMD_SERVICE)
        systemd_env = read_text_limited(SYSTEMD_ENV_EXAMPLE)
        kubernetes = read_text_limited(KUBERNETES_PROFILE)
        kubernetes_production = read_text_limited(KUBERNETES_PRODUCTION_PROFILE)
    except (OSError, ValueError, UnicodeDecodeError) as exc:
        print(f"deployment profile validation error: {exc}", file=sys.stderr)
        return 1

    dockerignore_patterns = {
        line.strip()
        for line in dockerignore.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }

    docker_base_images = DOCKER_FROM_RE.findall(dockerfile)
    require(bool(docker_base_images), "Dockerfile must declare at least one base image", errors)
    for image in docker_base_images:
        require(has_explicit_non_latest_reference(image),
                f"Dockerfile image {image} must use an explicit non-latest tag or sha256 digest", errors)

    compose_images = COMPOSE_IMAGE_RE.findall(compose)
    require(bool(compose_images), "compose must declare an image name", errors)
    for image in compose_images:
        require(has_explicit_non_latest_reference(image),
                f"compose image {image} must use an explicit non-latest tag or sha256 digest", errors)

    require("ENV AIONDB_PGWIRE_LISTEN_ADDR=0.0.0.0:5432" in dockerfile,
            "Dockerfile must listen for pgwire on 0.0.0.0:5432", errors)
    require("ENV AIONDB_OBSERVABILITY_BIND=127.0.0.1" in dockerfile,
            "Dockerfile must keep observability bound to loopback", errors)
    require("ENV AIONDB_BOOTSTRAP_USER=" not in dockerfile,
            "Dockerfile must not bake a bootstrap username into the image", errors)
    require("ENV AIONDB_BOOTSTRAP_PASSWORD=" not in dockerfile,
            "Dockerfile must not bake a bootstrap password into the image", errors)
    require("No default bootstrap credentials are baked in." in dockerfile,
            "Dockerfile must explain that bootstrap credentials are supplied by the operator", errors)
    require("ENV AIONDB_ALLOW_UNENCRYPTED_STORAGE=true" in dockerfile,
            "Dockerfile must make the local evaluation image runnable on ordinary filesystems", errors)
    require("ENV AIONDB_PGWIRE_TLS_MODE=disable" in dockerfile,
            "Dockerfile must disable pgwire TLS in the local evaluation image", errors)
    require("EXPOSE 5432" in dockerfile, "Dockerfile must expose pgwire port 5432", errors)
    require("EXPOSE 9187" not in dockerfile, "Dockerfile must not expose observability port 9187", errors)
    require("http://127.0.0.1:9187/readyz" in dockerfile,
            "Dockerfile healthcheck must use /readyz on loopback", errors)
    require('CMD ["--data-dir", "/var/lib/aiondb/data", "--storage-backend", "durable"]' in dockerfile,
            "Dockerfile default command must use durable storage data-dir", errors)
    for label in (
        'org.opencontainers.image.title="AionDB"',
        'org.opencontainers.image.description="Local evaluation image for the AionDB database server"',
        'org.opencontainers.image.licenses="BUSL-1.1"',
        'org.opencontainers.image.source="https://github.com/aiondb/aiondb"',
        'org.opencontainers.image.documentation="https://github.com/aiondb/aiondb/tree/main/packaging"',
    ):
        require(label in dockerfile, f"Dockerfile must declare OCI label {label}", errors)

    for pattern in (
        ".git",
        ".env",
        ".env.*",
        "!.env.example",
        ".agents",
        ".claude",
        ".codex*",
        ".ssh",
        "target",
        "backups",
        "node_modules",
    ):
        require(pattern in dockerignore_patterns, f".dockerignore must exclude {pattern}", errors)

    require("build:" not in compose,
            "default compose must use prebuilt images and must not build locally", errors)
    require("image: ${AIONDB_IMAGE:-ghcr.io/ayoubnabil/aiondb:main}" in compose,
            "compose must default to the prebuilt AionDB GHCR image", errors)
    require("image: ${AIONDB_STUDIO_IMAGE:-ghcr.io/ayoubnabil/aiondb-studio:main}" in compose,
            "compose must default to the prebuilt AionDB Studio GHCR image", errors)
    require("context: ." in compose_build,
            "local build compose override must build the AionDB image from the repository root", errors)
    require("context: ./integrations/aiondb-studio" in compose_build,
            "local build compose override must build the Studio image from integrations/aiondb-studio", errors)
    require("image: ${AIONDB_IMAGE:-aiondb:local}" in compose_build,
            "local build compose override must tag the local AionDB image", errors)
    require("image: ${AIONDB_STUDIO_IMAGE:-aiondb-studio:local}" in compose_build,
            "local build compose override must tag the local Studio image", errors)
    require('      - "127.0.0.1:${AIONDB_PGWIRE_PORT:-5432}:5432"' in compose,
            "compose must publish pgwire on loopback host port 5432 by default", errors)
    require("9187:9187" not in compose, "compose must not publish observability port 9187", errors)
    require('      - "127.0.0.1:${AIONDB_STUDIO_PORT:-8082}:8081"' in compose,
            "compose must publish Studio on loopback host port 8082 by default", errors)
    require("http://127.0.0.1:9187/readyz" in compose,
            "compose healthcheck must use /readyz on loopback", errors)
    require("AIONDB_PGWIRE_TLS_MODE: disable" in compose,
            "evaluation compose profile must disable pgwire TLS explicitly", errors)
    require("AIONDB_ALLOW_UNENCRYPTED_STORAGE: \"true\"" in compose,
            "evaluation compose profile must declare unencrypted storage override explicitly", errors)
    require("AIONDB_BOOTSTRAP_USER: ${AIONDB_BOOTSTRAP_USER:?Set AIONDB_BOOTSTRAP_USER in .env}" in compose,
            "compose must fail closed unless AIONDB_BOOTSTRAP_USER is set in .env", errors)
    require("AIONDB_BOOTSTRAP_PASSWORD: ${AIONDB_BOOTSTRAP_PASSWORD:?Set AIONDB_BOOTSTRAP_PASSWORD in .env}" in compose,
            "compose must fail closed unless AIONDB_BOOTSTRAP_PASSWORD is set in .env", errors)
    require("aiondb-data:/var/lib/aiondb/data" in compose,
            "compose must mount the durable data volume", errors)
    require("DevPassword42!" not in compose,
            "compose must not include the historical demo password", errors)

    require("AIONDB_BOOTSTRAP_USER=CHANGE_ME" in dotenv_example,
            ".env.example must force the operator to choose a bootstrap user", errors)
    require("AIONDB_BOOTSTRAP_PASSWORD=CHANGE_ME_TO_A_STRONG_PASSWORD" in dotenv_example,
            ".env.example must force the operator to choose a strong bootstrap password", errors)
    require(
        "AIONDB_STUDIO_DATABASE_URL=postgres://CHANGE_ME:CHANGE_ME_TO_A_STRONG_PASSWORD@aiondb:5432/default?sslmode=disable"
        in dotenv_example,
        ".env.example must show the Studio DSN that matches the chosen bootstrap credentials",
        errors,
    )
    require("DevPassword42!" not in dotenv_example,
            ".env.example must not include the historical demo password", errors)

    kubernetes_images = KUBERNETES_IMAGE_RE.findall(kubernetes)
    require(bool(kubernetes_images), "Kubernetes profile must declare an image", errors)
    for image in kubernetes_images:
        require(has_explicit_non_latest_reference(image),
                f"Kubernetes image {image} must use an explicit non-latest tag or sha256 digest", errors)
    require("kind: StatefulSet" in kubernetes,
            "Kubernetes profile must use a StatefulSet", errors)
    require("kind: Service" in kubernetes,
            "Kubernetes profile must expose pgwire through a Service", errors)
    require("kind: ConfigMap" in kubernetes,
            "Kubernetes profile must include a ConfigMap", errors)
    require("kind: Secret" in kubernetes,
            "Kubernetes profile must include a bootstrap Secret template", errors)
    require("volumeClaimTemplates:" in kubernetes,
            "Kubernetes profile must request persistent storage", errors)
    require("terminationGracePeriodSeconds: 30" in kubernetes,
            "Kubernetes profile must allow graceful termination", errors)
    require("updateStrategy:" in kubernetes and "type: RollingUpdate" in kubernetes,
            "Kubernetes profile must declare a rolling update strategy", errors)
    require("mountPath: /var/lib/aiondb/data" in kubernetes,
            "Kubernetes profile must mount the durable data directory", errors)
    require("port: 5432" in kubernetes and "targetPort: pgwire" in kubernetes,
            "Kubernetes Service must expose pgwire on port 5432", errors)
    require("port: 9187" not in kubernetes,
            "Kubernetes Service must not publish observability port 9187", errors)
    require("containerPort: 9187" in kubernetes,
            "Kubernetes profile must keep observability available for probes", errors)
    require("path: /readyz" in kubernetes,
            "Kubernetes readiness probe must use /readyz", errors)
    require("path: /livez" in kubernetes,
            "Kubernetes liveness probe must use /livez", errors)
    require("AIONDB_PGWIRE_LISTEN_ADDR: 0.0.0.0:5432" in kubernetes,
            "Kubernetes profile must bind pgwire inside the pod", errors)
    require("AIONDB_OBSERVABILITY_BIND: 0.0.0.0" in kubernetes,
            "Kubernetes profile must bind observability inside the pod for kubelet probes", errors)
    require("AIONDB_ALLOW_UNENCRYPTED_STORAGE: \"true\"" in kubernetes,
            "Kubernetes evaluation profile must declare unencrypted storage override explicitly", errors)
    require("AIONDB_PGWIRE_TLS_MODE: disable" in kubernetes,
            "Kubernetes evaluation profile must disable pgwire TLS explicitly", errors)
    require("CHANGE_ME_BEFORE_EVALUATION" in kubernetes and "DevPassword42!" not in kubernetes,
            "Kubernetes bootstrap Secret must use a placeholder instead of the compose demo password", errors)
    require("resources:" in kubernetes and "requests:" in kubernetes and "limits:" in kubernetes,
            "Kubernetes profile must declare resource requests and limits", errors)
    require("cpu: 250m" in kubernetes and "memory: 512Mi" in kubernetes,
            "Kubernetes profile must declare evaluation resource requests", errors)
    require("cpu: \"2\"" in kubernetes and "memory: 2Gi" in kubernetes,
            "Kubernetes profile must declare evaluation resource limits", errors)

    production_images = KUBERNETES_IMAGE_RE.findall(kubernetes_production)
    require(bool(production_images), "Kubernetes production-like profile must declare an image", errors)
    for image in production_images:
        require(
            has_explicit_non_latest_reference(image),
            f"Kubernetes production-like image {image} must use an explicit non-latest tag or sha256 digest",
            errors,
        )
    require("kind: StatefulSet" in kubernetes_production,
            "Kubernetes production-like profile must use a StatefulSet", errors)
    require("kind: Service" in kubernetes_production,
            "Kubernetes production-like profile must expose pgwire through a Service", errors)
    require("kind: ConfigMap" in kubernetes_production,
            "Kubernetes production-like profile must include a ConfigMap", errors)
    require("kind: Secret" in kubernetes_production,
            "Kubernetes production-like profile must include Secret templates", errors)
    require("volumeClaimTemplates:" in kubernetes_production,
            "Kubernetes production-like profile must request persistent storage", errors)
    require("AIONDB_PGWIRE_TLS_MODE: require" in kubernetes_production,
            "Kubernetes production-like profile must require pgwire TLS", errors)
    require("AIONDB_ALLOW_UNENCRYPTED_STORAGE" not in kubernetes_production,
            "Kubernetes production-like profile must not enable unencrypted storage", errors)
    require("AIONDB_PGWIRE_TLS_CERT_PATH: /etc/aiondb/tls/server.crt" in kubernetes_production,
            "Kubernetes production-like profile must mount a pgwire server certificate", errors)
    require("AIONDB_PGWIRE_TLS_KEY_PATH: /etc/aiondb/tls/server.key" in kubernetes_production,
            "Kubernetes production-like profile must mount a pgwire server key", errors)
    require("AIONDB_PGWIRE_TLS_CLIENT_CA_PATH: /etc/aiondb/tls/client-ca.crt" in kubernetes_production,
            "Kubernetes production-like profile must declare a client CA path", errors)
    require("secretName: aiondb-pgwire-tls" in kubernetes_production,
            "Kubernetes production-like profile must mount a TLS secret", errors)
    require("readOnly: true" in kubernetes_production,
            "Kubernetes production-like TLS mount must be read-only", errors)
    require("startupProbe:" in kubernetes_production,
            "Kubernetes production-like profile must declare a startup probe", errors)
    require("path: /readyz" in kubernetes_production and "path: /livez" in kubernetes_production,
            "Kubernetes production-like profile must use readiness and liveness probes", errors)
    require("REPLACE_WITH_BOOTSTRAP_ADMIN" in kubernetes_production,
            "Kubernetes production-like bootstrap Secret must use a non-demo placeholder username", errors)
    require("REPLACE_WITH_LONG_UNIQUE_PASSWORD" in kubernetes_production,
            "Kubernetes production-like bootstrap Secret must use a non-demo placeholder password", errors)
    require("REPLACE_WITH_PEM_CERT" in kubernetes_production,
            "Kubernetes production-like TLS Secret must use PEM placeholders", errors)
    require("DevPassword42!" not in kubernetes_production and "CHANGE_ME_BEFORE_EVALUATION" not in kubernetes_production,
            "Kubernetes production-like profile must not reuse evaluation placeholders", errors)
    require("port: 9187" not in kubernetes_production,
            "Kubernetes production-like Service must not publish observability port 9187", errors)

    require("User=aiondb" in systemd_service, "systemd service must run as the aiondb user", errors)
    require("Group=aiondb" in systemd_service, "systemd service must run as the aiondb group", errors)
    require("EnvironmentFile=-/etc/aiondb/aiondb.env" in systemd_service,
            "systemd service must support /etc/aiondb/aiondb.env overrides", errors)
    require("Environment=AIONDB_STORAGE_DATA_DIR=/var/lib/aiondb/data" in systemd_service,
            "systemd service must declare the durable data directory", errors)
    require("Environment=AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432" in systemd_service,
            "systemd service must bind pgwire to loopback by default", errors)
    require("Environment=AIONDB_OBSERVABILITY_BIND=127.0.0.1" in systemd_service,
            "systemd service must bind observability to loopback", errors)
    require("ExecStart=/usr/local/bin/aiondb --data-dir /var/lib/aiondb/data --storage-backend durable" in systemd_service,
            "systemd service must start durable storage on the declared data directory", errors)
    require("NoNewPrivileges=true" in systemd_service,
            "systemd service must enable NoNewPrivileges", errors)
    require("ProtectSystem=strict" in systemd_service,
            "systemd service must enable ProtectSystem=strict", errors)
    require("ReadWritePaths=/var/lib/aiondb" in systemd_service,
            "systemd service must restrict writable paths to /var/lib/aiondb", errors)

    require("AIONDB_STORAGE_BACKEND=durable" in systemd_env,
            "systemd env example must select durable storage", errors)
    require("AIONDB_STORAGE_DATA_DIR=/var/lib/aiondb/data" in systemd_env,
            "systemd env example must use the service data directory", errors)
    require("AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432" in systemd_env,
            "systemd env example must bind pgwire to loopback by default", errors)
    require("AIONDB_PGWIRE_TLS_MODE=require" in systemd_env,
            "systemd env example must require TLS by default", errors)
    require("# AIONDB_BOOTSTRAP_PASSWORD=" in systemd_env,
            "systemd env example must document bootstrap password as a commented value", errors)
    require("DevPassword42!" not in systemd_env,
            "systemd env example must not reuse the evaluation compose password", errors)
    require("# AIONDB_ALLOW_UNENCRYPTED_STORAGE=true" in systemd_env,
            "systemd env example must keep unencrypted storage override commented", errors)
    require("AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always" in systemd_env,
            "systemd env example must show the safest WAL commit policy", errors)

    if errors:
        print("deployment profile validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("deployment profile validation ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
