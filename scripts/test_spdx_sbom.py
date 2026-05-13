#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

import spdx_sbom


class SpdxSbomTests(unittest.TestCase):
    def test_read_lock_packages_rejects_oversized_lockfile(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-spdx-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        lockfile = tmpdir / "Cargo.lock"
        with lockfile.open("wb") as handle:
            handle.truncate(spdx_sbom.MAX_SPDX_INPUT_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "exceeds maximum size"):
            spdx_sbom.read_lock_packages(lockfile)

    def test_validate_document_name_rejects_unsafe_names(self) -> None:
        with self.assertRaisesRegex(ValueError, "SPDX document name"):
            spdx_sbom.validate_document_name("aiondb/test?name")

        spdx_sbom.validate_document_name("aiondb-test_1.0")

    def test_validate_spdx_rejects_unknown_relationship_targets(self) -> None:
        data = {
            "spdxVersion": "SPDX-2.3",
            "dataLicense": "CC0-1.0",
            "SPDXID": "SPDXRef-DOCUMENT",
            "creationInfo": {"created": "1970-01-01T00:00:00Z"},
            "packages": [
                {
                    "SPDXID": "SPDXRef-Package-AionDB",
                    "name": "AionDB",
                    "versionInfo": "NOASSERTION",
                    "downloadLocation": "NOASSERTION",
                    "licenseConcluded": "BUSL-1.1",
                    "licenseDeclared": "BUSL-1.1",
                },
                {
                    "SPDXID": "SPDXRef-Cargo-dep-1.0.0",
                    "name": "dep",
                    "versionInfo": "1.0.0",
                    "downloadLocation": "NOASSERTION",
                    "licenseConcluded": "NOASSERTION",
                    "licenseDeclared": "NOASSERTION",
                },
            ],
            "relationships": [
                {
                    "spdxElementId": "SPDXRef-Package-AionDB",
                    "relationshipType": "DEPENDS_ON",
                    "relatedSpdxElement": "SPDXRef-Missing",
                }
            ],
        }

        self.assertIn(
            "relationships[0].relatedSpdxElement references unknown SPDXID SPDXRef-Missing",
            spdx_sbom.validate_spdx(data),
        )

    @staticmethod
    def _cleanup(path: Path) -> None:
        for child in sorted(path.rglob("*"), reverse=True):
            child.unlink()
        path.rmdir()


if __name__ == "__main__":
    unittest.main()
