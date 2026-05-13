#!/usr/bin/env python3
"""Build and validate the JSON manifest for a local AionDB package."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


EXPECTED_PACKAGE_PATHS = {
    "aiondb/GOVERNANCE.md",
    "aiondb/COMMERCIAL-LICENSE.md",
    "aiondb/LICENSE",
    "aiondb/NOTICE",
    "aiondb/README.md",
    "aiondb/SECURITY.md",
    "aiondb/THIRD_PARTY_LICENSES.md",
    "aiondb/aiondb",
    "aiondb/integrations/README.md",
    "aiondb/integrations/psql-smoke.sql",
    "aiondb/packaging/INSTALL.md",
    "aiondb/packaging/README.md",
    "aiondb/packaging/kubernetes/aiondb.yaml",
    "aiondb/packaging/systemd/aiondb.env.example",
    "aiondb/packaging/systemd/aiondb.service",
}


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_MANIFEST_INPUT_BYTES = 4 * 1024 * 1024


def read_text_limited(path: Path, max_bytes: int = MAX_MANIFEST_INPUT_BYTES) -> str:
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


def read_key_value_manifest(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(read_text_limited(path).splitlines(), start=1):
        if not raw_line.strip():
            continue
        if "=" not in raw_line:
            raise ValueError(f"{path}:{line_number}: expected key=value")
        key, value = raw_line.split("=", 1)
        if not key:
            raise ValueError(f"{path}:{line_number}: empty key")
        if key in values:
            raise ValueError(f"{path}:{line_number}: duplicate key {key}")
        values[key] = value
    return values


def read_filelist(path: Path) -> list[dict[str, str]]:
    files: list[dict[str, str]] = []
    seen: set[str] = set()
    for line_number, raw_line in enumerate(read_text_limited(path).splitlines(), start=1):
        if not raw_line.strip():
            continue
        try:
            digest, package_path = raw_line.split(maxsplit=1)
        except ValueError as exc:
            raise ValueError(f"{path}:{line_number}: expected '<sha256> <path>'") from exc
        if not SHA256_RE.fullmatch(digest):
            raise ValueError(f"{path}:{line_number}: invalid sha256 digest")
        if not package_path.startswith("aiondb/"):
            raise ValueError(f"{path}:{line_number}: package path must start with aiondb/")
        if package_path in seen:
            raise ValueError(f"{path}:{line_number}: duplicate package path {package_path}")
        seen.add(package_path)
        files.append({"path": package_path, "sha256": digest})
    return sorted(files, key=lambda item: item["path"])


def build_manifest(legacy_manifest: Path, filelist: Path) -> dict[str, object]:
    values = read_key_value_manifest(legacy_manifest)
    files = read_filelist(filelist)
    package_paths = {entry["path"] for entry in files}
    if package_paths != EXPECTED_PACKAGE_PATHS:
        missing = EXPECTED_PACKAGE_PATHS - package_paths
        extra = package_paths - EXPECTED_PACKAGE_PATHS
        details = []
        if missing:
            details.append(f"missing expected package paths: {', '.join(sorted(missing))}")
        if extra:
            details.append(f"unexpected package paths: {', '.join(sorted(extra))}")
        raise ValueError("; ".join(details))
    archive_sha256 = values.get("archive_sha256", "")
    if not SHA256_RE.fullmatch(archive_sha256):
        raise ValueError("legacy manifest archive_sha256 is missing or invalid")
    dependency_inventory_sha256 = values.get("dependency_inventory_sha256", "")
    if not SHA256_RE.fullmatch(dependency_inventory_sha256):
        raise ValueError("legacy manifest dependency_inventory_sha256 is missing or invalid")
    spdx_sbom_sha256 = values.get("spdx_sbom_sha256", "")
    if not SHA256_RE.fullmatch(spdx_sbom_sha256):
        raise ValueError("legacy manifest spdx_sbom_sha256 is missing or invalid")
    return {
        "schema_version": 1,
        "name": values.get("name", ""),
        "version": values.get("version", ""),
        "commit": values.get("commit", ""),
        "worktree_dirty": values.get("worktree_dirty") == "true",
        "archive": {
            "path": values.get("archive", ""),
            "sha256": archive_sha256,
            "sha256_file": values.get("sha256_file", ""),
        },
        "content_sha256_file": values.get("filelist_sha256_file", ""),
        "dependency_inventory": {
            "path": values.get("dependency_inventory_file", ""),
            "sha256": dependency_inventory_sha256,
        },
        "spdx_sbom": {
            "path": values.get("spdx_sbom_file", ""),
            "sha256": spdx_sbom_sha256,
        },
        "legacy_manifest_file": str(legacy_manifest),
        "manifest_json_file": values.get("manifest_json_file", ""),
        "files": files,
    }


def validate_manifest(data: object) -> list[str]:
    errors: list[str] = []
    if not isinstance(data, dict):
        return ["manifest root must be an object"]
    if data.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    for key in ("name", "version", "commit", "content_sha256_file", "legacy_manifest_file"):
        if not isinstance(data.get(key), str) or not data.get(key):
            errors.append(f"{key} must be a non-empty string")
    archive = data.get("archive")
    if not isinstance(archive, dict):
        errors.append("archive must be an object")
    else:
        for key in ("path", "sha256", "sha256_file"):
            if not isinstance(archive.get(key), str) or not archive.get(key):
                errors.append(f"archive.{key} must be a non-empty string")
        digest = archive.get("sha256")
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append("archive.sha256 must be a sha256 digest")
    dependency_inventory = data.get("dependency_inventory")
    if not isinstance(dependency_inventory, dict):
        errors.append("dependency_inventory must be an object")
    else:
        for key in ("path", "sha256"):
            if not isinstance(dependency_inventory.get(key), str) or not dependency_inventory.get(key):
                errors.append(f"dependency_inventory.{key} must be a non-empty string")
        digest = dependency_inventory.get("sha256")
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append("dependency_inventory.sha256 must be a sha256 digest")
    spdx_sbom = data.get("spdx_sbom")
    if not isinstance(spdx_sbom, dict):
        errors.append("spdx_sbom must be an object")
    else:
        for key in ("path", "sha256"):
            if not isinstance(spdx_sbom.get(key), str) or not spdx_sbom.get(key):
                errors.append(f"spdx_sbom.{key} must be a non-empty string")
        digest = spdx_sbom.get("sha256")
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append("spdx_sbom.sha256 must be a sha256 digest")
    files = data.get("files")
    if not isinstance(files, list) or not files:
        errors.append("files must be a non-empty array")
        return errors
    paths: set[str] = set()
    for index, entry in enumerate(files):
        if not isinstance(entry, dict):
            errors.append(f"files[{index}] must be an object")
            continue
        package_path = entry.get("path")
        digest = entry.get("sha256")
        if not isinstance(package_path, str) or not package_path.startswith("aiondb/"):
            errors.append(f"files[{index}].path must start with aiondb/")
        elif package_path in paths:
            errors.append(f"duplicate file path {package_path}")
        else:
            paths.add(package_path)
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            errors.append(f"files[{index}].sha256 must be a sha256 digest")
    if paths != EXPECTED_PACKAGE_PATHS:
        missing = EXPECTED_PACKAGE_PATHS - paths
        extra = paths - EXPECTED_PACKAGE_PATHS
        if missing:
            errors.append(f"missing expected package paths: {', '.join(sorted(missing))}")
        if extra:
            errors.append(f"unexpected package paths: {', '.join(sorted(extra))}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--filelist", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--verify", action="store_true", help="verify an existing JSON manifest instead of writing it")
    args = parser.parse_args()

    try:
        if args.verify:
            data = json.loads(read_text_limited(args.out))
            expected = build_manifest(args.manifest, args.filelist)
            errors = validate_manifest(data)
            if data != expected:
                errors.append("JSON manifest does not match legacy manifest and filelist")
            if errors:
                for error in errors:
                    print(f"manifest validation failed: {error}", file=sys.stderr)
                return 1
            print("package JSON manifest validation ok")
            return 0

        manifest = build_manifest(args.manifest, args.filelist)
        errors = validate_manifest(manifest)
        if errors:
            for error in errors:
                print(f"manifest validation failed: {error}", file=sys.stderr)
            return 1
        args.out.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {args.out}")
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"package JSON manifest error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
