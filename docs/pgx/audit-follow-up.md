# Audit follow-up and local validation

This report tracks issue [#17](https://github.com/pgrundev/pgx/issues/17) and its
companion issues #7, #8, and #16. The reviewed base is
`d0df77764a3f697db7b12c67a1658e08a59bdb96`. Validation ran on Linux amd64 on
2026-10-22. Native ARM64/macOS runtime checks and GitHub OIDC release execution
remain CI checks; compilation for Apple ARM64 is separately verified.

## Reproductions and results

| Contract | Before | After |
|---|---|---|
| Init environment (#16) | Requested port 59297/full; saved 5432/ephemeral | Saved 59297/full; authenticated TCP SQL reports port 59297, fsync on, synchronous_commit on, and SELECT 42 |
| Supervisor recovery (#7) | Killed supervisor stays absent; expired database remains | Real container, UID 10001, pgx serve PID 1: supervisor replaced in 1.674 s, expired database removed, existing transaction returns 42, engine PID unchanged |
| Startup test races (#8) | Inherited writable executable produces ETXTBSY; inherited probe listener produces init exit 4 | Immutable executable cache and child-only socket probes; 20 repetitions of 10 tests pass, including 2,560 direct CLI executions; authenticated TCP startup proof passes |
| Parallel memory (F-S3) | A launched worker allocates 48 MiB with a 16 MiB session limit | Actual worker returns SQLSTATE 53200 with parallel-worker error context; the same and sibling sessions remain usable |
| Autovacuum retry (F-C7) | 10,000 dead tuples remain after the old snapshot ends and no new writes arrive | Autovacuum revisits the written database and removes the dead tuples |
| TTL lifecycle | Queued old supervisor snapshot can remove a recreated database's TTL | Deterministic lock-controlled processes preserve new identity and cancellation; foreign PIDs survive stop |
| PostgreSQL regression | Ephemeral settings produce two configuration-dependent semantic classifications | Stock compatibility settings: all 231 tests reported, 219 byte-exact, 12 EXPLAIN-only, zero semantic differences; gate passes |

The final source-built engine SHA-256 is
`98b15515e14acb8b00bad2261f1e9a1ec929a67ecfc8bf97ee2a397bcad6713e`.
The final source distribution was assembled with real PostgreSQL 18.6 initdb.
Runtime tests use actual engine connections, not mocked PostgreSQL responses.

## Finding map

| Finding | Resolution and verification |
|---|---|
| F-S1 local socket access | Owned private home/run directories, unsafe ancestor and symlink rejection, socket mode 0700; real hardening E2E |
| F-S2 release authenticity | OIDC build provenance, bundle verification before extraction, exact repository/workflow/certificate and tag-or-main source restrictions; real download and fail-closed boundary tests; actual OIDC issuance awaits release CI |
| F-S3 parallel memory | Workers share the leader's budget and enforce refusal; real worker/53200/persistent-session E2E |
| F-S4 CI trust | SHA-pinned actions, least privilege per job, release inputs passed through environment, immutable build-source references; actionlint and zizmor pass |
| F-S5 mint caps | Shipped profile caps minting at 100 per role; cap and connection-limit E2E return 53400/53300; template ownership remains control-plane policy |
| F-S6 malformed wire frames | Bounds-checked authentication, row descriptions and rows; malformed-frame unit coverage |
| F-S7 installer | Curl-only HTTPS and redirect policy, validated members and complete hardlink targets, unrelated executable preservation; 14 security tests including real TLS downgrade oracle |
| F-C1 stale supervisor PID | Held lock, executable and canonical home/argv proof before signaling; foreign process survives even with a held lock |
| F-C2 TTL cancellation race | Per-name lock across mutation and complete expiry, reload after acquiring it; real queued cancellation/reincarnation tests |
| F-C3 TTL persistence failure | Compensating DROP on returned metadata persistence errors; create and clone rollback E2E |
| F-C4 stale TTL records | OID/cluster checks, removal on drop/force-init, stale-cluster cleanup; drop/recreate E2E |
| F-C5 process identity | Canonical paths, raw Linux/macOS argv, force-init coordination locks, conservative handling of unknown live PIDs; symlink/space/held-lock tests |
| F-C6 fallible allocation | Explicit fallible scopes preserve checkpointer and hash/vector fallback paths; reserve refusal preserves queues and existing data; checkpointer 10 tests pass |
| F-C7 autovacuum | Skip only untouched databases; written databases retain retry cadence; old-snapshot E2E |
| F-C8 release version | Separate tag/dispatch selection and validation, valid fetch depth; four subprocess tests |
| F-C9 Unicode TTL | Character-boundary-safe parser and structured usage errors; real CLI exits 2 |
| F-C10 readiness errors | Authentication failure surfaces immediately and leaves a healthy engine available for repair; real socket-auth E2E |
| F-C11 serve child ownership | Companion #7 keeps pgx as foreground monitor and reaps/forwards signals; real PID-1 container recovery and session-continuity test; prior crash cascade was not reproduced |
| F-T1 missing feature gates | Engine crate tests and janitor/pool/reaper/pin/caps/memory/schema/parallel/maintenance black-box suites run in PR CI |
| F-T2 nonblocking regression | Missing tests, semantic differences and new unapproved differences fail; evidence uploaded; configuration assumptions isolated from stock compatibility baseline |
| F-T3 hidden nightly failures | Suites run after independent failures, report outcomes and retain diagnostics; missing app toolchains fail CI; Rails toolchain installed |
| F-T4 flaky harness | Cross-process non-ephemeral port leases, IPv4/IPv6 probes isolated from forks, immutable CLI copy, native ARM repetition gate; companion #8 contains deterministic reproductions |
| F-A1 upstream path | Existing fork model and reviewed upstream-fix porting procedure documented in upstream-maintenance.md; no unsolicited history rewrite or automatic merge |
| F-Q1 duplication | Shared secure lock and stack setup helpers; doctor split into six check groups; behavioral suites remain green |
| F-Q2 swallowed errors | Status/pid/fsync failures surfaced; durable rename/unlink metadata; companion #7 exposes spawn failures and #16 validates init environment |
| F-O1 repository weight | Sparse CI checkouts omit fuzz corpus and historical benchmark payloads; history/dataset migration intentionally deferred as the audit requests |
| F-O2 CI leftovers | Temporary branch trigger removed and package-install failures no longer masked |
| F-D1 contributor/security path | CONTRIBUTING, SECURITY, CHANGELOG, replacement contributor guidance and private reporting path |
| F-D2 stale documentation | Deleted GUC removed, old plans explicitly historical, baseline paths corrected, signed-install instructions updated |

Additional audit notes: shared L2 content remains charged until the last holder
leaves (16 tests, including real map eviction); watchdog startup initializes
limits synchronously and refuses silent watchdog loss; engine log tail is bounded
and rotation preserves the live log inode. Dependency policy, full omitted license
texts, Dependabot, Docker image digests and RE2/Abseil source commits are included.

## Reproduce the focused engine checks

```sh
export PGX_DIST=/path/to/assembled/prefix
python3 -I tests/pgx/test_features.py -v
python3 -I tests/pgx/test_parallel_limits.py -v
python3 -I tests/pgx/test_maintenance.py -v
python3 -I tests/pgx/test_sql.py -v
cargo test --locked --lib -p mcx -p l2cache -p relcache -p janitor -p memwatchdog -p guc_tables -p autovacuum -p pgstat -p adt_xml -p checkpointer -p parallel
```

The last command passed all **372** distinct library tests in debug profile,
both serially and with default test concurrency. The nine packages required by
the new CI gate account for 351 tests; checkpointer/parallel add 21. Release CI
uses the release profile. The full workspace baseline is scheduled in nightly;
it was not claimed as a locally completed run.

See [regression evidence](audit-regression-validation.md),
[parallel memory evidence](audit-parallel-memory-validation.json), and the
companion supervisor/startup validation reports for exact commands and limits.

## Scope of the evidence

Memory limits cover memory-context accounting with the existing admission slack
and concurrency behavior; this is not a hard RSS or cgroup ceiling. TTL rollback
covers returned SQL/filesystem errors, not crash atomicity across both systems.
The broad inherited unsafe-code audit, repository history migration, and ZFS
runtime validation require separate work or appropriate infrastructure. No release
was published, deployed or signed through OIDC during local validation.
