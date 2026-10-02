"""Tests the exact shell programs embedded by Rust, using a real local PTY.

No network access and no SSH daemon are needed. Run on Linux or macOS:
    python3 -m unittest discover -s tests -p 'test_*.py' -v
"""
from __future__ import annotations

import errno
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

if os.name == "posix":
    import pty
else:
    pty = None

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = {name: (ROOT / "remote" / f"{name}.sh").read_text() for name in ("connect", "discover", "task")}
TOKEN = "test-123"


class RemotePTY:
    def __init__(self, script: str, *args: str):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execl("/bin/sh", "sh", "-c", script, "makeport", *args)
        self.output = bytearray()
        self.closed = False
        self.reaped = False

    def read_until(self, marker: bytes, seconds: float = 5) -> bytes:
        deadline = time.monotonic() + seconds
        while marker not in self.output and time.monotonic() < deadline:
            if self.closed:
                break
            if select.select([self.fd], [], [], 0.1)[0]:
                try:
                    chunk = os.read(self.fd, 8192)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
                    chunk = b""
                if not chunk:
                    self.closed = True
                    break
                self.output.extend(chunk)
        return bytes(self.output)

    def interrupt(self):
        os.write(self.fd, b"\x03")

    def wait(self, seconds: float = 5) -> int:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.reaped = True
                return os.waitstatus_to_exitcode(status)
            self.read_until(b"__NO_SUCH_MARKER__", 0.05)
        raise TimeoutError("PTY child did not exit")

    def close(self):
        if not self.reaped:
            try:
                os.killpg(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(self.pid, 0)
            self.reaped = True
        os.close(self.fd)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


@unittest.skipUnless(os.name == "posix" and shutil.which("make"), "POSIX shell and make required")
class ShellTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="makeport-test-")
        self.root = Path(self.temporary.name)
        self.project = self.root / "project with 'quote' and $(literal)"
        self.project.mkdir()
        (self.project / "Makefile").write_text("ok: ## Success\n\t@printf 'TASK_OK\\n'\n")

    def tearDown(self):
        self.temporary.cleanup()

    def run_script(self, name, *args, **kwargs):
        return subprocess.run(
            ["/bin/sh", "-c", SCRIPTS[name], "makeport", *map(str, args)],
            input=b"", stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5, **kwargs
        )

    def test_all_scripts_have_valid_posix_syntax(self):
        for name, script in SCRIPTS.items():
            result = subprocess.run(["/bin/sh", "-n"], input=script.encode(), capture_output=True)
            self.assertEqual(result.returncode, 0, (name, result.stderr))

    def test_connection_reports_ready_after_preflight(self):
        result = self.run_script("connect", self.project, TOKEN)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"__MAKEPORT_READY_{TOKEN}__".encode(), result.stdout)

    def test_connection_stays_alive_until_stdin_eof(self):
        proc = subprocess.Popen(["sh", "-c", SCRIPTS["connect"], "makeport", str(self.project), TOKEN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            time.sleep(0.15)
            self.assertIsNone(proc.poll())
            output, _ = proc.communicate(b"", timeout=5)
            self.assertIn(b"__MAKEPORT_READY_", output)
            self.assertEqual(proc.returncode, 0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()

    def test_invalid_directory_never_reports_ready(self):
        result = self.run_script("connect", self.root / "missing", TOKEN)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(b"__MAKEPORT_READY_", result.stdout)

    def test_relative_directory_is_rejected(self):
        result = self.run_script("connect", "relative", TOKEN)
        self.assertEqual(result.returncode, 64)

    def test_home_expansion_is_explicit(self):
        env = dict(os.environ, HOME=str(self.root))
        project = self.root / "plain"
        project.mkdir()
        result = self.run_script("connect", "~/plain", TOKEN, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_discovery_reads_makefile_without_evaluating_it(self):
        marker = self.root / "SHOULD_NOT_EXIST"
        content = f"X := $(shell touch {shlex.quote(str(marker))})\nok: ## Success\n"
        (self.project / "Makefile").write_text(content)
        result = self.run_script("discover", self.project)
        self.assertEqual(result.stdout.decode(), content)
        self.assertFalse(marker.exists())

    def test_discovery_uses_gnu_filename_precedence(self):
        (self.project / "makefile").write_text("lower: ## Lower\n")
        (self.project / "GNUmakefile").write_text("gnu: ## GNU\n")
        result = self.run_script("discover", self.project)
        self.assertEqual(result.stdout, b"gnu: ## GNU\n")

    def test_discovery_size_is_bounded(self):
        (self.project / "Makefile").write_bytes(b"x" * 400_000)
        result = self.run_script("discover", self.project)
        self.assertEqual(len(result.stdout), 262_145)

    def test_missing_makefile_is_explicit(self):
        (self.project / "Makefile").unlink()
        result = self.run_script("discover", self.project)
        self.assertEqual(result.returncode, 66)

    def test_task_requires_a_real_pty(self):
        result = self.run_script("task", self.project, TOKEN, "ok")
        self.assertEqual(result.returncode, 70)
        self.assertNotIn(b"TASK_OK", result.stdout)

    def test_success_is_acknowledged(self):
        with RemotePTY(SCRIPTS["task"], str(self.project), TOKEN, "ok") as process:
            output = process.read_until(f"__MAKEPORT_EXIT_{TOKEN}__:0".encode())
            self.assertIn(b"TASK_OK", output)
            self.assertIn(f"__MAKEPORT_STARTED_{TOKEN}__".encode(), output)
            self.assertIn(f"__MAKEPORT_EXIT_{TOKEN}__:0".encode(), output)
            self.assertEqual(process.wait(), 0)

    def test_failure_is_acknowledged_with_nonzero_code(self):
        (self.project / "Makefile").write_text("fail:\n\t@exit 7\n")
        with RemotePTY(SCRIPTS["task"], str(self.project), TOKEN, "fail") as process:
            output = process.read_until(f"__MAKEPORT_EXIT_{TOKEN}__:".encode())
            self.assertIn(f"__MAKEPORT_EXIT_{TOKEN}__:2".encode(), output)
            self.assertEqual(process.wait(), 2)

    def test_ctrl_c_reaches_make_and_its_child(self):
        child = self.project / "child.py"
        child.write_text("import os,time\nfrom pathlib import Path\nPath('child.pid').write_text(str(os.getpid()))\nprint('CHILD_READY', flush=True)\ntime.sleep(60)\n")
        (self.project / "Makefile").write_text("dev:\n\t@python3 child.py\n")
        with RemotePTY(SCRIPTS["task"], str(self.project), TOKEN, "dev") as process:
            output = process.read_until(b"CHILD_READY")
            self.assertIn(b"CHILD_READY", output)
            child_pid = int((self.project / "child.pid").read_text())
            process.interrupt()
            output = process.read_until(f"__MAKEPORT_EXIT_{TOKEN}__:".encode())
            self.assertIn(f"__MAKEPORT_EXIT_{TOKEN}__:".encode(), output)
            self.assertNotEqual(process.wait(), 0)
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)

    def test_nested_remote_shell_quoting_is_literal(self):
        # Same single-quote strategy as shell::quote, including the outer login shell.
        quote = lambda value: "'" + value.replace("'", "'\\''") + "'"
        command = f"sh -c {quote(SCRIPTS['discover'])} makeport {quote(str(self.project))}"
        result = subprocess.run(["/bin/sh", "-c", command], capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"ok: ## Success", result.stdout)


if __name__ == "__main__":
    unittest.main()
