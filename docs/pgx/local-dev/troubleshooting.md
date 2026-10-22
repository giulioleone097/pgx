# Troubleshooting

Run `pgx doctor` first; it names most problems and the fix.

| Symptom | Cause / fix |
|---|---|
| `pgx init` → exit 2, "socket path would be … bytes" | `$PGX_HOME` is too deep for a Unix socket (limit 103 bytes). Set `PGX_RUN_DIR=/tmp/pgx` and re-run, or use a shorter `$PGX_HOME`. |
| `pgx start` → exit 4, "port … is in use" | Another process holds the port. Stop it, or `pgx init --force --port N`. |
| `pgx start` → exit 3, "not initialised" | Run `pgx init`. |
| exit 7, "incomplete PGX installation" | A file is missing from the distribution; reinstall. `pgx doctor` lists which. |
| `pgx drop` → exit 6, "in use" | Clients are connected. Disconnect them, or `pgx drop <db> --force`. |
| XML functions raise `0A000 unsupported XML feature` | The host has no libxml2. Install it (`libxml2` on Debian/Ubuntu); macOS and the Docker image include it. `pgx doctor` shows library status. |
| `pgx ls` shows a database as `invalid (interrupted drop)` | A `DROP DATABASE` was interrupted. Run `pgx drop <db> --force` again. |
| `pgx doctor` warns about orphan `base/<oid>` directories | A CREATE DATABASE or clone was interrupted by a crash. PGX never deletes them automatically; after confirming nothing is creating a database, remove them by hand from `$PGX_HOME/data/base`. |
| In a container, `pgx stop` seems slow or restart fails after a kill | The container has no init to reap processes. Run it with `docker run --init`, or use `pgx serve` (PGX stays at PID 1 and reaps its engine and cleanup worker). |
| disk full → `53100` then the runtime stops | Expected: PGX is one crash domain. Free space and `pgx start`; committed data is recovered. |

Logs: `$PGX_HOME/log/pgx.log`. Status as JSON: `pgx --json status`.
