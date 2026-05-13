#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

import package_manifest_json


class PackageManifestJsonTests(unittest.TestCase):
    def test_read_key_value_manifest_rejects_duplicate_keys(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-package-manifest-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        manifest = tmpdir / "bundle.manifest"
        manifest.write_text("name=first\nname=second\n", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "duplicate key name"):
            package_manifest_json.read_key_value_manifest(manifest)

    def test_read_text_limited_rejects_oversized_inputs(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-package-manifest-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        manifest = tmpdir / "bundle.manifest"
        with manifest.open("wb") as handle:
            handle.truncate(package_manifest_json.MAX_MANIFEST_INPUT_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "exceeds maximum size"):
            package_manifest_json.read_key_value_manifest(manifest)

    def test_read_text_limited_rejects_non_regular_inputs(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-package-manifest-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))

        with self.assertRaisesRegex(ValueError, "regular file"):
            package_manifest_json.read_key_value_manifest(tmpdir)

    def test_build_manifest_rejects_extra_filelist_paths(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-package-manifest-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        manifest = tmpdir / "bundle.manifest"
        filelist = tmpdir / "bundle.files.sha256"
        digest = "0" * 64

        manifest.write_text(
            "\n".join(
                [
                    "name=aiondb-local-test",
                    "version=0.0.0",
                    "commit=unknown",
                    "worktree_dirty=false",
                    "archive=aiondb-local-test.tar.gz",
                    "sha256_file=aiondb-local-test.tar.gz.sha256",
                    f"archive_sha256={digest}",
                    "filelist_sha256_file=aiondb-local-test.files.sha256",
                    "manifest_json_file=aiondb-local-test.manifest.json",
                    "dependency_inventory_file=aiondb-local-test.dependencies.json",
                    f"dependency_inventory_sha256={digest}",
                    "spdx_sbom_file=aiondb-local-test.spdx.json",
                    f"spdx_sbom_sha256={digest}",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        lines = [f"{digest} {path}" for path in sorted(package_manifest_json.EXPECTED_PACKAGE_PATHS)]
        lines.append(f"{digest} aiondb/unexpected-secret.txt")
        filelist.write_text("\n".join(lines) + "\n", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "unexpected package paths"):
            package_manifest_json.build_manifest(manifest, filelist)

    @staticmethod
    def _cleanup(path: Path) -> None:
        for child in sorted(path.rglob("*"), reverse=True):
            if child.is_dir():
                child.rmdir()
            else:
                child.unlink()
        path.rmdir()


if __name__ == "__main__":
    unittest.main()
