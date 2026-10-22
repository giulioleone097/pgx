//! Black-box TTL tests against an assembled distribution: real databases, real
//! background expiry by the supervisor (no manual drop), restart-resumed
//! cleanup, extension/disable, non-inheritance on clone, and the identity gate
//! that stops a stale record from deleting a reused name. Cleanup is observed
//! with bounded polling, never a bare sleep-as-assertion.
mod common;
use common::Pgx;
use std::time::{Duration, Instant};

fn db_exists(p: &Pgx, name: &str) -> bool {
    let result = p.psql(
        "postgres",
        &format!("SELECT count(*) FROM pg_database WHERE datname='{name}'"),
    );
    assert_eq!(result.code, 0, "database lookup failed: {}", result.all());
    result.stdout.trim() == "1"
}

/// Poll until `name` is gone, up to `secs`. Returns true if it disappeared.
fn wait_gone(p: &Pgx, name: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if !db_exists(p, name) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    false
}

fn up(p: &Pgx) {
    // `pgx run` is the real onboarding path: it initialises, starts the runtime,
    // spawns the TTL supervisor, and creates the default `app` database.
    let r = p.run_cmd(&[]);
    assert_eq!(r.code, 0, "pgx run failed: {}\n{}", r.all(), p.log_tail());
}

#[test]
fn create_ttl_expires_while_permanent_survives() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "permanent"]).code, 0);
    assert_eq!(p.run(&["create", "temp5", "--ttl", "3s"]).code, 0);
    // Both exist immediately; temp5 carries a TTL, permanent does not.
    assert!(db_exists(&p, "permanent") && db_exists(&p, "temp5"));
    let ls = p.run(&["ls", "--json"]).json();
    let temp = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "temp5")
        .unwrap();
    assert_eq!(temp["ttl"], "3s");
    assert!(temp["expires_at"].is_string());
    // Supervisor removes temp5 automatically — no `pgx drop` is ever run here.
    assert!(
        wait_gone(&p, "temp5", 20),
        "temp5 must expire automatically:\n{}",
        p.log_tail()
    );
    assert!(db_exists(&p, "permanent"), "permanent must survive");
    assert!(db_exists(&p, "app"), "app must survive");
}

#[test]
fn clone_ttl_expires_and_source_is_preserved() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "src"]).code, 0);
    p.psql("src", "CREATE TABLE d(v int); INSERT INTO d VALUES (7)");
    assert_eq!(p.run(&["clone", "src", "clone5", "--ttl", "3s"]).code, 0);
    assert_eq!(p.psql("clone5", "SELECT v FROM d").stdout.trim(), "7");
    assert!(
        wait_gone(&p, "clone5", 20),
        "clone must expire:\n{}",
        p.log_tail()
    );
    assert!(db_exists(&p, "src"), "source must remain");
    assert_eq!(
        p.psql("src", "SELECT v FROM d").stdout.trim(),
        "7",
        "source data must be unchanged"
    );
}

#[test]
fn ttl_extension_prevents_the_original_expiry() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "extend-me", "--ttl", "3s"]).code, 0);
    assert_eq!(p.run(&["ttl", "extend-me", "1h"]).code, 0);
    // Wait well past the original 3s deadline plus several supervisor cycles.
    std::thread::sleep(Duration::from_secs(7));
    assert!(
        db_exists(&p, "extend-me"),
        "an extended database must outlive its original deadline"
    );
    assert_eq!(p.run(&["--json", "ttl", "extend-me"]).json()["ttl"], "1h");
}

#[test]
fn ttl_unlimited_disables_expiry() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "keep-me", "--ttl", "3s"]).code, 0);
    assert_eq!(p.run(&["ttl", "keep-me", "unlimited"]).code, 0);
    std::thread::sleep(Duration::from_secs(7));
    assert!(
        db_exists(&p, "keep-me"),
        "a database set back to unlimited must survive its original deadline"
    );
    let show = p.run(&["--json", "ttl", "keep-me"]).json();
    assert_eq!(show["ttl"], "unlimited");
    assert!(show["expires_at"].is_null());
}

#[test]
fn ttl_cleanup_resumes_after_restart() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "permanent"]).code, 0);
    assert_eq!(p.run(&["create", "short", "--ttl", "3s"]).code, 0);
    // Stop PGX, let the deadline pass while stopped, then start again.
    p.stop_ok();
    std::thread::sleep(Duration::from_secs(5));
    p.start_ok();
    assert!(
        wait_gone(&p, "short", 20),
        "cleanup must resume on restart and remove the expired database:\n{}",
        p.log_tail()
    );
    assert!(db_exists(&p, "permanent"), "permanent must be untouched");
}

#[test]
fn default_create_is_unlimited_and_never_expires() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "plain"]).code, 0);
    // No TTL record is written for an unlimited database.
    assert!(!p.home.join("ttl").join(hex("plain")).exists());
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        db_exists(&p, "plain"),
        "a default database must never expire"
    );
    assert_eq!(
        p.run(&["--json", "ttl", "plain"]).json()["ttl"],
        "unlimited"
    );
}

#[test]
fn ttl_is_not_inherited_on_clone() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "tsrc", "--ttl", "30s"]).code, 0);
    // Clone without --ttl: the child must be unlimited, not inherit 30s.
    assert_eq!(p.run(&["clone", "tsrc", "child"]).code, 0);
    assert_eq!(
        p.run(&["--json", "ttl", "child"]).json()["ttl"],
        "unlimited",
        "a clone must not inherit the source TTL"
    );
    assert_eq!(p.run(&["--json", "ttl", "tsrc"]).json()["ttl"], "30s");
}

#[test]
fn stale_ttl_record_never_deletes_a_reused_name() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "reuse", "--ttl", "3s"]).code, 0);
    // Drop and recreate OUTSIDE pgx, so the TTL record (old OID) is left stale
    // while a brand-new database reuses the name with a different OID.
    p.psql("postgres", "DROP DATABASE reuse");
    p.psql("postgres", "CREATE DATABASE reuse");
    p.psql(
        "reuse",
        "CREATE TABLE keep(v int); INSERT INTO keep VALUES (1)",
    );
    // Past the stale deadline and several supervisor cycles.
    std::thread::sleep(Duration::from_secs(7));
    assert!(
        db_exists(&p, "reuse"),
        "a reused name must not be deleted by a stale TTL record:\n{}",
        p.log_tail()
    );
    assert_eq!(
        p.psql("reuse", "SELECT v FROM keep").stdout.trim(),
        "1",
        "the replacement database's data must be intact"
    );
}

#[test]
fn many_temporary_branches_expire_on_one_runtime() {
    let p = Pgx::new();
    up(&p);
    assert_eq!(p.run(&["create", "base"]).code, 0);
    // 12 clones with a short TTL, all on the single shared runtime.
    for i in 0..12 {
        let name = format!("br{i}");
        assert_eq!(p.run(&["clone", "base", &name, "--ttl", "3s"]).code, 0);
    }
    assert_eq!(
        p.engine_pids().len(),
        1,
        "there must be exactly one shared engine, not one per branch"
    );
    // All expire automatically; base and app remain.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = (0..12).filter(|i| db_exists(&p, &format!("br{i}"))).count();
        if remaining == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{remaining} temporary branches still present:\n{}",
            p.log_tail()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(db_exists(&p, "base") && db_exists(&p, "app"));
    // Protected system databases must survive mass expiration. Their names
    // (`template0`/`template1`) lexically resemble a temp-branch prefix, so this
    // pins that the supervisor skips them by identity, never by name matching,
    // even when many deletions fire at once.
    for sys in ["postgres", "template0", "template1"] {
        assert!(
            db_exists(&p, sys),
            "protected system database {sys} must never be auto-deleted"
        );
    }
}

/// Hex-encoded metadata filename, matching ttlstore's scheme.
fn hex(name: &str) -> String {
    let mut s = String::new();
    for b in name.bytes() {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str(".json");
    s
}

#[test]
fn stop_stops_the_supervisor_and_restart_respawns_it() {
    // The supervisor is tied to the runtime's lifetime: `pgx stop` must stop it
    // promptly (not only when it next notices the engine is down), so a stale
    // supervisor can never hold the single-instance lock across a restart (which
    // left no supervisor running after a PGX_HOME relocation). A later start must
    // bring a fresh supervisor back and cleanup must resume.
    let p = Pgx::new();
    up(&p);
    assert!(
        p.wait_supervisor(10),
        "supervisor should be running after `pgx run`"
    );
    p.stop_ok();
    assert!(
        !p.supervisor_running(),
        "`pgx stop` must stop the TTL supervisor, not leave it polling:\n{}",
        p.log_tail()
    );
    // Stop frees the port; wait for the kernel to release it before rebinding
    // (deterministic under parallel load — not a weaker assertion).
    assert!(p.wait_port_free(10), "stop should free the runtime port");
    p.start_ok();
    assert!(
        p.wait_supervisor(10),
        "`pgx start` must respawn the supervisor:\n{}",
        p.log_tail()
    );
    // Cleanup actually works again after the restart.
    assert_eq!(p.run(&["create", "afterrestart", "--ttl", "3s"]).code, 0);
    assert!(
        wait_gone(&p, "afterrestart", 20),
        "cleanup must resume after restart:\n{}",
        p.log_tail()
    );
}

fn wait_replacement(p: &Pgx, old: u32) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(pid) = p.supervisor_pid() {
            if pid != old && p.supervisor_running() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "worker did not recover:\n{}",
            p.log_tail()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn killed_supervisor_recovers_without_another_cli_invocation() {
    let p = Pgx::new();
    up(&p);
    assert!(p.wait_supervisor(10));
    let engines = p.engine_pids();
    assert_eq!(p.run(&["create", "permanent"]).code, 0);
    assert_eq!(p.run(&["create", "recover", "--ttl", "8s"]).code, 0);
    let mut pid = p.supervisor_pid().unwrap();
    // No CLI calls between SIGKILL and recovery: the background watchdog must
    // heal on its own. Repeat to reject a one-shot recovery implementation.
    for _ in 0..3 {
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
        pid = wait_replacement(&p, pid);
    }
    assert!(
        wait_gone(&p, "recover", 20),
        "expired database survived recovery:\n{}",
        p.log_tail()
    );
    assert_eq!(
        p.engine_pids(),
        engines,
        "worker recovery restarted the engine"
    );
    for name in ["permanent", "app", "postgres", "template0", "template1"] {
        assert!(db_exists(&p, name), "recovery deleted {name}");
    }
    // Concurrent callers must leave the same singleton worker in charge.
    let callers: Vec<_> = (0..4).map(|_| p.cmd(&["start"]).spawn().unwrap()).collect();
    for mut child in callers {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(p.supervisor_pid(), Some(pid));
    let watchdog = std::fs::read_to_string(p.home.join("watchdog.pid")).unwrap();
    let watchdog: i32 = watchdog.trim().parse().unwrap();
    p.stop_ok();
    assert!(!p.home.join("watchdog.pid").exists());
    // kill(0) sees unreaped zombies in bare containers; the removed pidfile and
    // unlocked lock are the lifecycle authority after graceful shutdown.
    let lock = std::fs::File::open(p.home.join("watchdog.lock")).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "watchdog {watchdog} retained its lock after stop"
    );
    assert!(!p.supervisor_running());
}

#[test]
fn serve_worker_crash_preserves_an_existing_session_and_heals() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let p = Pgx::new();
    p.init_ok();
    let mut serve = p
        .cmd(&["serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(p.wait_supervisor(15), "serve did not launch its worker");
    assert_eq!(p.run(&["create", "serve_expiry", "--ttl", "8s"]).code, 0);
    let engines = p.engine_pids();
    let mut session = Command::new(common::find_psql())
        .args([
            "-X",
            "-qAt",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            &p.sock_dir().to_string_lossy(),
            "-p",
            &p.port.to_string(),
            "-U",
            "postgres",
            "-d",
            "postgres",
        ])
        .env("PGOPTIONS", "-c statement_timeout=10000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = session.stdin.take().unwrap();
    let mut output = BufReader::new(session.stdout.take().unwrap());
    writeln!(input, "BEGIN; SELECT 'connected';").unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "connected");
    let old = p.supervisor_pid().unwrap();
    assert_eq!(unsafe { libc::kill(old as i32, libc::SIGKILL) }, 0);
    wait_replacement(&p, old);
    writeln!(input, "SELECT 42; COMMIT;").unwrap();
    drop(input);
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        line.trim(),
        "42",
        "the existing transaction was disconnected"
    );
    assert!(session.wait().unwrap().success());
    assert_eq!(p.engine_pids(), engines);
    assert!(wait_gone(&p, "serve_expiry", 20));
    // The foreground monitor must still forward container shutdown signals.
    assert_eq!(unsafe { libc::kill(serve.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = serve.try_wait().unwrap() {
            assert!(status.success(), "serve shutdown failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            let _ = serve.kill();
            let _ = serve.wait();
            panic!("serve did not forward SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(p.engine_pids().is_empty());
    assert!(!p.supervisor_running());
}
