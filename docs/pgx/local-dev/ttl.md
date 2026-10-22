# Temporary databases with TTL

A PGX database can be given a **time-to-live**: after it expires, a background
worker deletes it automatically. This makes disposable branches for agents, CI
jobs and feature work safe to create freely — they clean themselves up.

```bash
pgx create agent-001 --ttl 5m      # expires 5 minutes after creation
pgx clone myapp feature-x --ttl 1h # a disposable clone
pgx ttl agent-001 30m              # change the TTL
pgx ttl agent-001 unlimited        # make it permanent
pgx ttl agent-001                  # show the current TTL
pgx ls                             # NAME, STATUS, TTL, EXPIRES IN, SIZE
```

## Durations

`30s`, `5m`, `15m`, `1h`, `6h`, `24h`, `7d` — any positive whole number with a
unit of `s` (seconds), `m` (minutes), `h` (hours) or `d` (days), up to a maximum
of `3650d`. `unlimited` (also `none`/`never`) means no expiry. Zero, negative,
fractional, unit-less and over-cap values are rejected.

## Unlimited is the default — and never inherited

`pgx create myapp` and `pgx clone myapp x` create **permanent** databases: no
TTL, never auto-deleted. A TTL applies only when you pass `--ttl`. A clone never
inherits its source's TTL — you set it explicitly or it is unlimited. There is
no metadata record for a permanent database, so nothing can ever mark it for
deletion.

The default `app` database created by `pgx run` is permanent unless you set a
TTL on it yourself.

## When the clock starts

- `pgx create x --ttl 5m` → expires at **creation time + 5m**.
- `pgx ttl x 1h` → expires at **the time you run it + 1h** (extends or shortens
  from now, not from creation).

Deadlines are stored as UTC Unix timestamps, so they are timezone-independent.

## While PGX is stopped

Deadlines keep advancing while PGX is stopped — wall-clock time still passes.
Automatic deletion does not happen while the runtime is down (there is nothing
running to do it), but **cleanup resumes automatically the next time PGX
starts**: the worker's first sweep removes anything already past its deadline.

## The cleanup worker

`pgx start`, `pgx run` and `pgx serve` launch a small background supervisor
(one per runtime, enforced by a lock). It polls every 5 seconds by default
(`PGX_TTL_INTERVAL` seconds to change it), so an expired database is removed
within roughly its TTL plus one poll interval. The worker exits when the
runtime stops and is relaunched on the next start. A small watchdog owns and
reaps the worker. If the worker crashes or is killed while PGX is running, the
watchdog replaces it after a one-second backoff; cleanup resumes without another
CLI invocation. Both processes stop with the runtime. The worker's lock still
allows only one cleanup sweep at a time.

In `pgx serve`, the foreground PGX process owns the engine and worker separately
and forwards shutdown signals to the engine. A failed cleanup worker therefore
cannot trigger the engine's child-process crash handling or disconnect other
database sessions.

`pgx run` exits after starting everything — the worker keeps running without a
terminal open.

## Active connections

A TTL deadline is authoritative. When a temporary database expires, the worker
terminates **that database's** sessions (never any other database's, never the
shared runtime), then drops it with `DROP DATABASE ... WITH (FORCE)`. Treat a
TTL-managed branch as ephemeral: do not keep long-lived work in it past its
deadline. Extending the TTL (`pgx ttl x <longer>`) before expiry cancels the
deletion; setting it to `unlimited` makes the database permanent.

## Safety

A database is deleted automatically only when **all** of these hold: it had an
explicit TTL, the deadline has passed, the metadata is valid, the recorded
database OID still matches (a dropped-and-recreated name is a different
incarnation and is never touched), it belongs to the current cluster, and it is
not a protected system database (`postgres`, `template0`, `template1`). Missing
or corrupt metadata is never treated as permission to delete. Cleanup is
idempotent and crash-safe: interrupted at any point, the next sweep either
finishes the drop or finds the database already gone and finalizes.

## Inspecting and diagnosing

- `pgx ls` / `pgx ls --json` show each database's TTL and remaining time.
- `pgx ttl <db>` shows one database's TTL (JSON with `--json`:
  `{ "name", "ttl", "ttl_seconds", "expires_at", "expires_in_seconds" }`;
  `expires_at` is `null` for unlimited).
- `pgx doctor` reports whether the supervisor is running, how many temporary
  databases exist, how many are past their deadline awaiting cleanup, any
  unreadable TTL records, and any failed cleanups (with the last error).
