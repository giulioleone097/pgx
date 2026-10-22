# TTL supervisor recovery validation

Validated on Linux amd64 on 2026-10-22 with Rust 1.96.0 and the checksum-verified
PGX v0.1.0 distribution (engine commit `d62547da71`). The harness substitutes the
CLI built from the branch while using the distribution's real engine and base
cluster. Native ARM64 and macOS checks remain the responsibility of the CI matrix.

## Automated regression coverage

```sh
cargo test -p pgx --bin pgx
cargo clippy -p pgx --all-targets -- -D warnings
PGX_DIST=/path/to/distribution PGX_TEST_PSQL=/path/to/psql \
  cargo test -p pgx --test ttl -- --test-threads=3
```

Results: 61 unit tests passed, one existing live-server unit test ignored;
Clippy passed; all 12 real-engine TTL tests passed.

`killed_supervisor_recovers_without_another_cli_invocation` kills three successive
workers with SIGKILL, observes a replacement after each kill without calling
another CLI command, and waits for the expired database to disappear. It verifies
that permanent and protected databases survive, the engine PID stays the same,
four concurrent starts retain one worker, and stop releases the watchdog lock.
Against the unmodified v0.1.0 CLI, the same test fails its eight-second recovery
bound: no replacement worker appears.

`serve_worker_crash_preserves_an_existing_session_and_heals` keeps a transaction
open while killing the cleanup worker, then runs `SELECT 42` and commits through
that same connection. It also verifies resumed expiry, unchanged engine PID,
and signal forwarding on foreground shutdown. The original foreground lifecycle
test also passes.

## Actual PID 1 container check

Both CLIs were also exercised in scratch containers, with UID/GID 10001 and
`--network=none`. Each image contained the real distribution, its dynamic runtime
libraries and psql. The fixed CLI ran `pgx serve` as PID 1; no init wrapper was
used. After opening a transaction, the test killed the PID from `supervisor.pid`,
waited for recovery, checked TTL expiry and a permanent database, then sent
SIGTERM with `docker stop`.

| Observation | v0.1.0 CLI | Fixed CLI |
|---|---|---|
| PID 1 | postgres engine | `pgx serve` monitor |
| Worker after SIGKILL | No replacement after 8.378 s | PID 31 replaced by 100 in 1.674 s |
| Expired database | Still present | Automatically dropped |
| Existing transaction | `SELECT 42` succeeds | `SELECT 42` succeeds |
| Engine PID | 1 | 22 before and after recovery |
| Permanent database | Not tested | Preserved |
| Container exit after SIGTERM | 0 | 0 |

The old release reproduced the recovery failure. It did not reproduce the
audit's proposed worker-crash connection loss in this run. The new foreground
topology gives the monitor ownership of the worker and engine separately, and
the session-continuity test guards that behavior.
