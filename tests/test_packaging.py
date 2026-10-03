"""Checks archive structure only; fixture bytes are NOT compiled binaries."""
from __future__ import annotations
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("package", ROOT / "scripts" / "package.py")
packager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packager)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name)
        self.binary = self.directory / "fixture"
        self.binary.write_bytes(b"TEST FIXTURE; NOT AN EXECUTABLE")

    def tearDown(self):
        self.temp.cleanup()

    def test_missing_binary_is_an_error(self):
        with self.assertRaises(FileNotFoundError):
            packager.package(self.directory / "missing", "linux", "x86_64", self.directory / "out")

    def test_linux_package_contains_binary_and_license(self):
        path = packager.package(self.binary, "linux", "x86_64", self.directory / "out")
        with zipfile.ZipFile(path) as archive:
            self.assertIn("MakePort-linux-x86_64/makeport", archive.namelist())
            self.assertIn("MakePort-linux-x86_64/LICENSE", archive.namelist())
            self.assertIsNone(archive.testzip())

    def test_windows_package_uses_exe_extension(self):
        path = packager.package(self.binary, "windows", "amd64", self.directory / "out")
        with zipfile.ZipFile(path) as archive:
            self.assertIn("MakePort-windows-amd64/makeport.exe", archive.namelist())

    def test_macos_bundle_structure_without_signing(self):
        with patch("platform.system", return_value="Linux"):
            path = packager.package(self.binary, "macos", "arm64", self.directory / "out")
        with zipfile.ZipFile(path) as archive:
            root = "MakePort-macos-arm64/MakePort.app/Contents/"
            self.assertIn(root + "MacOS/makeport", archive.namelist())
            info = plistlib.loads(archive.read(root + "Info.plist"))
            self.assertEqual(info["CFBundleExecutable"], "makeport")
            version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
            self.assertEqual(info["CFBundleShortVersionString"], version)
            self.assertEqual(info["CFBundleVersion"], version)


if __name__ == "__main__":
    unittest.main()
