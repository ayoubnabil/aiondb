#!/usr/bin/env python3
"""Verify a local AionDB release artifact bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tarfile
from pathlib import Path, PurePosixPath


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
    "aiondb/packaging/kubernetes/aiondb-production.yaml",
    "aiondb/packaging/kubernetes/aiondb.yaml",
    "aiondb/packaging/systemd/aiondb.env.example",
    "aiondb/packaging/systemd/aiondb.service",
}


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_BUNDLE_METADATA_BYTES = 16 * 1024 * 1024
MAX_ARCHIVE_MEMBER_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_TOTAL_BYTES = 1024 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 2048
MAX_BUNDLE_FILES = 32


def read_text_limited(path: Path, max_bytes: int = MAX_BUNDLE_METADATA_BYTES) -> str:
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


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_sha256_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(read_text_limited(path).splitlines(), start=1):
        if not raw_line.strip():
            continue
        try:
            digest, filename = raw_line.split(maxsplit=1)
        except ValueError as exc:
            raise ValueError(f"{path}:{line_number}: expected '<sha256> <filename>'") from exc
        filename = filename.removeprefix("*")
        if not SHA256_RE.fullmatch(digest):
            raise ValueError(f"{path}:{line_number}: invalid sha256 digest")
        if filename in values:
            raise ValueError(f"{path}:{line_number}: duplicate filename {filename}")
        values[filename] = digest
    return values


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


def read_filelist(path: Path) -> dict[str, str]:
    values = read_sha256_file(path)
    package_paths = set(values)
    if package_paths != EXPECTED_PACKAGE_PATHS:
        missing = EXPECTED_PACKAGE_PATHS - package_paths
        extra = package_paths - EXPECTED_PACKAGE_PATHS
        details = []
        if missing:
            details.append(f"missing package paths: {', '.join(sorted(missing))}")
        if extra:
            details.append(f"unexpected package paths: {', '.join(sorted(extra))}")
        raise ValueError("; ".join(details))
    return values


def validate_archive_member_name(path: Path, name: str) -> None:
    parsed = PurePosixPath(name)
    raw_parts = name.split("/")
    if (
        not name
        or "\\" in name
        or parsed.is_absolute()
        or any(part in {"", ".", ".."} for part in raw_parts)
        or any(part in {"", ".", ".."} for part in parsed.parts)
    ):
        raise ValueError(f"{path}: unsafe archive member path {name!r}")
    if parsed.parts[0] != "aiondb":
        raise ValueError(f"{path}: archive member path must start with aiondb/: {name}")


def validate_archive_member_size(path: Path, name: str, size: int, current_total: int) -> None:
    if size < 0:
        raise ValueError(f"{path}: archive member has invalid size for {name}")
    if size > MAX_ARCHIVE_MEMBER_BYTES:
        raise ValueError(
            f"{path}: archive member {name} exceeds maximum size of {MAX_ARCHIVE_MEMBER_BYTES} bytes"
        )
    if current_total + size > MAX_ARCHIVE_TOTAL_BYTES:
        raise ValueError(f"{path}: archive contents exceed maximum size of {MAX_ARCHIVE_TOTAL_BYTES} bytes")


def tar_file_digests(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    seen_members: set[str] = set()
    total_size = 0
    member_count = 0
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            member_count += 1
            if member_count > MAX_ARCHIVE_MEMBERS:
                raise ValueError(f"{path}: archive exceeds maximum member count of {MAX_ARCHIVE_MEMBERS}")
            validate_archive_member_name(path, member.name)
            if member.name in seen_members:
                raise ValueError(f"{path}: duplicate archive path {member.name}")
            seen_members.add(member.name)
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError(f"{path}: unsupported archive member type for {member.name}")
            validate_archive_member_size(path, member.name, member.size, total_size)
            total_size += member.size
            extracted = archive.extractfile(member)
            if extracted is None:
                raise ValueError(f"{path}: could not read {member.name}")
            digest = hashlib.sha256()
            for chunk in iter(lambda: extracted.read(1024 * 1024), b""):
                digest.update(chunk)
            values[member.name] = digest.hexdigest()
    return values


def basename_from_manifest_value(value: str) -> str:
    return Path(value).name


def bundle_file_names(bundle_dir: Path) -> set[str]:
    names: set[str] = set()
    for path in bundle_dir.iterdir():
        if not path.is_file():
            continue
        names.add(path.name)
        if len(names) > MAX_BUNDLE_FILES:
            raise ValueError(f"{bundle_dir}: exceeds maximum bundle file count of {MAX_BUNDLE_FILES}")
    return names


def verify_bundle(bundle_dir: Path) -> None:
    if not bundle_dir.is_dir():
        raise ValueError(f"{bundle_dir} is not a directory")

    basename = bundle_dir.name
    archive_name = f"{basename}.tar.gz"
    archive_sha_name = f"{archive_name}.sha256"
    filelist_name = f"{basename}.files.sha256"
    legacy_manifest_name = f"{basename}.manifest"
    json_manifest_name = f"{basename}.manifest.json"
    dependency_inventory_name = f"{basename}.dependencies.json"
    spdx_sbom_name = f"{basename}.spdx.json"
    expected_bundle_files = {
        archive_name,
        archive_sha_name,
        filelist_name,
        legacy_manifest_name,
        json_manifest_name,
        dependency_inventory_name,
        spdx_sbom_name,
        "README.txt",
        "SHA256SUMS",
    }
    actual_bundle_files = bundle_file_names(bundle_dir)
    if actual_bundle_files != expected_bundle_files:
        missing = expected_bundle_files - actual_bundle_files
        extra = actual_bundle_files - expected_bundle_files
        details = []
        if missing:
            details.append(f"missing bundle files: {', '.join(sorted(missing))}")
        if extra:
            details.append(f"unexpected bundle files: {', '.join(sorted(extra))}")
        raise ValueError("; ".join(details))

    bundle_sums = read_sha256_file(bundle_dir / "SHA256SUMS")
    if set(bundle_sums) != expected_bundle_files - {"SHA256SUMS"}:
        raise ValueError("SHA256SUMS entries do not match release bundle files")
    for filename, expected_digest in bundle_sums.items():
        actual_digest = sha256_file(bundle_dir / filename)
        if actual_digest != expected_digest:
            raise ValueError(f"{filename}: SHA256SUMS digest mismatch")

    archive_path = bundle_dir / archive_name
    archive_digest = sha256_file(archive_path)
    archive_sums = read_sha256_file(bundle_dir / archive_sha_name)
    if archive_sums != {archive_name: archive_digest}:
        raise ValueError(f"{archive_sha_name}: archive digest mismatch")

    legacy_manifest = read_key_value_manifest(bundle_dir / legacy_manifest_name)
    if legacy_manifest.get("name") != basename:
        raise ValueError("legacy manifest name does not match bundle directory")
    if basename_from_manifest_value(legacy_manifest.get("archive", "")) != archive_name:
        raise ValueError("legacy manifest archive path does not match bundle archive")
    if basename_from_manifest_value(legacy_manifest.get("sha256_file", "")) != archive_sha_name:
        raise ValueError("legacy manifest sha256_file does not match bundle archive checksum")
    if basename_from_manifest_value(legacy_manifest.get("filelist_sha256_file", "")) != filelist_name:
        raise ValueError("legacy manifest filelist_sha256_file does not match bundle filelist")
    if basename_from_manifest_value(legacy_manifest.get("manifest_json_file", "")) != json_manifest_name:
        raise ValueError("legacy manifest manifest_json_file does not match bundle JSON manifest")
    if basename_from_manifest_value(legacy_manifest.get("dependency_inventory_file", "")) != dependency_inventory_name:
        raise ValueError("legacy manifest dependency_inventory_file does not match bundle dependency inventory")
    if basename_from_manifest_value(legacy_manifest.get("spdx_sbom_file", "")) != spdx_sbom_name:
        raise ValueError("legacy manifest spdx_sbom_file does not match bundle SPDX SBOM")
    if legacy_manifest.get("archive_sha256") != archive_digest:
        raise ValueError("legacy manifest archive_sha256 does not match archive digest")
    dependency_inventory_digest = sha256_file(bundle_dir / dependency_inventory_name)
    if legacy_manifest.get("dependency_inventory_sha256") != dependency_inventory_digest:
        raise ValueError("legacy manifest dependency_inventory_sha256 does not match dependency inventory digest")
    spdx_sbom_digest = sha256_file(bundle_dir / spdx_sbom_name)
    if legacy_manifest.get("spdx_sbom_sha256") != spdx_sbom_digest:
        raise ValueError("legacy manifest spdx_sbom_sha256 does not match SPDX SBOM digest")

    filelist = read_filelist(bundle_dir / filelist_name)
    archive_files = tar_file_digests(archive_path)
    if archive_files != filelist:
        raise ValueError("archive contents do not match filelist SHA256 entries")

    json_manifest = json.loads(read_text_limited(bundle_dir / json_manifest_name))
    if not isinstance(json_manifest, dict):
        raise ValueError("JSON manifest root must be an object")
    archive = json_manifest.get("archive")
    if not isinstance(archive, dict):
        raise ValueError("JSON manifest archive must be an object")
    if json_manifest.get("schema_version") != 1:
        raise ValueError("JSON manifest schema_version must be 1")
    if json_manifest.get("name") != basename:
        raise ValueError("JSON manifest name does not match bundle directory")
    if json_manifest.get("version") != legacy_manifest.get("version"):
        raise ValueError("JSON manifest version does not match legacy manifest")
    if json_manifest.get("commit") != legacy_manifest.get("commit"):
        raise ValueError("JSON manifest commit does not match legacy manifest")
    if json_manifest.get("worktree_dirty") != (legacy_manifest.get("worktree_dirty") == "true"):
        raise ValueError("JSON manifest worktree_dirty does not match legacy manifest")
    if basename_from_manifest_value(str(archive.get("path", ""))) != archive_name:
        raise ValueError("JSON manifest archive path does not match bundle archive")
    if archive.get("sha256") != archive_digest:
        raise ValueError("JSON manifest archive sha256 does not match archive digest")
    if basename_from_manifest_value(str(archive.get("sha256_file", ""))) != archive_sha_name:
        raise ValueError("JSON manifest archive sha256_file does not match bundle archive checksum")
    if basename_from_manifest_value(str(json_manifest.get("content_sha256_file", ""))) != filelist_name:
        raise ValueError("JSON manifest content_sha256_file does not match bundle filelist")
    if basename_from_manifest_value(str(json_manifest.get("legacy_manifest_file", ""))) != legacy_manifest_name:
        raise ValueError("JSON manifest legacy_manifest_file does not match bundle manifest")
    if basename_from_manifest_value(str(json_manifest.get("manifest_json_file", ""))) != json_manifest_name:
        raise ValueError("JSON manifest manifest_json_file does not match bundle JSON manifest")
    dependency_inventory_ref = json_manifest.get("dependency_inventory")
    if not isinstance(dependency_inventory_ref, dict):
        raise ValueError("JSON manifest dependency_inventory must be an object")
    if basename_from_manifest_value(str(dependency_inventory_ref.get("path", ""))) != dependency_inventory_name:
        raise ValueError("JSON manifest dependency_inventory path does not match bundle dependency inventory")
    if dependency_inventory_ref.get("sha256") != dependency_inventory_digest:
        raise ValueError("JSON manifest dependency_inventory sha256 does not match dependency inventory digest")
    spdx_sbom_ref = json_manifest.get("spdx_sbom")
    if not isinstance(spdx_sbom_ref, dict):
        raise ValueError("JSON manifest spdx_sbom must be an object")
    if basename_from_manifest_value(str(spdx_sbom_ref.get("path", ""))) != spdx_sbom_name:
        raise ValueError("JSON manifest spdx_sbom path does not match bundle SPDX SBOM")
    if spdx_sbom_ref.get("sha256") != spdx_sbom_digest:
        raise ValueError("JSON manifest spdx_sbom sha256 does not match SPDX SBOM digest")

    json_files = json_manifest.get("files")
    if not isinstance(json_files, list):
        raise ValueError("JSON manifest files must be an array")
    json_filelist: dict[str, str] = {}
    for index, entry in enumerate(json_files):
        if not isinstance(entry, dict):
            raise ValueError(f"JSON manifest files[{index}] must be an object")
        package_path = entry.get("path")
        digest = entry.get("sha256")
        if not isinstance(package_path, str) or not isinstance(digest, str):
            raise ValueError(f"JSON manifest files[{index}] must contain path and sha256 strings")
        if package_path in json_filelist:
            raise ValueError(f"JSON manifest files[{index}] duplicates {package_path}")
        json_filelist[package_path] = digest
    if json_filelist != filelist:
        raise ValueError("JSON manifest files do not match filelist SHA256 entries")

    dependency_inventory = json.loads(read_text_limited(bundle_dir / dependency_inventory_name))
    if not isinstance(dependency_inventory, dict):
        raise ValueError("dependency inventory root must be an object")
    if dependency_inventory.get("schema_version") != 1:
        raise ValueError("dependency inventory schema_version must be 1")
    if dependency_inventory.get("lockfile") != "Cargo.lock":
        raise ValueError("dependency inventory lockfile must be Cargo.lock")
    packages = dependency_inventory.get("packages")
    if not isinstance(packages, list) or not packages:
        raise ValueError("dependency inventory packages must be a non-empty array")
    if dependency_inventory.get("package_count") != len(packages):
        raise ValueError("dependency inventory package_count must match packages length")
    workspace_count = 0
    external_count = 0
    previous_key: tuple[str, str, str] | None = None
    seen_packages: set[tuple[str, str, str]] = set()
    for index, package in enumerate(packages):
        if not isinstance(package, dict):
            raise ValueError(f"dependency inventory packages[{index}] must be an object")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        dependency_count = package.get("dependency_count")
        if not isinstance(name, str) or not name:
            raise ValueError(f"dependency inventory packages[{index}].name must be a non-empty string")
        if not isinstance(version, str) or not version:
            raise ValueError(f"dependency inventory packages[{index}].version must be a non-empty string")
        if not isinstance(source, str) or not source:
            raise ValueError(f"dependency inventory packages[{index}].source must be a non-empty string")
        if checksum is not None and (not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum)):
            raise ValueError(f"dependency inventory packages[{index}].checksum must be null or a sha256 digest")
        if not isinstance(dependency_count, int) or dependency_count < 0:
            raise ValueError(f"dependency inventory packages[{index}].dependency_count must be a non-negative integer")
        key = (name, version, source)
        if key in seen_packages:
            raise ValueError(f"dependency inventory packages[{index}] duplicates {name} {version} {source}")
        if previous_key is not None and key < previous_key:
            raise ValueError("dependency inventory packages must be sorted")
        seen_packages.add(key)
        previous_key = key
        if source == "workspace":
            workspace_count += 1
        else:
            external_count += 1
    if dependency_inventory.get("workspace_package_count") != workspace_count:
        raise ValueError("dependency inventory workspace_package_count mismatch")
    if dependency_inventory.get("external_package_count") != external_count:
        raise ValueError("dependency inventory external_package_count mismatch")

    spdx_sbom = json.loads(read_text_limited(bundle_dir / spdx_sbom_name))
    if not isinstance(spdx_sbom, dict):
        raise ValueError("SPDX SBOM root must be an object")
    if spdx_sbom.get("spdxVersion") != "SPDX-2.3":
        raise ValueError("SPDX SBOM spdxVersion must be SPDX-2.3")
    if spdx_sbom.get("dataLicense") != "CC0-1.0":
        raise ValueError("SPDX SBOM dataLicense must be CC0-1.0")
    if spdx_sbom.get("SPDXID") != "SPDXRef-DOCUMENT":
        raise ValueError("SPDX SBOM SPDXID must be SPDXRef-DOCUMENT")
    creation_info = spdx_sbom.get("creationInfo")
    if not isinstance(creation_info, dict) or creation_info.get("created") != "1970-01-01T00:00:00Z":
        raise ValueError("SPDX SBOM creationInfo.created must be deterministic")
    spdx_packages = spdx_sbom.get("packages")
    if not isinstance(spdx_packages, list) or len(spdx_packages) != len(packages) + 1:
        raise ValueError("SPDX SBOM package count must match dependency inventory plus root package")
    spdx_package_ids: set[str] = set()
    for index, package in enumerate(spdx_packages):
        if not isinstance(package, dict):
            raise ValueError(f"SPDX SBOM packages[{index}] must be an object")
        package_id = package.get("SPDXID")
        if not isinstance(package_id, str) or not package_id.startswith("SPDXRef-"):
            raise ValueError(f"SPDX SBOM packages[{index}].SPDXID must start with SPDXRef-")
        if package_id in spdx_package_ids:
            raise ValueError(f"SPDX SBOM packages[{index}] duplicates {package_id}")
        spdx_package_ids.add(package_id)
    relationships = spdx_sbom.get("relationships")
    if not isinstance(relationships, list) or len(relationships) < len(packages) + 1:
        raise ValueError("SPDX SBOM relationships must describe root package and dependencies")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle_dir", type=Path, help="release artifact directory from make release-local")
    args = parser.parse_args()

    try:
        verify_bundle(args.bundle_dir)
    except (OSError, ValueError, json.JSONDecodeError, tarfile.TarError) as exc:
        print(f"release bundle verification failed: {exc}", file=sys.stderr)
        return 1
    print(f"release bundle verification ok: {args.bundle_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
