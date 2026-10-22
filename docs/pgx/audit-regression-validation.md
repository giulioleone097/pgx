# Regression gate validation, 2026-10-22

The two semantic differences reported in issue #17 came from running PostgreSQL's
stock regression expectations against the deliberately different PGX ephemeral
configuration. They reproduce with the unchanged published `pgx 0.1.0` Linux amd64
engine (`engine_commit=d62547da71`) and PostgreSQL 18.6 regression tools.

| Settings | Scheduled/reported | Byte-exact | EXPLAIN only | Semantic | Gate |
|---|---|---|---|---|---|
| Stock configuration, worker limits 8/2 | 231 / 231 | 219 | 12 | 0 | Pass |
| `configs/pgx-ephemeral.conf` | 231 / 231 | 223 | 6 | 2 | Fail |

In `stats`, the changed boolean is specifically
`SELECT current_setting('synchronous_commit') = 'on'`. The profile explicitly
sets `synchronous_commit=off`. In `select_parallel`, the profile's
`max_parallel_workers=0` prevents launching workers: the expected worker error
context disappears and the final `wrk_launched` assertion becomes false.
Neither difference requires changing a SQL result or relaxing the classifier.

Nightly now runs compatibility with the stock configuration and explicit
PostgreSQL worker limits. The PGX ephemeral/resource policies have independent
black-box feature and parallel-memory tests in PR CI and nightly. Every semantic
diff still fails the compatibility gate, even if the test name is allowlisted.
Only named, explained EXPLAIN differences are permitted, and only while all their
changed lines remain inside EXPLAIN output blocks. Missing reports, crashed
servers, harness errors and unclassified/new failures also fail.

Reproduce with a built engine and PostgreSQL 18.6 tools (`PG18_PREFIX` if needed):

```sh
python3 benchmarks/pgx/regress-baseline.py --out /tmp/regress-stock \
  --binary "$PGX_DIST/bin/postgres" --max-connections 15 \
  --server-arg=-c --server-arg=max_parallel_workers_per_gather=2 \
  --server-arg=-c --server-arg=max_parallel_workers=8 \
  --server-arg=-c --server-arg=max_worker_processes=8
python3 benchmarks/pgx/classify-regress-diffs.py /tmp/regress-stock

python3 benchmarks/pgx/regress-baseline.py --out /tmp/regress-ephemeral \
  --binary "$PGX_DIST/bin/postgres" --conf configs/pgx-ephemeral.conf \
  --max-connections 15
python3 benchmarks/pgx/classify-regress-diffs.py /tmp/regress-ephemeral
# Expected nonzero: the two profile assumptions above differ from stock PG.
```

The compact machine-readable outputs are in
[audit-regression-results.json](audit-regression-results.json). Full
`regress.json`, `classification.json`, `regression.diffs`, actual result files,
server logs and runner logs are uploaded by nightly even when its gate fails.
The classifier subprocess tests additionally inject a semantic boolean change,
a new EXPLAIN difference and an unreported scheduled test; each is rejected.
