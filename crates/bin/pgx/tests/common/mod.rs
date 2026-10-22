//! Black-box harness: a temporary installation prefix (the engine and share
//! trees of an assembled distribution, plus the `pgx` binary under test) and a
//! temporary `PGX_HOME`. Every test gets its own; the engine is stopped and
//! the directories removed on drop. Failed tests keep their logs for diagnosis.
//!
//! Needs `PGX_DIST=<assembled prefix>` (see dist/assemble.sh). Without it the
//! harness fails loudly: these suites are release gates, never silent skips.
#![allow(dead_code)]
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

static COUNTER: AtomicU32 = AtomicU32::new(0);
static PORT_SEQ: AtomicU32 = AtomicU32::new(0);
static CLI_BINARY: OnceLock<PathBuf> = OnceLock::new();

pub struct Pgx {
    pub prefix: PathBuf,
    pub home: PathBuf,
    pub port: u16,
    // Keep the cross-process reservation until engine cleanup has finished.
    _port_lease: std::fs::File,
}

pub fn dist_prefix() -> PathBuf {
    let p = std::env::var("PGX_DIST").unwrap_or_else(|_| {
        panic!("PGX_DIST is not set: point it at an assembled distribution prefix (dist/assemble.sh --out ...)")
    });
    let p = PathBuf::from(p);
    for rel in [
        "bin/postgres",
        "share/timezone",
        "share/pgx/base-cluster/PG_VERSION",
        "share/pgx/dist.json",
    ] {
        assert!(p.join(rel).exists(), "PGX_DIST={} lacks {rel}", p.display());
    }
    p
}

fn port_is_free(port: u16) -> bool {
    let Ok(_v4) = TcpListener::bind(("127.0.0.1", port)) else {
        return false;
    };
    match TcpListener::bind(("::1", port)) {
        Ok(_) => true,
        Err(e) => {
            e.kind() == std::io::ErrorKind::AddrNotAvailable
                || e.raw_os_error() == Some(libc::EAFNOSUPPORT)
        }
    }
}

fn ephemeral_ports() -> (u16, u16) {
    #[cfg(target_os = "linux")]
    let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").ok();
    #[cfg(not(target_os = "linux"))]
    let range = Command::new("sysctl")
        .args([
            "-n",
            "net.inet.ip.portrange.first",
            "net.inet.ip.portrange.last",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    range
        .and_then(|s| {
            let mut words = s.split_whitespace();
            Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
        })
        .unwrap_or((32768, 65535))
}

fn reserve_port() -> (u16, std::fs::File) {
    // A PID offset alone is not a reservation: two test binaries can choose the
    // same candidate before either starts its engine. Keep a flock for the
    // entire harness lifetime, including init's socket-only bootstrap and stops.
    // Do not unlink lease files: replacing their inode would defeat flock.
    let leases = scratch_root().join("ports");
    std::fs::create_dir_all(&leases).unwrap();
    let (first_ephemeral, last_ephemeral) = ephemeral_ports();
    let offset = (std::process::id().wrapping_mul(131)) % 20000;
    for _ in 0..20000u32 {
        let seq = PORT_SEQ.fetch_add(1, Ordering::SeqCst);
        let port = (10000 + ((offset + seq) % 20000)) as u16;
        // Sibling TCP clients must not take a future engine's listening port.
        if (first_ephemeral..=last_ephemeral).contains(&port) {
            continue;
        }
        let lease = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(leases.join(format!("{port}.lock")))
            .expect("open test port lease");
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = std::io::Error::last_os_error();
            assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock, "port lease: {e}");
            continue;
        }
        // Match init's IPv4 AND IPv6 check, not just its first listener.
        if port_is_free(port) {
            return (port, lease);
        }
    }
    panic!("no unleased, non-ephemeral test port found in [10000, 30000)");
}

pub fn scratch_root() -> PathBuf {
    // Short: the Unix socket path must stay under 103 bytes.
    let root = std::env::var("PGX_TEST_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/pgxt"));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn immutable_cli() -> &'static Path {
    CLI_BINARY.get_or_init(|| {
        // Finish the sole executable copy before ANY harness can spawn a CLI.
        // With per-harness copies, another test's fork can inherit a writable
        // descriptor while the copy is in flight. That child holds the writer
        // until exec closes CLOEXEC descriptors, causing transient ETXTBSY even
        // after the copying parent has closed its own descriptor.
        let path = scratch_root().join(format!(".pgx-cli-{}", std::process::id()));
        let _ = std::fs::remove_file(&path); // an exited process may have reused this PID
        std::fs::copy(env!("CARGO_BIN_EXE_pgx"), &path).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    })
}

impl Pgx {
    /// A fresh harness: prefix and home created, nothing initialised yet.
    pub fn new() -> Pgx {
        let dist = dist_prefix();
        let id = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let base = scratch_root().join(id);
        let prefix = base.join("p");
        let home = base.join("h");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        // The cache and prefixes share a scratch filesystem, so hard links also
        // work when the build target lives on a different volume. Each link
        // still has its own prefix for installation autodiscovery.
        std::fs::hard_link(immutable_cli(), prefix.join("bin/pgx")).unwrap();
        std::os::unix::fs::symlink(dist.join("bin/postgres"), prefix.join("bin/postgres")).unwrap();
        std::os::unix::fs::symlink(dist.join("share"), prefix.join("share")).unwrap();
        let (port, lease) = reserve_port();
        Pgx {
            prefix,
            home,
            port,
            _port_lease: lease,
        }
    }

    pub fn with_home(home: PathBuf) -> Pgx {
        let mut p = Pgx::new();
        std::fs::create_dir_all(&home).unwrap();
        p.home = home;
        p
    }

    pub fn bin(&self) -> PathBuf {
        self.prefix.join("bin/pgx")
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.bin());
        c.args(args)
            .env("PGX_HOME", &self.home)
            // Poll TTL cleanup every second in tests so expiry is prompt.
            .env("PGX_TTL_INTERVAL", "1")
            .env_remove("PGPORT")
            .env_remove("PGHOST");
        c
    }

    /// The background TTL supervisor's pid for this home, if it recorded one.
    pub fn supervisor_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.home.join("supervisor.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// True when a supervisor process recorded in this home is actually alive.
    pub fn supervisor_running(&self) -> bool {
        match self.supervisor_pid() {
            Some(pid) => (unsafe { libc::kill(pid as i32, 0) }) == 0,
            None => false,
        }
    }

    /// Poll until this runtime's TCP port is bindable again (released by a
    /// stopped engine), up to `secs`. Lets a stop/start test rebind the same
    /// port deterministically without racing the kernel's socket teardown under
    /// heavy parallel load.
    pub fn wait_port_free(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if port_is_free(self.port) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// Poll until the supervisor is alive, up to `secs`.
    pub fn wait_supervisor(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if self.supervisor_running() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// Run a prepared command, retrying only a failure to SPAWN the process
    /// (never a non-zero exit, which is a real result): under heavy CI
    /// parallelism fork/exec can transiently fail — EAGAIN, or ETXTBSY on the
    /// freshly copied pgx binary ("Text file busy"). The transient clears in
    /// milliseconds.
    fn output_retrying(&self, make: impl Fn() -> Command, what: &str) -> Run {
        let mut last = None;
        for _ in 0..8 {
            match make().output() {
                Ok(o) => return Run::from(o),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        panic!("could not spawn {what} after retries: {}", last.unwrap());
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.output_retrying(|| self.cmd(args), "pgx")
    }

    pub fn init(&self) -> Run {
        let port = self.port.to_string();
        self.run(&["init", "--port", &port])
    }

    /// `pgx run [extra...]` with PGX_PORT pinned to this harness's free port, so
    /// a fresh run initialises there instead of scanning 5432-5500 (and colliding
    /// with a real PostgreSQL or a sibling test).
    pub fn run_cmd(&self, extra: &[&str]) -> Run {
        let mut args = vec!["run"];
        args.extend_from_slice(extra);
        let result = self.output_retrying(
            || {
                let mut c = self.cmd(&args);
                c.env("PGX_PORT", self.port.to_string());
                c
            },
            "pgx run",
        );
        if result.code == 4 {
            eprintln!("{}", self.port_diagnostics());
        }
        result
    }

    pub fn port_diagnostics(&self) -> String {
        use std::os::unix::fs::MetadataExt;
        let mut parts = vec![format!(
            "port {} conflict, test pid {}, home {}",
            self.port,
            std::process::id(),
            self.home.display()
        )];
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(text) = std::fs::read_to_string(table) {
                let port = format!(":{:04X}", self.port);
                parts.extend(
                    text.lines()
                        .filter(|l| l.contains(&port))
                        .map(str::to_string),
                );
            }
        }
        let lease = scratch_root().join(format!("ports/{}.lock", self.port));
        if let (Ok(metadata), Ok(locks)) = (
            std::fs::metadata(lease),
            std::fs::read_to_string("/proc/locks"),
        ) {
            let inode = format!(":{} ", metadata.ino());
            parts.extend(
                locks
                    .lines()
                    .filter(|l| l.contains(&inode))
                    .map(str::to_string),
            );
        }
        #[cfg(target_os = "linux")]
        let output = Command::new("ss")
            .args(["-tanp", &format!("sport = :{}", self.port)])
            .output();
        #[cfg(not(target_os = "linux"))]
        let output = Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{}", self.port)])
            .output();
        if let Ok(output) = output {
            parts.push(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        parts.join("\n")
    }

    /// Spawn `pgx run` without waiting — for concurrency tests.
    pub fn spawn_run(&self) -> std::process::Child {
        let mut last = None;
        for _ in 0..8 {
            match self
                .cmd(&["run"])
                .env("PGX_PORT", self.port.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => return c,
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        panic!("could not spawn pgx run after retries: {}", last.unwrap());
    }

    pub fn init_ok(&self) {
        let r = self.init();
        assert_eq!(r.code, 0, "pgx init failed: {}", r.all());
    }

    pub fn start_ok(&self) {
        let r = self.run(&["start"]);
        assert_eq!(
            r.code,
            0,
            "pgx start failed: {}\n{}",
            r.all(),
            self.log_tail()
        );
    }

    pub fn stop_ok(&self) {
        let r = self.run(&["stop"]);
        assert_eq!(r.code, 0, "pgx stop failed: {}", r.all());
        assert!(
            self.wait_port_free(10),
            "stop did not release port {}",
            self.port
        );
    }

    pub fn log_tail(&self) -> String {
        std::fs::read_to_string(self.home.join("log/pgx.log"))
            .map(|s| {
                s.lines()
                    .rev()
                    .take(25)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    pub fn config(&self) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.home.join("config.json")).expect("config.json"),
        )
        .unwrap()
    }

    pub fn password(&self) -> String {
        self.config()["password"].as_str().unwrap().to_string()
    }

    pub fn sock_dir(&self) -> PathBuf {
        self.config()["run_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join("run"))
    }

    /// Any `postgres` process whose command line names this home's data dir.
    pub fn engine_pids(&self) -> Vec<u32> {
        let out = Command::new("pgrep")
            .args([
                "-f",
                "--",
                &format!("postgres .*-D {}( |$)", self.home.join("data").display()),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    /// Runs SQL through psql if available, else through `pgx`'s own client via a tiny helper.
    pub fn psql(&self, db: &str, sql: &str) -> Run {
        let psql = find_psql();
        let out = Command::new(psql)
            .args([
                "-X",
                "-q",
                "-v",
                "ON_ERROR_STOP=1",
                "-h",
                &self.sock_dir().to_string_lossy(),
                "-p",
                &self.port.to_string(),
                "-U",
                "postgres",
                "-d",
                db,
                "-Atc",
                sql,
            ])
            .output()
            .expect("spawn psql");
        Run::from(out)
    }

    pub fn psql_tcp(&self, db: &str, password: &str, sql: &str) -> Run {
        let psql = find_psql();
        let out = Command::new(psql)
            .args([
                "-X",
                "-q",
                "-v",
                "ON_ERROR_STOP=1",
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "postgres",
                "-d",
                db,
                "-Atc",
                sql,
            ])
            .env("PGPASSWORD", password)
            .output()
            .expect("spawn psql");
        Run::from(out)
    }
}

pub fn find_psql() -> PathBuf {
    if let Ok(p) = std::env::var("PGX_TEST_PSQL") {
        return PathBuf::from(p);
    }
    for cand in [
        "/opt/homebrew/opt/postgresql@18/bin/psql",
        "/usr/lib/postgresql/18/bin/psql",
        "/usr/local/pgsql/bin/psql",
    ] {
        if Path::new(cand).exists() {
            return PathBuf::from(cand);
        }
    }
    PathBuf::from("psql")
}

impl Drop for Pgx {
    fn drop(&mut self) {
        let failed = std::thread::panicking();
        if failed {
            eprintln!(
                "preserving failed test home {} (port {})\n{}",
                self.home.display(),
                self.port,
                self.log_tail()
            );
        }
        // Stop the TTL supervisor first so it does not linger after the home is gone.
        if let Some(pid) = self.supervisor_pid() {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        let _ = self.cmd(&["stop", "--force"]).output();
        for pid in self.engine_pids() {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        if !failed && std::env::var("PGX_TEST_KEEP").is_err() {
            if let Some(base) = self.prefix.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Run {
    pub fn all(&self) -> String {
        format!(
            "exit {}\nstdout: {}\nstderr: {}",
            self.code, self.stdout, self.stderr
        )
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.all()))
    }
}
