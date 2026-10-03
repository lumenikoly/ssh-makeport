"""Release guards and exact executable asset set, using temporary fixture bytes."""
from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts" / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text('[package]\nname = "makeport"\nversion = "1.2.3"\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "makeport"\nversion = "1.2.3"\n', encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def test_valid_tag_must_match_manifest_and_lock(self):
        release.validate("v1.2.3", self.root)
        with self.assertRaises(ValueError):
            release.validate("v1.2.4", self.root)
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "makeport"\nversion = "1.2.2"\n', encoding="utf-8")
        with self.assertRaises(ValueError):
            release.validate("v1.2.3", self.root)

    def test_invalid_tags_and_path_injection_are_rejected(self):
        for tag in ("1.2.3", "v01.2.3", "v1.2", "v1.2.3-beta", "v1.2.3\n", "v1.2.3;echo bad", "../v1.2.3"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.version_from_tag(tag)

    def test_missing_lock_is_rejected(self):
        (self.root / "Cargo.lock").unlink()
        with self.assertRaises(FileNotFoundError):
            release.validate("v1.2.3", self.root)

    def test_collects_executable_bytes_without_archiving_or_overwriting(self):
        directory = self.root / "target" / "release"
        directory.mkdir(parents=True)
        for system, arch in release.PLATFORMS:
            source = directory / ("makeport.exe" if system == "windows" else "makeport")
            fixture = f"EXECUTABLE FIXTURE {system}-{arch}".encode()
            source.write_bytes(fixture)
            source.chmod(0o755)
            asset = release.collect("v1.2.3", system, arch, self.root)
            self.assertEqual(asset.read_bytes(), fixture)
            self.assertTrue(asset.name.endswith(".exe" if system == "windows" else f"-{arch}"))
            with self.assertRaises(FileExistsError):
                release.collect("v1.2.3", system, arch, self.root)

    def test_missing_executable_and_unsupported_platform_fail(self):
        with self.assertRaises(FileNotFoundError):
            release.collect("v1.2.3", "macos", "arm64", self.root)
        with self.assertRaises(ValueError):
            release.asset_name("v1.2.3", "windows", "../evil")

    def test_checksums_require_all_four_executables_and_no_extra_files(self):
        directory = self.root / "release-assets"
        directory.mkdir()
        for system, arch in release.PLATFORMS:
            (directory / release.asset_name("v1.2.3", system, arch)).write_bytes(b"fixture")
        output = release.checksums("v1.2.3", self.root)
        lines = output.read_text().splitlines()
        self.assertEqual(len(lines), 4)
        self.assertTrue(all(line.startswith(hashlib.sha256(b"fixture").hexdigest()+"  ") for line in lines))
        self.assertEqual(output.read_bytes().count(b"\r"), 0)
        output_before = output.read_bytes()
        (directory / "unexpected.zip").write_bytes(b"bad")
        with self.assertRaises(ValueError):
            release.checksums("v1.2.3", self.root)
        self.assertEqual(output.read_bytes(), output_before)
        (directory / "unexpected.zip").unlink()
        (directory / release.asset_name("v1.2.3", "macos", "arm64")).unlink()
        with self.assertRaises(ValueError):
            release.checksums("v1.2.3", self.root)


if __name__ == "__main__":
    unittest.main()
