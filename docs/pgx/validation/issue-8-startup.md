# Startup flake validation (#8)

The original native ARM64 failure is in [CI attempt 1, job 117739569836](https://github.com/pgrundev/pgx/actions/runs/39149334607/attempts/1): `run_defaults_are_localhost_only_and_password_protected` returned exit 4 (conflict), while the other seven run tests passed. The failed assertion did not print the command's stderr, so the original conflicting address or process cannot be identified from that log.

The harness now holds a port lease across processes for each runtime's whole lifetime, checks both IPv4 and IPv6 as `pgx init` does, and avoids the operating system's ephemeral client-port range. This also covers the socket-only password bootstrap, when a simple TCP bind probe cannot see another harness's pending reservation. Stop/start tests wait for both loopback listeners to become bindable. Failed tests print command output, retain engine logs, and upload those logs in CI.

The additional end-to-end test starts eight independent runtimes together, then checks an authenticated TCP `SELECT 1` on each and verifies that all eight ports differ. The original authentication, idempotency, concurrency, persistence, and crash-recovery assertions remain enabled.

Run the repeated gate against an assembled distribution:

```sh
PGX_DIST=/path/to/assembled/prefix PGX_RUN_REPETITIONS=20 sh tests/pgx/repeat_run.sh
```

Recorded on Linux x86_64, 2026-10-22, with Rust 1.96.0 and PostgreSQL client 18.6:

- 20 consecutive run-suite repetitions passed: 180 tests, zero failures, eight test threads per repetition.
- Engine: published PGX v0.1.0 linux-amd64 release; SHA-256 of `bin/postgres`: `8c2d0ce2967e34fefcb044f35e3d8b94401e674ad2f1e9e6e97b6d73b128b8be`.
- CLI: built from this change and copied into each isolated test installation.
- The other suites sharing this harness passed: clone 7/7, databases 10/10, doctor 7/7, lifecycle 26/26, TTL 10/10 (60 tests total), using `cargo test -p pgx --release --test lifecycle --test databases --test clone --test doctor --test ttl`.
- `cargo clippy -p pgx --all-targets -- -D warnings` passed.

The PR CI runs the same 20-repetition gate on `ubuntu-24.04-arm`. The local results above are x86_64 evidence; native ARM64 acceptance requires that CI step to pass.
