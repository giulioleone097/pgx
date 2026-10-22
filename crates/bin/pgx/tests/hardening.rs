//! Audit regressions exercised through the real CLI and engine.
mod common;
use common::Pgx;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

fn exists(p: &Pgx, name: &str) -> bool {
    p.psql(
        "postgres",
        &format!("SELECT count(*) FROM pg_database WHERE datname='{name}'"),
    )
    .stdout
    .trim()
        == "1"
}

fn record_path(p: &Pgx, name: &str) -> std::path::PathBuf {
    p.home.join("ttl").join(format!(
        "{}.json",
        name.bytes().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

#[test]
fn stale_supervisor_pid_never_signals_an_unrelated_process() {
    let p = Pgx::new();
    p.init_ok();
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    std::fs::write(p.home.join("supervisor.pid"), child.id().to_string()).unwrap();
    // Even a held lock is insufficient without matching executable/home argv.
    let lock = std::fs::File::create(p.home.join("supervisor.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let stopped = p.run(&["stop"]);
    let survived = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(stopped.code, 0, "{}", stopped.all());
    assert!(survived, "pgx stop must never signal a reused foreign PID");
}

#[test]
fn socket_and_home_are_private_and_symlink_override_is_rejected() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    for path in [
        &p.home,
        &p.sock_dir(),
        &p.sock_dir().join(format!(".s.PGSQL.{}", p.port)),
    ] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700,
            "{}",
            path.display()
        );
    }
    let other = Pgx::new();
    let real = other.home.join("real");
    std::fs::create_dir(&real).unwrap();
    let link = other.home.join("linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let result = other
        .cmd(&["init", "--port", &other.port.to_string()])
        .env("PGX_RUN_DIR", link)
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "symlink socket dir must fail closed"
    );
}

#[test]
fn failed_ttl_persistence_rolls_back_create_and_clone() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "source"]).code, 0);
    // Keep lock directory usable but make the metadata directory unwritable as
    // a directory, including when these tests run as root.
    std::fs::write(p.home.join("ttl"), "not a directory").unwrap();
    for args in [
        vec!["create", "failed", "--ttl", "1h"],
        vec!["clone", "source", "failed", "--ttl", "1h"],
    ] {
        let result = p.run(&args);
        assert_ne!(result.code, 0, "{}", result.all());
        assert!(result.all().contains("rolled back"), "{}", result.all());
        assert!(
            !exists(&p, "failed"),
            "failed create/clone must leave no permanent database"
        );
        assert!(exists(&p, "source"));
    }
}

#[test]
fn stale_metadata_never_describes_a_reused_database_and_drop_cleans_it() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "reuse", "--ttl", "1h"]).code, 0);
    let dropped = p.psql("postgres", "DROP DATABASE reuse");
    assert_eq!(dropped.code, 0, "{}", dropped.all());
    let recreated = p.psql("postgres", "CREATE DATABASE reuse");
    assert_eq!(recreated.code, 0, "{}", recreated.all());
    assert_eq!(
        p.run(&["--json", "ttl", "reuse"]).json()["ttl"],
        "unlimited"
    );
    let list = p.run(&["--json", "ls"]).json();
    assert_eq!(
        list.as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "reuse")
            .unwrap()["ttl"],
        "unlimited"
    );
    assert_eq!(p.run(&["drop", "reuse"]).code, 0);
    assert!(!record_path(&p, "reuse").exists());
    assert_eq!(p.run(&["create", "old", "--ttl", "1h"]).code, 0);
    p.stop_ok();
    assert_eq!(
        p.run(&["init", "--force", "--port", &p.port.to_string()])
            .code,
        0
    );
    assert!(
        !p.home.join("ttl").exists(),
        "force init removes the previous cluster's metadata"
    );
}

#[test]
fn canonical_home_identity_keeps_the_live_pidfile() {
    let p = Pgx::new();
    p.init_ok();
    let alias = p.prefix.join("home-link");
    std::os::unix::fs::symlink(&p.home, &alias).unwrap();
    let started = p.cmd(&["start"]).env("PGX_HOME", alias).output().unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert_eq!(
        p.run(&["status"]).code,
        0,
        "canonical and alias paths identify the same running engine"
    );
    assert!(p.home.join("data/postmaster.pid").exists());
    p.stop_ok();
}

#[test]
fn home_paths_with_spaces_preserve_engine_and_supervisor_identity() {
    let mut p = Pgx::new();
    p.home = p.home.join("My PGX");
    std::fs::create_dir_all(&p.home).unwrap();
    p.init_ok();
    p.start_ok();
    assert!(p.wait_supervisor(5));
    assert_eq!(p.psql("postgres", "SELECT 42").stdout.trim(), "42");
    assert_eq!(p.run(&["status"]).code, 0);
    p.stop_ok();
    assert!(!p.home.join("supervisor.pid").exists());
}

#[test]
fn authentication_readiness_failure_is_immediate_and_keeps_engine_alive() {
    let p = Pgx::new();
    p.init_ok();
    let hba = p.home.join("data/pg_hba.conf");
    let text = std::fs::read_to_string(&hba).unwrap().replace(
        "local   all             all                                     trust",
        "local   all             all                                     reject",
    );
    // Match spacing independently of the renderer.
    let text = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("local ") {
                "local all all reject"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(hba, text).unwrap();
    let started = Instant::now();
    let result = p.run(&["start"]);
    assert_ne!(result.code, 0);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "authentication errors must not wait for the startup timeout"
    );
    assert!(result.all().contains("readiness"), "{}", result.all());
    assert!(p.home.join("data/postmaster.pid").exists());
    assert_eq!(
        p.engine_pids().len(),
        1,
        "healthy engine is preserved for authentication repair"
    );
    p.stop_ok();
}

#[test]
fn unicode_ttl_and_json_usage_errors_are_reported_without_panics() {
    let p = Pgx::new();
    let result = p.run(&["--json", "create"]);
    assert_eq!(result.code, 2);
    assert!(result.json().is_object());
    p.init_ok();
    p.start_ok();
    let result = p.run(&["create", "bad", "--ttl", "5é"]);
    assert_eq!(result.code, 2, "{}", result.all());
    assert!(!exists(&p, "bad"));
}
