# `pgx` CLI reference

`pgx [--json] <command> [arguments]`. `--json` makes a command print one JSON
object (or array) on stdout; errors print `{"error":{...}}`. Data lives under
`$PGX_HOME` (default `~/.pgx`).

## Quick start

| Command | What it does |
|---|---|
| `pgx run` | One command to a ready database. Initialise `$PGX_HOME` if needed, start the runtime if it is not already up, ensure a default `app` database, and print its connection URL. Idempotent and safe to repeat; it never resets or overwrites an existing database. `--json` prints one object (for agents). The first run reads `PGX_PORT` / `PGX_DURABILITY` / `PGX_PASSWORD`; afterwards they come from the saved config. |

```console
$ pgx run
PGX is ready.

  postgres://postgres:…@127.0.0.1:5432/app

  database   app (created)
  runtime    started (pid 23469, port 5432)
  home       /home/you/.pgx

Connect:
  psql "postgres://postgres:…@127.0.0.1:5432/app"
```

## Runtime

| Command | What it does |
|---|---|
| `pgx init [--port N] [--durability ephemeral\|full] [--force]` | Create the cluster under `$PGX_HOME` from the bundled base cluster, pick a free port, set a random superuser password, write the config. `--force` recreates (deletes every database). |
| `pgx start` | Start the runtime in the background (idempotent: a second start reports "already running"). |
| `pgx serve` | Run the engine in the **foreground** (init first if needed); the Docker/systemd entrypoint. |
| `pgx stop [--force]` | Fast shutdown (SIGINT); `--force` escalates to SIGKILL after the timeout. |
| `pgx restart` | Stop then start. |
| `pgx status` | Running?, pid, port, databases, connections, uptime, memory, libraries. Exit 3 when stopped. |

## Databases

| Command | What it does |
|---|---|
| `pgx create <db> [--ttl <dur>]` | Create an empty database. `--ttl` makes it temporary (auto-expires); default is permanent. |
| `pgx drop <db> [--force]` | Drop it; without `--force`, exit 6 if sessions are connected (data untouched); `--force` terminates them first. |
| `pgx clone <src> <dst> [--ttl <dur>]` | Copy-on-write clone (`CREATE DATABASE … STRATEGY = FILE_COPY`); shares blocks on APFS/ZFS/reflink filesystems. TTL is never inherited from the source. |
| `pgx ls` | List databases with status, TTL, remaining time and size. |
| `pgx ttl <db> [<dur>\|unlimited]` | Show a database's TTL, or set/extend/disable it. See [ttl.md](ttl.md). |
| `pgx url <db> [--socket]` | Print the connection URL (TCP with password, or Unix socket). |

Temporary databases (`--ttl`) are deleted automatically by a background worker;
permanent databases are never auto-deleted. Full semantics: [ttl.md](ttl.md).

## Tools

`pgx doctor` checks the install, data directory, config mode, socket-path
length, port, runtime, disk, stack limit, filesystem/block-cloning, libxml2/ICU,
and orphan `base/<oid>` directories (read-only; never deletes). `pgx version`
reports the engine commit and PostgreSQL compatibility. `pgx help`.

## Database names

1–63 bytes, `^[A-Za-z_][A-Za-z0-9_-]*$`, always sent quoted; `postgres`,
`template0`, `template1` cannot be created, dropped or cloned onto.

## Exit codes

`0` ok · `1` engine/runtime error · `2` usage · `3` not initialised / not
running · `4` conflict (exists, running, port in use) · `5` not found · `6`
database in use · `7` corrupted or incomplete installation.

## Environment

`PGX_HOME` (data home), `PGX_RUN_DIR` (socket directory, for a long `$PGX_HOME`),
`PGX_PORT` / `PGX_DURABILITY` / `PGX_LISTEN` / `PGX_PASSWORD` are read by
`init`, and by `run` / `serve` when they initialise a new cluster. Later starts
use the saved configuration.

For `init`, `--port` and `--durability` take precedence over their corresponding
environment variables. `PGX_PORT` accepts a number from 1024 to 65535;
`PGX_DURABILITY` accepts `ephemeral` or `full`. Invalid values fail with usage
exit code 2 instead of silently falling back. When unset, the port is selected
from 5432–5500 and durability defaults to `ephemeral`.

```sh
PGX_PORT=55440 PGX_DURABILITY=full pgx init
```
