#!/usr/bin/env python3
"""Generate and verify the AionDB release dependency inventory."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_INVENTORY_INPUT_BYTES = 16 * 1024 * 1024


def read_text_limited(path: Path, max_bytes: int = MAX_INVENTORY_INPUT_BYTES) -> str:
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


def package_key(package: dict[str, object]) -> tuple[str, str, str]:
    return (
        str(package.get("name", "")),
        str(package.get("version", "")),
        str(package.get("source") or "workspace"),
    )


def read_lockfile(path: Path) -> dict[str, object]:
    data = tomllib.loads(read_text_limited(path))
    packages = data.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError(f"{path}: expected non-empty package list")

    inventory_packages: list[dict[str, object]] = []
    seen: set[tuple[str, str, str]] = set()
    for index, package in enumerate(packages):
        if not isinstance(package, dict):
            raise ValueError(f"{path}: package[{index}] must be a table")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        dependencies = package.get("dependencies", [])
        if not isinstance(name, str) or not name:
            raise ValueError(f"{path}: package[{index}].name must be a non-empty string")
        if not isinstance(version, str) or not version:
            raise ValueError(f"{path}: package[{index}].version must be a non-empty string")
        if source is not None and not isinstance(source, str):
            raise ValueError(f"{path}: package[{index}].source must be a string when present")
        if checksum is not None:
            if not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum):
                raise ValueError(f"{path}: package[{index}].checksum must be a sha256 digest when present")
        if not isinstance(dependencies, list):
            raise ValueError(f"{path}: package[{index}].dependencies must be an array when present")

        entry = {
            "name": name,
            "version": version,
            "source": source or "workspace",
            "checksum": checksum,
            "dependency_count": len(dependencies),
        }
        key = package_key(entry)
        if key in seen:
            raise ValueError(f"{path}: duplicate package entry {name} {version} {entry['source']}")
        seen.add(key)
        inventory_packages.append(entry)

    inventory_packages.sort(key=package_key)
    external_packages = [package for package in inventory_packages if package["source"] != "workspace"]
    workspace_packages = [package for package in inventory_packages if package["source"] == "workspace"]
    return {
        "schema_version": 1,
        "lockfile": path.name,
        "package_count": len(inventory_packages),
        "workspace_package_count": len(workspace_packages),
        "external_package_count": len(external_packages),
        "packages": inventory_packages,
    }


def validate_inventory(data: object) -> list[str]:
    errors: list[str] = []
    if not isinstance(data, dict):
        return ["inventory root must be an object"]
    if data.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    if data.get("lockfile") != "Cargo.lock":
        errors.append("lockfile must be Cargo.lock")
    packages = data.get("packages")
    if not isinstance(packages, list) or not packages:
        errors.append("packages must be a non-empty array")
        return errors
    if data.get("package_count") != len(packages):
        errors.append("package_count must match packages length")

    workspace_count = 0
    external_count = 0
    previous_key: tuple[str, str, str] | None = None
    seen: set[tuple[str, str, str]] = set()
    for index, package in enumerate(packages):
        if not isinstance(package, dict):
            errors.append(f"packages[{index}] must be an object")
            continue
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        dependency_count = package.get("dependency_count")
        if not isinstance(name, str) or not name:
            errors.append(f"packages[{index}].name must be a non-empty string")
        if not isinstance(version, str) or not version:
            errors.append(f"packages[{index}].version must be a non-empty string")
        if not isinstance(source, str) or not source:
            errors.append(f"packages[{index}].source must be a non-empty string")
        if checksum is not None and (not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum)):
            errors.append(f"packages[{index}].checksum must be null or a sha256 digest")
        if not isinstance(dependency_count, int) or dependency_count < 0:
            errors.append(f"packages[{index}].dependency_count must be a non-negative integer")

        key = package_key(package)
        if key in seen:
            errors.append(f"duplicate package entry {key[0]} {key[1]} {key[2]}")
        seen.add(key)
        if previous_key is not None and key < previous_key:
            errors.append("packages must be sorted by name, version, and source")
        previous_key = key
        if source == "workspace":
            workspace_count += 1
        elif isinstance(source, str):
            external_count += 1

    if data.get("workspace_package_count") != workspace_count:
        errors.append("workspace_package_count must match workspace package count")
    if data.get("external_package_count") != external_count:
        errors.append("external_package_count must match external package count")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lockfile", type=Path, default=Path("Cargo.lock"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--verify", action="store_true", help="verify after writing the inventory")
    args = parser.parse_args()

    try:
        inventory = read_lockfile(args.lockfile)
        errors = validate_inventory(inventory)
        if errors:
            for error in errors:
                print(f"dependency inventory validation failed: {error}", file=sys.stderr)
            return 1
        args.out.write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {args.out}")
        if args.verify:
            data = json.loads(read_text_limited(args.out))
            errors = validate_inventory(data)
            if data != inventory:
                errors.append("dependency inventory does not match Cargo.lock")
            if errors:
                for error in errors:
                    print(f"dependency inventory validation failed: {error}", file=sys.stderr)
                return 1
            print("dependency inventory validation ok")
        return 0
    except (OSError, ValueError, json.JSONDecodeError, tomllib.TOMLDecodeError) as exc:
        print(f"dependency inventory error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
