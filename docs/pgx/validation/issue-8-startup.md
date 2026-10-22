# Startup flake validation (#8)

The original native ARM64 failure is in [CI attempt 1, job 117739569836](https://github.com/pgrundev/pgx/actions/runs/39149334607/attempts/1): `run_defaults_are_localhost_only_and_password_protected` returned exit 4 (conflict), while the other seven run tests passed. The failed assertion did not print the command's stderr, so the original conflicting address or process cannot be identified from that log.

Concurrent forks can inherit temporary resources from another test thread. An inherited writable executable descriptor produces `ETXTBSY` even after the copying parent closes it. An inherited port-probe listener produces the original exit-4 error even after the probing parent closes it. Both mechanisms were reproduced deterministically with the real CLI; the precise holder in the original ARM64 log remains unknown.

The harness initializes one immutable CLI copy before starting commands, then hardlinks it into each test installation. Both loopback bind probes run in a separate single-thread process, whose socket descriptors cannot be inherited by sibling harness commands. Each runtime also holds a cross-process port lease for its whole lifetime and avoids the operating system's ephemeral client-port range. This covers the socket-only password bootstrap, when a simple TCP bind probe cannot see another harness's pending reservation. Stop/start tests wait for both loopback listeners to become bindable. Failed tests print command output, retain engine logs, and upload those logs in CI.

One additional end-to-end test starts eight independent runtimes together, then checks an authenticated TCP `SELECT 1` on each and verifies that all eight ports differ. Another executes 128 direct CLI commands across eight threads while creating test installations; these commands bypass spawn retries. The original authentication, idempotency, concurrency, persistence, and crash-recovery assertions remain enabled.

Run the repeated gate against an assembled distribution:

```sh
PGX_DIST=/path/to/assembled/prefix PGX_RUN_REPETITIONS=20 sh tests/pgx/repeat_run.sh
PGX_DIST=/path/to/assembled/prefix python3 -I tests/pgx/startup_resource_proof.py
```

Recorded on Linux x86_64, 2026-10-22, with Rust 1.96.0 and PostgreSQL client 18.6:

- Final 20 consecutive run-suite repetitions passed: 200 tests, zero failures, eight test threads per repetition, including 2,560 direct CLI executions without spawn retries.
- The deterministic resource proof produced exit 4 with a closed parent probe whose listener was inherited by a child. With the probe isolated in a separate process, init and start succeeded and authenticated TCP `SELECT 1` returned `1` while the sibling child was still alive.
- Engine: published PGX v0.1.0 linux-amd64 release; SHA-256 of `bin/postgres`: `8c2d0ce2967e34fefcb044f35e3d8b94401e674ad2f1e9e6e97b6d73b128b8be`.
- CLI: built from this change and copied into each isolated test installation.
- The other suites sharing this harness passed: clone 7/7, databases 10/10, doctor 7/7, lifecycle 26/26, TTL 10/10 (60 tests total), using `cargo test -p pgx --release --test lifecycle --test databases --test clone --test doctor --test ttl`. Lifecycle 26/26 passed again after the executable-copy and probe-isolation fixes.
- `cargo clippy -p pgx --all-targets -- -D warnings` passed.

The PR CI runs the same 20-repetition gate on `ubuntu-24.04-arm`. The local results above are x86_64 evidence; native ARM64 acceptance requires that CI step to pass.
