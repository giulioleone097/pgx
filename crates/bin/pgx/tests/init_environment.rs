//! The documented environment contract must survive through configuration and
//! a real running engine, including flag precedence and rejected inputs.
mod common;
use common::{Pgx, Run};
use std::process::Command;

fn command(p: &Pgx, args: &[&str]) -> Command {
    let mut c = p.cmd(args);
    c.env_remove("PGX_PORT").env_remove("PGX_DURABILITY");
    c
}

#[test]
fn init_environment_controls_the_running_engine() {
    let p = Pgx::new();
    let r = Run::from(
        command(&p, &["--json", "init"])
            .env("PGX_PORT", p.port.to_string())
            .env("PGX_DURABILITY", "full")
            .output()
            .unwrap(),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.json()["port"], p.port);
    assert_eq!(r.json()["durability"], "full");
    assert_eq!(p.config()["port"], p.port);
    assert_eq!(p.config()["durability"], "full");

    p.start_ok();
    let q = p.psql_tcp(
        "postgres",
        &p.password(),
        "SHOW port; SHOW fsync; SELECT 42",
    );
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), format!("{}\non\n42", p.port));
}

#[test]
fn init_flags_override_even_invalid_environment_values() {
    let p = Pgx::new();
    let port = p.port.to_string();
    let r = Run::from(
        command(&p, &["init", "--port", &port, "--durability", "ephemeral"])
            .env("PGX_PORT", "invalid-ignored-because-of-flag")
            .env("PGX_DURABILITY", "invalid-ignored-because-of-flag")
            .output()
            .unwrap(),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(p.config()["port"], p.port);
    assert_eq!(p.config()["durability"], "ephemeral");
    p.start_ok();
    let q = p.psql("postgres", "SHOW port; SHOW fsync");
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), format!("{}\noff", p.port));
}

#[test]
fn every_init_entrypoint_rejects_invalid_environment_before_creating_data() {
    for entrypoint in ["init", "run", "serve"] {
        for (key, values) in [
            (
                "PGX_PORT",
                vec!["", "0", "1023", "65536", "-1", "five", "5é"],
            ),
            ("PGX_DURABILITY", vec!["", "FULL", "invalid"]),
        ] {
            for value in values {
                let p = Pgx::new();
                let r = Run::from(
                    command(&p, &["--json", entrypoint])
                        .env(key, value)
                        .output()
                        .unwrap(),
                );
                assert_eq!(r.code, 2, "{entrypoint} {key}={value:?}: {}", r.all());
                assert!(
                    r.json()["error"]["message"].as_str().unwrap().contains(key),
                    "{}",
                    r.all()
                );
                assert!(!p.home.join("config.json").exists());
                assert!(!p.home.join("data").exists());
                assert!(p.engine_pids().is_empty());
            }
        }
    }
}

#[test]
fn environment_port_conflict_never_falls_back_to_another_port() {
    let p = Pgx::new();
    let _busy = std::net::TcpListener::bind(("127.0.0.1", p.port)).unwrap();
    let r = Run::from(
        command(&p, &["init"])
            .env("PGX_PORT", p.port.to_string())
            .output()
            .unwrap(),
    );
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains(&p.port.to_string()), "{}", r.all());
    assert!(!p.home.join("data").exists());
    assert!(!p.home.join("config.json").exists());
}

#[test]
fn invalid_environment_does_not_destroy_a_cluster_on_force_init() {
    let p = Pgx::new();
    p.init_ok();
    let before = std::fs::read(p.home.join("config.json")).unwrap();
    std::fs::write(p.home.join("data/keep"), "keep").unwrap();
    let r = Run::from(
        command(&p, &["init", "--force"])
            .env("PGX_PORT", "invalid")
            .output()
            .unwrap(),
    );
    assert_eq!(r.code, 2, "{}", r.all());
    assert_eq!(std::fs::read(p.home.join("config.json")).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(p.home.join("data/keep")).unwrap(),
        "keep"
    );
}
