#!/usr/bin/env python3
"""Deterministic real-engine proof for inherited port-probe listeners.

PGX_DIST=<assembled prefix> python3 -I tests/pgx/startup_resource_proof.py
"""
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib.harness import Pgx


def hold_parent_descriptors():
    reader, notify = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(notify)
        os.read(reader, 1)
        os._exit(0)
    os.close(reader)
    return pid, notify


def release(holder):
    pid, notify = holder
    os.write(notify, b"x")
    os.close(notify)
    os.waitpid(pid, 0)


with Pgx("probe-proof") as pg:
    import socket
    probe = socket.socket()
    probe.bind(("127.0.0.1", pg.port))
    probe.listen(1)
    inherited = hold_parent_descriptors()
    probe.close()
    try:
        # The original parent probe would advertise this port as available,
        # but a concurrently forked child still owns the listener.
        failed = pg.cli("init", "--port", str(pg.port))
        assert failed.code == 4, repr(failed)
        assert "in use" in failed.stderr, repr(failed)
    finally:
        release(inherited)

    # This probe process is single-threaded and never forks. Another child
    # forked from the harness cannot inherit its socket.
    code = """
import socket, sys
s = socket.socket(); s.bind(('127.0.0.1', int(sys.argv[1]))); s.listen(1)
print('bound', flush=True)
sys.stdin.buffer.read(1)
s.close()
"""
    isolated = subprocess.Popen([sys.executable, "-I", "-c", code, str(pg.port)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    assert isolated.stdout.readline() == b"bound\n"
    sibling = hold_parent_descriptors()
    try:
        isolated.stdin.write(b"x")
        isolated.stdin.flush()
        assert isolated.wait(timeout=5) == 0
        # The sibling remains deliberately blocked, just as in the bad case.
        # This time init, start and authenticated TCP queries all succeed.
        pg.up()
        with pg.connect_tcp() as connection:
            result = connection.scalar("SELECT 1")
        assert result == "1", result
        print(json.dumps({"parent_probe_with_inherited_listener": failed.code,
                          "isolated_probe_with_live_sibling": "ready",
                          "authenticated_tcp_select": result}))
    finally:
        release(sibling)
        if isolated.poll() is None:
            isolated.kill()
            isolated.wait()
