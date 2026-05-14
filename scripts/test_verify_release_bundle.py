#!/usr/bin/env python3

import io
import tarfile
import tempfile
import unittest
from pathlib import Path

import verify_release_bundle


class TarFileDigestTests(unittest.TestCase):
    def test_read_key_value_manifest_rejects_duplicate_keys(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-release-verify-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        manifest_path = tmpdir / "bundle.manifest"
        manifest_path.write_text("name=first\nname=second\n", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "duplicate key name"):
            verify_release_bundle.read_key_value_manifest(manifest_path)

    def test_read_text_limited_rejects_oversized_metadata(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-release-verify-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        manifest_path = tmpdir / "bundle.manifest"
        with manifest_path.open("wb") as handle:
            handle.truncate(verify_release_bundle.MAX_BUNDLE_METADATA_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "exceeds maximum size"):
            verify_release_bundle.read_key_value_manifest(manifest_path)

    def test_tar_file_digests_rejects_duplicate_members(self) -> None:
        archive_path = self._write_archive(
            [
                ("file", "aiondb/README.md", b"first"),
                ("file", "aiondb/README.md", b"second"),
            ]
        )

        with self.assertRaisesRegex(ValueError, "duplicate archive path"):
            verify_release_bundle.tar_file_digests(archive_path)

    def test_tar_file_digests_rejects_duplicate_directory_and_file(self) -> None:
        archive_path = self._write_archive(
            [
                ("dir", "aiondb/README.md", b""),
                ("file", "aiondb/README.md", b"readme"),
            ]
        )

        with self.assertRaisesRegex(ValueError, "duplicate archive path"):
            verify_release_bundle.tar_file_digests(archive_path)

    def test_tar_file_digests_rejects_links(self) -> None:
        archive_path = self._write_archive(
            [
                ("file", "aiondb/README.md", b"readme"),
                ("symlink", "aiondb/aiondb", b"../../outside"),
            ]
        )

        with self.assertRaisesRegex(ValueError, "unsupported archive member type"):
            verify_release_bundle.tar_file_digests(archive_path)

    def test_tar_file_digests_rejects_normalized_paths(self) -> None:
        for name in ("aiondb//README.md", "aiondb/./README.md"):
            with self.subTest(name=name):
                archive_path = self._write_archive([("file", name, b"readme")])
                with self.assertRaisesRegex(ValueError, "unsafe archive member path"):
                    verify_release_bundle.tar_file_digests(archive_path)

    def test_tar_file_digests_rejects_backslash_paths(self) -> None:
        archive_path = self._write_archive([("file", "aiondb\\README.md", b"readme")])

        with self.assertRaisesRegex(ValueError, "unsafe archive member path"):
            verify_release_bundle.tar_file_digests(archive_path)

    def test_validate_archive_member_size_rejects_oversized_member(self) -> None:
        with self.assertRaisesRegex(ValueError, "archive member aiondb/aiondb exceeds maximum size"):
            verify_release_bundle.validate_archive_member_size(
                Path("bundle.tar.gz"),
                "aiondb/aiondb",
                verify_release_bundle.MAX_ARCHIVE_MEMBER_BYTES + 1,
                0,
            )

    def test_validate_archive_member_size_rejects_oversized_total(self) -> None:
        with self.assertRaisesRegex(ValueError, "archive contents exceed maximum size"):
            verify_release_bundle.validate_archive_member_size(
                Path("bundle.tar.gz"),
                "aiondb/aiondb",
                1,
                verify_release_bundle.MAX_ARCHIVE_TOTAL_BYTES,
            )

    def test_bundle_file_names_rejects_too_many_files(self) -> None:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-release-verify-test-"))
        self.addCleanup(lambda: self._cleanup(tmpdir))
        for index in range(verify_release_bundle.MAX_BUNDLE_FILES + 1):
            (tmpdir / f"file-{index}").write_text("x", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "exceeds maximum bundle file count"):
            verify_release_bundle.bundle_file_names(tmpdir)

    def test_tar_file_digests_rejects_too_many_members(self) -> None:
        archive_path = self._write_archive(
            [
                ("dir", f"aiondb/dir-{index}", b"")
                for index in range(verify_release_bundle.MAX_ARCHIVE_MEMBERS + 1)
            ]
        )

        with self.assertRaisesRegex(ValueError, "archive exceeds maximum member count"):
            verify_release_bundle.tar_file_digests(archive_path)

    def _write_archive(self, entries: list[tuple[str, str, bytes]]) -> Path:
        tmpdir = Path(tempfile.mkdtemp(prefix="aiondb-release-verify-test-"))
        archive_path = tmpdir / "bundle.tar.gz"
        with tarfile.open(archive_path, "w:gz") as archive:
            for kind, name, data in entries:
                info = tarfile.TarInfo(name)
                if kind == "file":
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
                elif kind == "symlink":
                    info.type = tarfile.SYMTYPE
                    info.linkname = data.decode("utf-8")
                    archive.addfile(info)
                elif kind == "dir":
                    info.type = tarfile.DIRTYPE
                    archive.addfile(info)
                else:
                    raise AssertionError(f"unknown test entry kind: {kind}")
        self.addCleanup(lambda: self._cleanup(tmpdir))
        return archive_path

    @staticmethod
    def _cleanup(path: Path) -> None:
        for child in sorted(path.rglob("*"), reverse=True):
            child.unlink()
        path.rmdir()


if __name__ == "__main__":
    unittest.main()
