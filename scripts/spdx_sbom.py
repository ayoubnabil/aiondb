#!/usr/bin/env python3
"""Generate and verify a deterministic SPDX JSON SBOM from Cargo.lock."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SPDX_ID_RE = re.compile(r"[^A-Za-z0-9.-]+")
DOCUMENT_NAME_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
MAX_SPDX_INPUT_BYTES = 16 * 1024 * 1024


def read_text_limited(path: Path, max_bytes: int = MAX_SPDX_INPUT_BYTES) -> str:
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


def spdx_id(prefix: str, *parts: object) -> str:
    value = "-".join(str(part) for part in parts if part is not None and str(part))
    value = SPDX_ID_RE.sub("-", value).strip("-")
    return f"SPDXRef-{prefix}-{value}"


def package_key(package: dict[str, object]) -> tuple[str, str, str]:
    return (
        str(package.get("name", "")),
        str(package.get("version", "")),
        str(package.get("source") or "workspace"),
    )


def cargo_purl(name: str, version: str) -> str:
    return f"pkg:cargo/{name}@{version}"


def validate_document_name(document_name: str) -> None:
    if not DOCUMENT_NAME_RE.fullmatch(document_name):
        raise ValueError("SPDX document name must contain only letters, numbers, dot, underscore, and dash")


def read_lock_packages(path: Path) -> list[dict[str, object]]:
    data = tomllib.loads(read_text_limited(path))
    packages = data.get("package")
    if not isinstance(packages, list) or not packages:
        raise ValueError(f"{path}: expected non-empty package list")

    parsed: list[dict[str, object]] = []
    seen: set[tuple[str, str, str]] = set()
    for index, package in enumerate(packages):
        if not isinstance(package, dict):
            raise ValueError(f"{path}: package[{index}] must be a table")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        if not isinstance(name, str) or not name:
            raise ValueError(f"{path}: package[{index}].name must be a non-empty string")
        if not isinstance(version, str) or not version:
            raise ValueError(f"{path}: package[{index}].version must be a non-empty string")
        if source is not None and not isinstance(source, str):
            raise ValueError(f"{path}: package[{index}].source must be a string when present")
        if checksum is not None and (not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum)):
            raise ValueError(f"{path}: package[{index}].checksum must be a sha256 digest when present")
        entry = {
            "name": name,
            "version": version,
            "source": source or "workspace",
            "checksum": checksum,
        }
        key = package_key(entry)
        if key in seen:
            raise ValueError(f"{path}: duplicate package entry {name} {version} {entry['source']}")
        seen.add(key)
        parsed.append(entry)
    return sorted(parsed, key=package_key)


def build_spdx(lockfile: Path, document_name: str) -> dict[str, object]:
    validate_document_name(document_name)
    root_id = "SPDXRef-Package-AionDB"
    packages: list[dict[str, object]] = [
        {
            "SPDXID": root_id,
            "name": "AionDB",
            "versionInfo": "NOASSERTION",
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "BUSL-1.1",
            "licenseDeclared": "BUSL-1.1",
            "copyrightText": "NOASSERTION",
            "externalRefs": [],
        }
    ]
    relationships: list[dict[str, str]] = [
        {
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": root_id,
        }
    ]

    for package in read_lock_packages(lockfile):
        name = str(package["name"])
        version = str(package["version"])
        source = str(package["source"])
        checksum = package.get("checksum")
        source_hash = hashlib.sha256(source.encode("utf-8")).hexdigest()[:12]
        package_id = spdx_id("Cargo", name, version, source_hash)
        checksums = []
        if isinstance(checksum, str):
            checksums.append({"algorithm": "SHA256", "checksumValue": checksum})
        packages.append(
            {
                "SPDXID": package_id,
                "name": name,
                "versionInfo": version,
                "downloadLocation": "NOASSERTION",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": "NOASSERTION",
                "copyrightText": "NOASSERTION",
                "checksums": checksums,
                "externalRefs": [
                    {
                        "referenceCategory": "PACKAGE-MANAGER",
                        "referenceType": "purl",
                        "referenceLocator": cargo_purl(name, version),
                    }
                ],
                "sourceInfo": source,
            }
        )
        relationships.append(
            {
                "spdxElementId": root_id,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": package_id,
            }
        )

    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": document_name,
        "documentNamespace": f"https://aiondb.local/spdx/{document_name}",
        "creationInfo": {
            "created": "1970-01-01T00:00:00Z",
            "creators": ["Tool: scripts/spdx_sbom.py"],
        },
        "documentDescribes": [root_id],
        "packages": packages,
        "relationships": relationships,
    }


def validate_spdx(data: object) -> list[str]:
    errors: list[str] = []
    if not isinstance(data, dict):
        return ["SPDX document root must be an object"]
    if data.get("spdxVersion") != "SPDX-2.3":
        errors.append("spdxVersion must be SPDX-2.3")
    if data.get("dataLicense") != "CC0-1.0":
        errors.append("dataLicense must be CC0-1.0")
    if data.get("SPDXID") != "SPDXRef-DOCUMENT":
        errors.append("SPDXID must be SPDXRef-DOCUMENT")
    creation_info = data.get("creationInfo")
    if not isinstance(creation_info, dict):
        errors.append("creationInfo must be an object")
    elif creation_info.get("created") != "1970-01-01T00:00:00Z":
        errors.append("creationInfo.created must be deterministic")
    packages = data.get("packages")
    if not isinstance(packages, list) or len(packages) < 2:
        errors.append("packages must include root package and dependencies")
        return errors
    seen_ids: set[str] = set()
    for index, package in enumerate(packages):
        if not isinstance(package, dict):
            errors.append(f"packages[{index}] must be an object")
            continue
        package_id = package.get("SPDXID")
        if not isinstance(package_id, str) or not package_id.startswith("SPDXRef-"):
            errors.append(f"packages[{index}].SPDXID must start with SPDXRef-")
        elif package_id in seen_ids:
            errors.append(f"duplicate SPDXID {package_id}")
        else:
            seen_ids.add(package_id)
        for key in ("name", "versionInfo", "downloadLocation", "licenseConcluded", "licenseDeclared"):
            if not isinstance(package.get(key), str) or not package.get(key):
                errors.append(f"packages[{index}].{key} must be a non-empty string")
        checksums = package.get("checksums", [])
        if checksums is not None and not isinstance(checksums, list):
            errors.append(f"packages[{index}].checksums must be an array when present")
    relationships = data.get("relationships")
    if not isinstance(relationships, list) or not relationships:
        errors.append("relationships must be a non-empty array")
        return errors
    valid_relationship_ids = seen_ids | {"SPDXRef-DOCUMENT"}
    for index, relationship in enumerate(relationships):
        if not isinstance(relationship, dict):
            errors.append(f"relationships[{index}] must be an object")
            continue
        source = relationship.get("spdxElementId")
        target = relationship.get("relatedSpdxElement")
        rel_type = relationship.get("relationshipType")
        if not isinstance(source, str) or not source:
            errors.append(f"relationships[{index}].spdxElementId must be a non-empty string")
        elif source not in valid_relationship_ids:
            errors.append(f"relationships[{index}].spdxElementId references unknown SPDXID {source}")
        if not isinstance(target, str) or not target:
            errors.append(f"relationships[{index}].relatedSpdxElement must be a non-empty string")
        elif target not in valid_relationship_ids:
            errors.append(f"relationships[{index}].relatedSpdxElement references unknown SPDXID {target}")
        if not isinstance(rel_type, str) or not rel_type:
            errors.append(f"relationships[{index}].relationshipType must be a non-empty string")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lockfile", type=Path, default=Path("Cargo.lock"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--document-name", required=True)
    parser.add_argument("--verify", action="store_true", help="verify after writing the SBOM")
    args = parser.parse_args()

    try:
        sbom = build_spdx(args.lockfile, args.document_name)
        errors = validate_spdx(sbom)
        if errors:
            for error in errors:
                print(f"SPDX SBOM validation failed: {error}", file=sys.stderr)
            return 1
        args.out.write_text(json.dumps(sbom, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {args.out}")
        if args.verify:
            data = json.loads(read_text_limited(args.out))
            errors = validate_spdx(data)
            if data != sbom:
                errors.append("SPDX SBOM does not match Cargo.lock")
            if errors:
                for error in errors:
                    print(f"SPDX SBOM validation failed: {error}", file=sys.stderr)
                return 1
            print("SPDX SBOM validation ok")
        return 0
    except (OSError, ValueError, json.JSONDecodeError, tomllib.TOMLDecodeError) as exc:
        print(f"SPDX SBOM error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
