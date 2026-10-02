"""Validate the offline transport used by the Rust integration test."""
from __future__ import annotations
import os
from pathlib import Path
import select
import shlex
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


def read_until(process, marker, timeout=5):
    result = bytearray()
    deadline = time.monotonic() + timeout
    while marker not in result and time.monotonic() < deadline:
        if select.select([process.stdout.fileno()], [], [], 0.1)[0]:
            data = os.read(process.stdout.fileno(), 8192)
            if not data:
                break
            result.extend(data)
    return bytes(result)


def command(name, path, token="shim", target=None):
    result = ["sh", "-c", (ROOT / "remote" / f"{name}.sh").read_text(), "makeport", str(path), token]
    if target:
        result.append(target)
    return " ".join(shlex.quote(arg) for arg in result)


@unittest.skipUnless(os.name == "posix", "Unix PTY transport")
class FakeSSHTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.project = Path(self.temp.name)
        (self.project / "Makefile").write_text("dev:\n\t@python3 -c \"import time; print('CHILD_READY',flush=True); time.sleep(60)\"\n")
        self.processes = []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                try:
                    process.communicate(b"\x03", timeout=3)
                except (subprocess.TimeoutExpired, BrokenPipeError):
                    process.kill()
                    process.communicate(timeout=3)
            for pipe in (process.stdin, process.stdout, process.stderr):
                if pipe:
                    pipe.close()
        self.temp.cleanup()

    def spawn(self, args):
        process = subprocess.Popen([sys.executable, str(ROOT / "tests" / "fake_ssh.py"), *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.processes.append(process)
        return process

    def test_real_tcp_forwarding_and_shutdown(self):
        with socket.socket() as remote, socket.socket() as free:
            remote.bind(("127.0.0.1", 0))
            remote.listen()
            free.bind(("127.0.0.1", 0))
            local = free.getsockname()[1]
            free.close()
            def echo():
                client, _ = remote.accept()
                with client:
                    client.sendall(client.recv(100))
            thread = threading.Thread(target=echo, daemon=True)
            thread.start()
            spec = f"127.0.0.1:{local}:127.0.0.1:{remote.getsockname()[1]}"
            process = self.spawn(["-T", "-L", spec, "fake", command("connect", self.project)])
            self.assertIn(b"__MAKEPORT_READY_shim__", read_until(process, b"__MAKEPORT_READY_shim__"))
            with socket.create_connection(("127.0.0.1", local), timeout=2) as client:
                client.sendall(b"tunnel-ok")
                self.assertEqual(client.recv(100), b"tunnel-ok")
            process.communicate(b"", timeout=5)
            self.assertEqual(process.returncode, 0)
            thread.join(timeout=2)
            with self.assertRaises(OSError):
                socket.create_connection(("127.0.0.1", local), timeout=0.3)

    def test_port_conflict_never_reports_ready(self):
        with socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0))
            occupied.listen()
            local = occupied.getsockname()[1]
            spec = f"127.0.0.1:{local}:127.0.0.1:1"
            process = self.spawn(["-T", "-L", spec, "fake", command("connect", self.project)])
            output, error = process.communicate(b"", timeout=5)
            self.assertEqual(process.returncode, 255)
            self.assertNotIn(b"__MAKEPORT_READY", output)
            self.assertIn(b"cannot listen", error)

    def test_piped_ctrl_c_is_delivered_to_remote_pty(self):
        process = self.spawn(["-tt", "fake", command("task", self.project, target="dev")])
        self.assertIn(b"CHILD_READY", read_until(process, b"CHILD_READY"))
        process.stdin.write(b"\x03")
        process.stdin.flush()
        output = read_until(process, b"__MAKEPORT_EXIT_shim__:")
        self.assertIn(b"__MAKEPORT_EXIT_shim__:", output)
        process.wait(timeout=5)
        self.assertNotEqual(process.returncode, 0)


if __name__ == "__main__":
    unittest.main()
