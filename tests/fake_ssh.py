#!/usr/bin/env python3
"""An offline SSH stand-in for engine tests; not shipped in release binaries.

Executes the *actual* remote command locally. -tt gets a real PTY; -L gets
real loopback TCP forwarding. There is deliberately no authentication emulation.
"""
from __future__ import annotations
import errno
import os
import pty
import select
import socket
import subprocess
import sys
import threading


def relay(client, host, port):
    try:
        upstream = socket.create_connection((host, port), timeout=2)
    except OSError:
        client.close()
        return
    with client, upstream:
        while True:
            ready, _, _ = select.select([client, upstream], [], [], 2)
            for source in ready:
                data = source.recv(65536)
                if not data:
                    return
                (upstream if source is client else client).sendall(data)


def listen(spec):
    bind, local, host, remote = spec.split(":")
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((bind, int(local)))
    listener.listen()
    def accept():
        while True:
            try:
                client, _ = listener.accept()
            except OSError:
                return
            threading.Thread(target=relay, args=(client, host, int(remote)), daemon=True).start()
    threading.Thread(target=accept, daemon=True).start()
    return listener


def main():
    args = sys.argv[1:]
    listeners = []
    for i, arg in enumerate(args):
        if arg == "-L":
            try:
                listeners.append(listen(args[i + 1]))
            except OSError as exc:
                print(f"cannot listen: {exc}", file=sys.stderr)
                return 255
    command = args[-1]
    if "-tt" not in args:
        return subprocess.call(["/bin/sh", "-c", command])
    pid, fd = pty.fork()
    if pid == 0:
        os.execl("/bin/sh", "sh", "-c", command)
    stdin_open = True
    try:
        while True:
            readers = [fd] + ([0] if stdin_open else [])
            ready, _, _ = select.select(readers, [], [], 0.2)
            if 0 in ready:
                data = os.read(0, 4096)
                if data:
                    os.write(fd, data)
                else:
                    stdin_open = False
                    os.close(fd)  # PTY hangup on SSH stdin closure in this test shim.
                    break
            if fd in ready:
                try:
                    data = os.read(fd, 8192)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
                    break
                if not data:
                    break
                os.write(1, data)
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    return code if code >= 0 else 255


if __name__ == "__main__":
    sys.exit(main())
