"""Runs the `pgx` CLI from an assembled distribution against a temporary
PGX_HOME, and connects to the runtime. One harness per test; everything is
stopped and deleted on exit, even on failure.

    PGX_DIST=<prefix> is required (dist/assemble.sh --out ...).
"""
import json
import errno
import fcntl
import os
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time

from . import pgclient

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))


def dist_prefix():
    p = os.environ.get("PGX_DIST")
    if not p:
        sys.exit("PGX_DIST is not set: point it at an assembled distribution prefix (dist/assemble.sh --out ...)")
    for rel in ("bin/pgx", "bin/postgres", "share/timezone", "share/pgx/base-cluster/PG_VERSION"):
        if not os.path.exists(os.path.join(p, rel)):
            sys.exit("PGX_DIST=%s lacks %s" % (p, rel))
    return p


def _is_zombie(pid):
    # A process that has exited but not been reaped (common when PID 1 does not
    # reap, e.g. a bare container) still appears in pgrep; it is not running.
    try:
        with open("/proc/%d/stat" % pid) as f:
            stat = f.read()
        return stat[stat.rfind(")") + 1:].strip().startswith("Z")
    except OSError:
        return False


def reserve_port():
    """Lease a non-ephemeral port across Rust and Python test processes."""
    leases = os.path.join(scratch_root(), "ports")
    os.makedirs(leases, exist_ok=True)
    try:
        with open("/proc/sys/net/ipv4/ip_local_port_range") as f:
            first, last = map(int, f.read().split())
    except FileNotFoundError:
        values = subprocess.check_output(
            ["sysctl", "-n", "net.inet.ip.portrange.first", "net.inet.ip.portrange.last"], text=True
        ).split()
        first, last = map(int, values)
    offset = secrets.randbelow(20000)
    for index in range(20000):
        port = 10000 + (offset + index) % 20000
        if first <= port <= last:
            continue
        # Never unlink leases: replacing the inode would defeat flock.
        lease = open(os.path.join(leases, "%d.lock" % port), "a")
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with socket.socket() as ipv4:
                ipv4.bind(("127.0.0.1", port))
                try:
                    with socket.socket(socket.AF_INET6) as ipv6:
                        ipv6.bind(("::1", port))
                except OSError as e:
                    if e.errno not in (errno.EAFNOSUPPORT, errno.EPROTONOSUPPORT, errno.EADDRNOTAVAIL):
                        raise
            return port, lease
        except (BlockingIOError, OSError):
            lease.close()
    raise RuntimeError("no unleased non-ephemeral test port available")


def scratch_root():
    root = os.environ.get("PGX_TEST_SCRATCH", "/tmp/pgxt")  # short: socket paths
    os.makedirs(root, exist_ok=True)
    return root


class CliResult:
    def __init__(self, proc):
        self.code = proc.returncode
        self.stdout = proc.stdout
        self.stderr = proc.stderr

    def json(self):
        return json.loads(self.stdout.strip())

    def __repr__(self):
        return "exit %d\nstdout: %s\nstderr: %s" % (self.code, self.stdout, self.stderr)


class Pgx:
    def __init__(self, name="t", durability=None, keep=False, settings=None):
        self.prefix = dist_prefix()
        self.home = os.path.join(scratch_root(), "%s-%d-%s" % (name, os.getpid(), secrets.token_hex(3)))
        os.makedirs(self.home)
        self.port, self._port_lease = reserve_port()
        self.durability = durability
        self.keep = keep or bool(os.environ.get("PGX_TEST_KEEP"))
        self.settings = settings or {}
        self.env = dict(os.environ, PGX_HOME=self.home)
        self.env.pop("PGHOST", None)
        self.env.pop("PGPORT", None)

    @property
    def bin(self):
        return os.path.join(self.prefix, "bin", "pgx")

    @property
    def data(self):
        return os.path.join(self.home, "data")

    def cli(self, *args, json_out=False, check=False, timeout=600, env=None):
        argv = [self.bin] + (["--json"] if json_out else []) + list(args)
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, env=dict(self.env, **(env or {})))
        r = CliResult(proc)
        if r.code == 4 or (check and r.code != 0):
            r.diagnostics = self.diagnostics()
        if check and r.code != 0:
            raise AssertionError("pgx %s failed: %r\n--- diagnostics\n%s\n--- log tail\n%s" % (" ".join(args), r, r.diagnostics, self.log_tail()))
        return r

    def diagnostics(self):
        """Who holds the port, what the pid file says, which engines exist: for post-mortems."""
        out = ["time %s" % time.strftime("%H:%M:%S")]
        if shutil.which("lsof"):
            out.append("lsof -i:%d:\n%s" % (self.port, subprocess.run(["lsof", "-nP", "-i:%d" % self.port], capture_output=True, text=True).stdout))
        out.append("netstat entries for port:\n%s" % subprocess.run("netstat -anv 2>/dev/null | grep '[.:]%d ' || true" % self.port, shell=True, capture_output=True, text=True).stdout.strip())
        try:
            with open(os.path.join(self.data, "postmaster.pid")) as f:
                out.append("postmaster.pid: %r" % f.read())
        except OSError as e:
            out.append("postmaster.pid: %s" % e)
        out.append("engine pids: %s" % self.engine_pids())
        out.append("ps (postgres): %s" % subprocess.run("ps -axo pid,ppid,stat,lstart,command | grep '[p]ostgres -D' || true", shell=True, capture_output=True, text=True).stdout.strip())
        return "\n".join(out)

    def init(self, *extra):
        args = ["init", "--port", str(self.port)] + list(extra)
        if self.durability:
            args += ["--durability", self.durability]
        result = self.cli(*args, check=True)
        if self.settings:
            # Test-only settings follow include 'pgx.conf', so startup-only
            # features can be enabled before the first engine launch.
            with open(os.path.join(self.data, "postgresql.conf"), "a") as f:
                for key, value in self.settings.items():
                    f.write("\n%s = %s\n" % (key, value))
        return result

    def start(self):
        return self.cli("start", check=True)

    def stop(self, force=False):
        return self.cli("stop", *(["--force"] if force else []), check=True)

    def up(self):
        self.init()
        self.start()
        return self

    def config(self):
        with open(os.path.join(self.home, "config.json")) as f:
            return json.load(f)

    @property
    def sock_dir(self):
        return self.config().get("run_dir") or os.path.join(self.home, "run")

    def connect(self, database="postgres", timeout=60):
        return pgclient.connect_unix(self.sock_dir, self.port, database, timeout=timeout)

    def connect_tcp(self, database="postgres", password=None, timeout=60):
        return pgclient.connect_tcp("127.0.0.1", self.port, database, "postgres", password or self.config()["password"], timeout=timeout)

    def sql(self, database, statement):
        with self.connect(database) as c:
            return c.query(statement)

    def engine_pid(self):
        try:
            with open(os.path.join(self.data, "postmaster.pid")) as f:
                return int(f.readline().strip())
        except (OSError, ValueError):
            return None

    def engine_pids(self):
        out = subprocess.run(["pgrep", "-f", "postgres .*-D %s" % self.data], capture_output=True, text=True).stdout
        pids = [int(x) for x in out.split()]
        return [p for p in pids if not _is_zombie(p)]

    def kill(self, sig=signal.SIGKILL):
        pid = self.engine_pid()
        if pid:
            os.kill(pid, sig)
        return pid

    def wait_engine_gone(self, timeout=30):
        t0 = time.time()
        while time.time() - t0 < timeout:
            if not self.engine_pids():
                return True
            time.sleep(0.05)
        return False

    def log_tail(self, n=30):
        try:
            with open(os.path.join(self.home, "log", "pgx.log")) as f:
                return "".join(f.readlines()[-n:])
        except OSError:
            return ""

    def base_dirs(self):
        return sorted(os.listdir(os.path.join(self.data, "base")))

    def cleanup(self):
        subprocess.run([self.bin, "stop", "--force"], capture_output=True, env=self.env)
        for pid in self.engine_pids():
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        if not self.keep:
            shutil.rmtree(self.home, ignore_errors=True)
        self._port_lease.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.cleanup()
