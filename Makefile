PACKAGE_BASENAME := aiondb-local-$(shell uname -s)-$(shell uname -m)
PACKAGE_ARCHIVE := target/$(PACKAGE_BASENAME).tar.gz
PACKAGE_SHA256 := target/$(PACKAGE_BASENAME).tar.gz.sha256
PACKAGE_MANIFEST := target/$(PACKAGE_BASENAME).manifest
PACKAGE_JSON_MANIFEST := target/$(PACKAGE_BASENAME).manifest.json
PACKAGE_FILELIST := target/$(PACKAGE_BASENAME).files.sha256
DEPENDENCY_INVENTORY := target/$(PACKAGE_BASENAME).dependencies.json
SPDX_SBOM := target/$(PACKAGE_BASENAME).spdx.json
PACKAGE_REPRO_SHA256 := target/$(PACKAGE_BASENAME).repro.tar.gz.sha256
RELEASE_ARTIFACT_DIR := target/release-artifacts/$(PACKAGE_BASENAME)
RELEASE_VERIFY_DIR ?= $(RELEASE_ARTIFACT_DIR)
PACKAGE_CONTENTS := target/$(PACKAGE_BASENAME).contents
PACKAGE_EXTRACT_DIR := target/$(PACKAGE_BASENAME)-verify
PACKAGE_HELP := target/$(PACKAGE_BASENAME).help

.PHONY: all check test clippy fmt doc clean ci-policy dependency-inventory spdx-sbom package-local package-verify package-reproducible release-local release-verify product-smoke product-smoke-neo4j-p0 product-smoke-neo4j-browser-p0 docker-build docker-run deployment-validate docker-validate dashboard-studio pg-regress-safe pg-regress-oomsafe pg-regress-suite-safe pg-regress-resume-safe bench bench-pgbench bench-surreal-suite bench-tpch bench-job bench-tpcds bench-hybrid-fusion-micro

all: check test clippy fmt

check:
	cargo check --workspace --all-targets

test:
	cargo test --workspace --tests --exclude aiondb-engine
	AIONDB_PERF_BUDGET_MULTIPLIER=2 cargo test -p aiondb-engine --tests

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

clean:
	cargo clean

ci-policy:
	python3 scripts/check_github_actions_pins.py

dependency-inventory:
	python3 scripts/dependency_inventory.py --lockfile Cargo.lock --out $(DEPENDENCY_INVENTORY) --verify

spdx-sbom:
	python3 scripts/spdx_sbom.py --lockfile Cargo.lock --out $(SPDX_SBOM) --document-name $(PACKAGE_BASENAME) --verify

package-local: dependency-inventory spdx-sbom
	cargo build --release -p aiondb-server --bin aiondb
	rm -rf target/dist/aiondb
	mkdir -p target/dist/aiondb
	mkdir -p target/dist/aiondb/integrations
	mkdir -p target/dist/aiondb/packaging/systemd
	mkdir -p target/dist/aiondb/packaging/kubernetes
	cp target/release/aiondb target/dist/aiondb/
	cp README.md LICENSE COMMERCIAL-LICENSE.md NOTICE THIRD_PARTY_LICENSES.md SECURITY.md GOVERNANCE.md target/dist/aiondb/
	cp integrations/README.md integrations/psql-smoke.sql target/dist/aiondb/integrations/
	cp packaging/README.md packaging/INSTALL.md target/dist/aiondb/packaging/
	cp packaging/systemd/aiondb.service packaging/systemd/aiondb.env.example target/dist/aiondb/packaging/systemd/
	cp packaging/kubernetes/aiondb.yaml packaging/kubernetes/aiondb-production.yaml target/dist/aiondb/packaging/kubernetes/
	cd target/dist && find aiondb -type f -print0 | sort -z | xargs -0 sha256sum > ../$(PACKAGE_BASENAME).files.sha256
	tar -C target/dist --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner -cf - aiondb | gzip -n > $(PACKAGE_ARCHIVE)
	cd target && sha256sum $(PACKAGE_BASENAME).tar.gz > $(PACKAGE_BASENAME).tar.gz.sha256
	printf "name=%s\nversion=%s\ncommit=%s\narchive=%s\nsha256_file=%s\nfilelist_sha256_file=%s\nmanifest_json_file=%s\ndependency_inventory_file=%s\nspdx_sbom_file=%s\narchive_sha256=%s\ndependency_inventory_sha256=%s\nspdx_sbom_sha256=%s\n" \
		"$(PACKAGE_BASENAME)" \
		"$$(target/release/aiondb --version)" \
		"$$(git rev-parse --short HEAD 2>/dev/null || printf unknown)" \
		"$(PACKAGE_ARCHIVE)" \
		"$(PACKAGE_SHA256)" \
		"$(PACKAGE_FILELIST)" \
		"$(PACKAGE_JSON_MANIFEST)" \
		"$(DEPENDENCY_INVENTORY)" \
		"$(SPDX_SBOM)" \
		"$$(cut -d ' ' -f 1 $(PACKAGE_SHA256))" \
		"$$(sha256sum $(DEPENDENCY_INVENTORY) | cut -d ' ' -f 1)" \
		"$$(sha256sum $(SPDX_SBOM) | cut -d ' ' -f 1)" \
		> $(PACKAGE_MANIFEST)
	printf "worktree_dirty=%s\n" \
		"$$(if git status --porcelain --untracked-files=normal > target/$(PACKAGE_BASENAME).git-status 2>/dev/null && [ ! -s target/$(PACKAGE_BASENAME).git-status ]; then printf false; else printf true; fi)" \
		>> $(PACKAGE_MANIFEST)
	rm -f target/$(PACKAGE_BASENAME).git-status
	python3 scripts/package_manifest_json.py --manifest $(PACKAGE_MANIFEST) --filelist $(PACKAGE_FILELIST) --out $(PACKAGE_JSON_MANIFEST)

package-verify: package-local
	cd target && sha256sum -c $(PACKAGE_BASENAME).tar.gz.sha256
	test -s $(PACKAGE_MANIFEST)
	test -s $(PACKAGE_JSON_MANIFEST)
	test -s $(PACKAGE_FILELIST)
	grep -Fqx "archive_sha256=$$(cut -d ' ' -f 1 $(PACKAGE_SHA256))" $(PACKAGE_MANIFEST)
	grep -Fqx "filelist_sha256_file=$(PACKAGE_FILELIST)" $(PACKAGE_MANIFEST)
	grep -Fqx "manifest_json_file=$(PACKAGE_JSON_MANIFEST)" $(PACKAGE_MANIFEST)
	grep -Fqx "dependency_inventory_file=$(DEPENDENCY_INVENTORY)" $(PACKAGE_MANIFEST)
	grep -Fqx "dependency_inventory_sha256=$$(sha256sum $(DEPENDENCY_INVENTORY) | cut -d ' ' -f 1)" $(PACKAGE_MANIFEST)
	grep -Fqx "spdx_sbom_file=$(SPDX_SBOM)" $(PACKAGE_MANIFEST)
	grep -Fqx "spdx_sbom_sha256=$$(sha256sum $(SPDX_SBOM) | cut -d ' ' -f 1)" $(PACKAGE_MANIFEST)
	python3 scripts/package_manifest_json.py --manifest $(PACKAGE_MANIFEST) --filelist $(PACKAGE_FILELIST) --out $(PACKAGE_JSON_MANIFEST) --verify
	tar -tzf $(PACKAGE_ARCHIVE) > $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/aiondb $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/README.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/LICENSE $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/COMMERCIAL-LICENSE.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/NOTICE $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/THIRD_PARTY_LICENSES.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/SECURITY.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/GOVERNANCE.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/integrations/README.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/integrations/psql-smoke.sql $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/README.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/INSTALL.md $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/systemd/aiondb.service $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/systemd/aiondb.env.example $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/kubernetes/aiondb.yaml $(PACKAGE_CONTENTS)
	grep -Fqx aiondb/packaging/kubernetes/aiondb-production.yaml $(PACKAGE_CONTENTS)
	rm -f $(PACKAGE_CONTENTS)
	rm -rf $(PACKAGE_EXTRACT_DIR)
	mkdir -p $(PACKAGE_EXTRACT_DIR)
	tar -xzf $(PACKAGE_ARCHIVE) -C $(PACKAGE_EXTRACT_DIR)
	cd $(PACKAGE_EXTRACT_DIR) && sha256sum -c ../$(PACKAGE_BASENAME).files.sha256
	$(PACKAGE_EXTRACT_DIR)/aiondb/aiondb --version | grep -Fq "aiondb "
	grep -Fqx "version=$$($(PACKAGE_EXTRACT_DIR)/aiondb/aiondb --version)" $(PACKAGE_MANIFEST)
	$(PACKAGE_EXTRACT_DIR)/aiondb/aiondb --help > $(PACKAGE_HELP)
	grep -Fq -- "--ephemeral" $(PACKAGE_HELP)
	grep -Fq "doctor --data-dir" $(PACKAGE_HELP)
	grep -Fq "dump --data-dir" $(PACKAGE_HELP)
	grep -Fq "restore --data-dir" $(PACKAGE_HELP)
	grep -Fq "/livez, /healthz, /readyz, /metrics, /info" $(PACKAGE_HELP)
	rm -f $(PACKAGE_HELP)
	rm -rf $(PACKAGE_EXTRACT_DIR)

package-reproducible:
	$(MAKE) package-local
	cp $(PACKAGE_SHA256) $(PACKAGE_REPRO_SHA256)
	$(MAKE) package-local
	cmp -s $(PACKAGE_REPRO_SHA256) $(PACKAGE_SHA256)
	rm -f $(PACKAGE_REPRO_SHA256)

release-local: package-verify dependency-inventory spdx-sbom
	rm -rf $(RELEASE_ARTIFACT_DIR)
	mkdir -p $(RELEASE_ARTIFACT_DIR)
	cp $(PACKAGE_ARCHIVE) $(PACKAGE_SHA256) $(PACKAGE_FILELIST) $(PACKAGE_MANIFEST) $(PACKAGE_JSON_MANIFEST) $(DEPENDENCY_INVENTORY) $(SPDX_SBOM) $(RELEASE_ARTIFACT_DIR)/
	printf "AionDB local release artifacts\n\n" > $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Archive: %s\n" "$(PACKAGE_BASENAME).tar.gz" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Verify bundle checksums:\n  sha256sum -c SHA256SUMS\n\n" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Verify archive checksum:\n  sha256sum -c %s\n\n" "$(PACKAGE_BASENAME).tar.gz.sha256" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Inspect JSON manifest:\n  python3 -m json.tool %s\n\n" "$(PACKAGE_BASENAME).manifest.json" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Inspect dependency inventory:\n  python3 -m json.tool %s\n\n" "$(PACKAGE_BASENAME).dependencies.json" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Inspect SPDX SBOM:\n  python3 -m json.tool %s\n\n" "$(PACKAGE_BASENAME).spdx.json" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	printf "Verify extracted files:\n  mkdir -p verify\n  tar -xzf %s -C verify\n  cd verify && sha256sum -c ../%s\n" "$(PACKAGE_BASENAME).tar.gz" "$(PACKAGE_BASENAME).files.sha256" >> $(RELEASE_ARTIFACT_DIR)/README.txt
	cd $(RELEASE_ARTIFACT_DIR) && sha256sum \
		$(PACKAGE_BASENAME).tar.gz \
		$(PACKAGE_BASENAME).tar.gz.sha256 \
		$(PACKAGE_BASENAME).files.sha256 \
		$(PACKAGE_BASENAME).manifest \
		$(PACKAGE_BASENAME).manifest.json \
		$(PACKAGE_BASENAME).dependencies.json \
		$(PACKAGE_BASENAME).spdx.json \
		README.txt > SHA256SUMS
	cd $(RELEASE_ARTIFACT_DIR) && sha256sum -c SHA256SUMS
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).tar.gz
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).tar.gz.sha256
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).files.sha256
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).manifest
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).manifest.json
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).dependencies.json
	test -s $(RELEASE_ARTIFACT_DIR)/$(PACKAGE_BASENAME).spdx.json
	test -s $(RELEASE_ARTIFACT_DIR)/SHA256SUMS
	test -s $(RELEASE_ARTIFACT_DIR)/README.txt
	python3 scripts/verify_release_bundle.py $(RELEASE_ARTIFACT_DIR)

release-verify:
	python3 scripts/verify_release_bundle.py $(RELEASE_VERIFY_DIR)

product-smoke:
	cargo fmt --all -- --check
	cargo check --workspace
	$(MAKE) ci-policy
	cargo test -p aiondb-storage-engine storage_compat
	cargo test -p aiondb-server "route_"
	cargo test -p aiondb-parser parses_explain_format_json_select
	cargo test -p aiondb-parser parses_explain_analyze_format_json_select
	cargo test -p aiondb-engine explain_format_json_returns_single_json_payload_row
	cargo test -p aiondb-engine explain_analyze_format_json_returns_actual_graph_metrics
	cargo run -q -p xtask -- ecosystem-compat --group neo4j-http-p1 --no-history --report target/compat/neo4j-http-p1-smoke.json
	@if [ -n "$(AIONDB_NEO4J_JS_DRIVER_BASE)" ] && [ -n "$(AIONDB_NEO4J_JAVA_DRIVER_JAR)" ] && [ -n "$(AIONDB_CYPHER_SHELL)" ]; then \
		echo "running optional neo4j-p0 Bolt smoke"; \
		cargo run -q -p xtask -- ecosystem-compat --group neo4j-p0 --no-history --report target/compat/neo4j-p0-smoke.json; \
	else \
		echo "skipping optional neo4j-p0 Bolt smoke; set AIONDB_NEO4J_JS_DRIVER_BASE, AIONDB_NEO4J_JAVA_DRIVER_JAR, and AIONDB_CYPHER_SHELL to enable it"; \
	fi
	@if [ -n "$(AIONDB_CYPHER_SHELL)" ]; then \
		echo "running optional neo4j-browser-p0 preflight smoke"; \
		cargo run -q -p xtask -- ecosystem-compat --group neo4j-browser-p0 --no-history --report target/compat/neo4j-browser-p0-smoke.json; \
	else \
		echo "skipping optional neo4j-browser-p0 preflight smoke; set AIONDB_CYPHER_SHELL to enable it"; \
	fi
	cargo test -p aiondb-server --test cli_storage
	python3 docs/build.py --check-links
	$(MAKE) package-reproducible
	$(MAKE) release-local
	$(MAKE) deployment-validate

product-smoke-neo4j-p0:
	@if [ -z "$(AIONDB_NEO4J_JS_DRIVER_BASE)" ] || [ -z "$(AIONDB_NEO4J_JAVA_DRIVER_JAR)" ] || [ -z "$(AIONDB_CYPHER_SHELL)" ]; then \
		echo "product-smoke-neo4j-p0 requires AIONDB_NEO4J_JS_DRIVER_BASE, AIONDB_NEO4J_JAVA_DRIVER_JAR, and AIONDB_CYPHER_SHELL"; \
		exit 1; \
	fi
	cargo run -q -p xtask -- ecosystem-compat --group neo4j-p0 --no-history --report target/compat/neo4j-p0-smoke.json

product-smoke-neo4j-browser-p0:
	@if [ -z "$(AIONDB_CYPHER_SHELL)" ]; then \
		echo "product-smoke-neo4j-browser-p0 requires AIONDB_CYPHER_SHELL"; \
		exit 1; \
	fi
	cargo run -q -p xtask -- ecosystem-compat --group neo4j-browser-p0 --no-history --report target/compat/neo4j-browser-p0-smoke.json

docker-build:
	docker compose -f docker-compose.yml -f docker-compose.build.yml build

docker-run:
	docker compose up aiondb

deployment-validate:
	python3 scripts/validate_container_profile.py
	@if command -v docker >/dev/null 2>&1; then \
		docker compose --env-file .env.example config >/dev/null; \
		docker compose --env-file .env.example -f docker-compose.yml -f docker-compose.build.yml config >/dev/null; \
	else \
		echo "docker not found; skipped docker compose config"; \
	fi

docker-validate: deployment-validate

dashboard-studio:
	cd integrations/aiondb-studio && go run . --sessions --skip-open --bind 127.0.0.1 --listen 8082 --no-ssh --query-timeout 60 --queries-dir ./aiondb_queries

pg-regress-safe:
	./scripts/run_pg_regress_full_oomsafe.sh

pg-regress-oomsafe:
	./scripts/run_pg_regress_full_oomsafe.sh

pg-regress-suite-safe:
	@if [ -z "$(SUITE)" ]; then \
		echo "Usage: make pg-regress-suite-safe SUITE=<suite-name>"; \
		exit 1; \
	fi
	./scripts/run_pg_regress_suite_guarded.sh "$(SUITE)"

pg-regress-resume-safe:
	@if [ -z "$(RUN)" ]; then \
		echo "Usage: make pg-regress-resume-safe RUN=<previous-run-id-or-dir>"; \
		exit 1; \
	fi
	./scripts/resume_pg_regress_guarded.sh "$(RUN)"

# Performance benchmark harness (AionDB vs PostgreSQL).
# See benchmarks/run.sh --help for all tunables. Override engines with
# BENCH_ENGINES="aiondb pg".
bench: bench-pgbench

bench-pgbench:
	./benchmarks/run.sh pgbench

bench-surreal-suite:
	./benchmarks/run.sh surreal-suite

bench-tpch:
	./benchmarks/run.sh tpch

bench-job:
	./benchmarks/run.sh job

bench-tpcds:
	./benchmarks/run.sh tpcds

bench-hybrid-fusion-micro:
	cargo xtask hybrid-fusion-microbench
