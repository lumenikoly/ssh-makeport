"""Release guards and exact executable asset set, using temporary fixture bytes."""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest.mock import patch
from urllib.error import URLError

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts" / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class BuildReuseTests(unittest.TestCase):
    SHA = "a" * 40
    RUN = {"id": 42, "head_sha": SHA, "head_branch": "main", "event": "push",
           "status": "completed", "conclusion": "success"}
    ARTIFACT = {"name": "MakePort-linux-x86_64", "expired": False}

    def lookup(self, responses):
        # Exercise real URL construction and decoding without network access.
        with patch.object(release, "urlopen", side_effect=[
            io.BytesIO(json.dumps(response).encode()) for response in responses
        ]) as request:
            result = release.find_linux_build("owner/repo", self.SHA, "test-token")
        return result, request

    def test_reuses_exact_successful_main_push_and_authenticates_requests(self):
        result, request = self.lookup([
            {"workflow_runs": [self.RUN]}, {"artifacts": [self.ARTIFACT]},
        ])
        self.assertEqual(result, "42")
        url = request.call_args_list[0].args[0].full_url
        self.assertIn(f"head_sha={self.SHA}", url)
        self.assertIn("event=push", url)
        self.assertEqual(request.call_args_list[0].args[0].get_header("Authorization"),
                         "Bearer test-token")
        self.assertNotIn("test-token", url)

    def test_wrong_commit_branch_event_or_run_state_never_reuses_artifacts(self):
        for field, value in [("head_sha", "b" * 40), ("head_branch", "feature"),
                             ("event", "pull_request"), ("status", "in_progress"),
                             ("conclusion", "failure"), ("id", "42\nrun-id=1")]:
            with self.subTest(field=field):
                result, request = self.lookup([{"workflow_runs": [{**self.RUN, field: value}]}])
                self.assertEqual(result, "")
                self.assertEqual(request.call_count, 1)

    def test_expired_or_wrong_artifact_falls_back_to_fresh_build(self):
        for artifact in [{**self.ARTIFACT, "expired": True},
                         {**self.ARTIFACT, "name": "unrelated"},
                         {"name": self.ARTIFACT["name"]}]:
            with self.subTest(artifact=artifact):
                result, _ = self.lookup([{"workflow_runs": [self.RUN]},
                                         {"artifacts": [artifact]}])
                self.assertEqual(result, "")

    def test_tries_another_matching_run_if_first_artifact_is_missing(self):
        result, _ = self.lookup([
            {"workflow_runs": [self.RUN, {**self.RUN, "id": 41}]},
            {"artifacts": []}, {"artifacts": [self.ARTIFACT]},
        ])
        self.assertEqual(result, "41")

    def test_api_failure_falls_back_without_exposing_credentials(self):
        diagnostic = io.StringIO()
        with patch.object(release, "urlopen", side_effect=URLError("test-token")), redirect_stderr(diagnostic):
            self.assertEqual(release.find_linux_build("owner/repo", self.SHA, "test-token"), "")
        self.assertIn("building normally", diagnostic.getvalue())
        self.assertNotIn("test-token", diagnostic.getvalue())

    def test_no_completed_build_returns_empty_run_id(self):
        result, request = self.lookup([{"workflow_runs": []}])
        self.assertEqual(result, "")
        self.assertEqual(request.call_count, 1)

    def test_unexpected_api_response_falls_back_to_build(self):
        for response in [{}, {"workflow_runs": None}, {"workflow_runs": [None]}]:
            with self.subTest(response=response), redirect_stderr(io.StringIO()):
                result, _ = self.lookup([response])
                self.assertEqual(result, "")


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
