//! Real-engine Linux regressions for the TTL lock's queued-operation races.
//! /proc/<pid>/wchan establishes that both CLI processes are waiting on flock,
//! rather than relying on a sleep to guess the supervisor's execution point.
#![cfg(target_os = "linux")]
mod common;
use common::{Pgx, Run};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ManagedChild(Option<Child>);

impl ManagedChild {
    fn spawn(mut command: Command) -> Self {
        Self(Some(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    fn pid(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }

    fn finish(mut self) -> Run {
        Run::from(self.0.take().unwrap().wait_with_output().unwrap())
    }

    fn waiting_on(&self, what: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{}/wchan", self.pid()))
            .map(|w| w.contains(what))
            .unwrap_or(false)
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for(mut check: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {label}");
}

fn query(p: &Pgx, sql: &str) -> String {
    let result = p.psql("postgres", sql);
    assert_eq!(result.code, 0, "{}", result.all());
    result.stdout.trim().to_string()
}

fn stop_automatic_supervisor(p: &Pgx) {
    assert!(p.wait_supervisor(5));
    // The self-healing runtime has a separate watchdog. Stop it first so this
    // test can deliberately control exactly when a replacement worker starts.
    if let Ok(text) = std::fs::read_to_string(p.home.join("watchdog.pid")) {
        let pid = text.trim().parse::<i32>().unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(p.home.join("watchdog.lock"))
            .unwrap();
        wait_for(
            || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "watchdog releases its singleton lock",
        );
    }
    // A managed watchdog reaps its child and removes supervisor.pid as part of
    // shutdown; the standalone audit worker still needs an explicit signal.
    if let Some(pid) = p.supervisor_pid() {
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(p.home.join("supervisor.lock"))
        .unwrap();
    wait_for(
        || unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "automatic supervisor releases its singleton lock",
    );
}

fn queued_update_preserves_database(reincarnate: bool) {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    stop_automatic_supervisor(&p);
    let created = p.run(&["create", "race", "--ttl", "1s"]);
    assert_eq!(created.code, 0, "{}", created.all());
    let record = p.home.join("ttl/72616365.json"); // UTF-8 hex of "race"
    let old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    wait_for(
        || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                >= old["expires_at"].as_u64().unwrap()
        },
        "the original TTL expires while its supervisor is stopped",
    );
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(p.home.join("ttl-locks/72616365.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let updater =
        ManagedChild::spawn(p.cmd(&["ttl", "race", if reincarnate { "1h" } else { "unlimited" }]));
    wait_for(|| updater.waiting_on("lock"), "TTL updater queues first");
    let supervisor = ManagedChild::spawn(p.cmd(&["supervise", &p.home.to_string_lossy()]));
    wait_for(
        || supervisor.waiting_on("lock"),
        "supervisor queues after reading the expired record",
    );
    let new_oid = if reincarnate {
        query(&p, "DROP DATABASE race");
        query(&p, "CREATE DATABASE race");
        let oid = query(&p, "SELECT oid FROM pg_database WHERE datname='race'")
            .parse::<u64>()
            .unwrap();
        assert_ne!(old["oid"].as_u64().unwrap(), oid);
        Some(oid)
    } else {
        None
    };
    // The earlier queued CLI update completes, then the supervisor acquires the
    // same lock with its OLD snapshot. It must re-read the current record.
    drop(held);
    let updated = updater.finish();
    assert_eq!(updated.code, 0, "{}", updated.all());
    wait_for(
        || supervisor.waiting_on("nanosleep"),
        "supervisor completes the queued cleanup cycle",
    );
    assert_eq!(
        query(&p, "SELECT count(*) FROM pg_database WHERE datname='race'"),
        "1",
        "acknowledged TTL update must preserve the database"
    );
    let current = p.run(&["--json", "ttl", "race"]).json();
    if let Some(oid) = new_oid {
        assert_eq!(current["ttl"], "1h");
        let metadata: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(record).unwrap()).unwrap();
        assert_eq!(metadata["oid"].as_u64().unwrap(), oid);
    } else {
        assert_eq!(current["ttl"], "unlimited");
        assert!(!record.exists(), "cancellation must remain committed");
    }
}

#[test]
fn queued_unlimited_update_cancels_the_supervisors_expired_snapshot() {
    queued_update_preserves_database(false);
}

#[test]
fn queued_reincarnation_keeps_the_new_database_ttl_metadata() {
    queued_update_preserves_database(true);
}

#[test]
fn competing_supervisor_exits_without_replacing_the_singleton() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert!(p.wait_supervisor(5));
    let original = p.supervisor_pid().unwrap();
    let second = ManagedChild::spawn(p.cmd(&["supervise", &p.home.to_string_lossy()]));
    let result = second.finish();
    assert_eq!(result.code, 0, "{}", result.all());
    assert_eq!(p.supervisor_pid(), Some(original));
    assert!(p.supervisor_running());
    assert_eq!(p.run(&["create", "victim", "--ttl", "1s"]).code, 0);
    assert_eq!(p.run(&["create", "keep"]).code, 0);
    wait_for(
        || {
            query(
                &p,
                "SELECT count(*) FROM pg_database WHERE datname='victim'",
            ) == "0"
        },
        "the original supervisor deletes the expired database",
    );
    assert_eq!(
        query(&p, "SELECT count(*) FROM pg_database WHERE datname='keep'"),
        "1"
    );
    assert_eq!(p.supervisor_pid(), Some(original));
}
