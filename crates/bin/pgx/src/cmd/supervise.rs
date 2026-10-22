//! `pgx supervise`: the background TTL cleanup worker. Spawned detached by
//! `pgx start`/`run`/`serve`, it polls the TTL metadata, and for each expired
//! database verifies identity (name present, OID matches the recorded
//! incarnation, same cluster) before a forced drop. It is single-instance (an
//! exclusive flock), tied to the runtime's lifetime (it exits when the engine
//! is gone, so `pgx start` owns respawning), and idempotent across crashes.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::layout::Home;
use crate::out::Out;
use crate::ttlstore::{MetaStore, State, TtlRecord};
use std::os::fd::AsRawFd;
use std::time::Duration;

const DEFAULT_INTERVAL_SECS: u64 = 5;
/// A failed cleanup is retried, but not before this backoff, so a persistently
/// failing drop cannot busy-loop.
const RETRY_BACKOFF_SECS: u64 = 30;

pub fn run(out: &Out) -> Result<ExitCode> {
    let home = Home::resolve();
    if !home.is_initialized() {
        return Ok(ExitCode::Ok); // nothing to supervise
    }

    // Single instance: hold an exclusive, non-blocking lock for our whole life.
    // A second supervisor simply exits, so start/run/serve may spawn freely.
    let lock_path = home.root.join("supervisor.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| PgxError::io("cannot open", &lock_path, &e))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        out.note("pgx supervise: another supervisor is already running; exiting");
        return Ok(ExitCode::Ok);
    }
    let pid_path = home.root.join("supervisor.pid");
    std::fs::write(&pid_path, std::process::id().to_string())
        .map_err(|e| PgxError::io("cannot write supervisor pid", &pid_path, &e))?;

    let interval = std::env::var("PGX_TTL_INTERVAL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(1);
    out.note(&format!(
        "pgx supervise: pid {} interval {interval}s",
        std::process::id()
    ));

    let mut down_cycles = 0u32;
    loop {
        if let Err(e) = crate::process::rotate_log(&home) {
            out.note(&format!("pgx supervise: {e}"));
        }
        // Home removed or re-initialised out from under us (e.g. a test tore it
        // down, or `pgx init --force`): stop immediately.
        if !home.is_initialized() {
            out.note("pgx supervise: home no longer initialised; exiting");
            let _ = std::fs::remove_file(&pid_path);
            return Ok(ExitCode::Ok);
        }
        match Ctx::load() {
            Ok(ctx) if ctx.running_pid().is_some() => {
                down_cycles = 0;
                run_cycle(&ctx);
            }
            // Runtime down (or config temporarily unreadable). We are tied to the
            // runtime's lifetime: exit so we never linger past `pgx stop`, and a
            // later `pgx start` owns respawning.
            _ => {
                down_cycles += 1;
                if down_cycles >= 2 {
                    out.note("pgx supervise: runtime not running; exiting");
                    let _ = std::fs::remove_file(&pid_path);
                    return Ok(ExitCode::Ok);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
}

/// One cleanup sweep. Reuses a single connection for all drops in the cycle.
fn run_cycle(ctx: &Ctx) {
    let store = MetaStore::new(&ctx.home.root);
    let (records, _bad) = store.list();
    let now = crate::fsutil::now_epoch();
    let due: Vec<TtlRecord> = records
        .into_iter()
        .filter(|r| r.is_expired(now) && should_attempt(r, now))
        .collect();
    if due.is_empty() {
        return;
    }
    let mut client = match ctx.connect("postgres") {
        Ok(c) => c,
        Err(_) => return, // runtime busy/starting; try again next cycle
    };
    for rec in due {
        let Ok(_lock) = store.lock(&rec.name) else {
            continue;
        };
        // The queued record may have been dropped/recreated or extended while
        // waiting for the lock. Never finalize metadata from that old snapshot.
        let Ok(Some(rec)) = store.load(&rec.name) else {
            continue;
        };
        let now = crate::fsutil::now_epoch();
        if !rec.is_expired(now) || !should_attempt(&rec, now) {
            continue;
        }
        if let Err(e) = expire_one(ctx, &store, &mut client, &rec) {
            // Observable failure; back off and retry on a later cycle.
            let Ok(Some(mut r)) = store.load(&rec.name) else {
                continue;
            };
            if r.oid != rec.oid
                || r.cluster_id != rec.cluster_id
                || !r.is_expired(crate::fsutil::now_epoch())
            {
                continue;
            }
            r.state = State::RetryPending;
            r.last_attempt = Some(crate::fsutil::now_epoch());
            r.last_error = Some(e.message.clone());
            let _ = store.save(&r);
        }
    }
    client.close();
}

fn should_attempt(rec: &TtlRecord, now: u64) -> bool {
    match rec.state {
        State::RetryPending => rec
            .last_attempt
            .map(|t| now.saturating_sub(t) >= RETRY_BACKOFF_SECS)
            .unwrap_or(true),
        _ => true,
    }
}

/// Expire one database under the full safety gate. Returns Ok when the database
/// is gone and its metadata finalized, or when cleanup is safely declined
/// (protected, wrong cluster, identity mismatch, deadline cancelled).
fn expire_one(
    ctx: &Ctx,
    store: &MetaStore,
    client: &mut crate::pgwire::Client<std::os::unix::net::UnixStream>,
    rec: &TtlRecord,
) -> Result<()> {
    // Gate 1: never auto-delete a protected system database.
    if crate::names::RESERVED.contains(&rec.name.as_str()) {
        return Ok(());
    }
    // Gate 2: only act on the cluster this metadata was written for.
    if rec.cluster_id != ctx.cluster_id() {
        return store.remove(&rec.name);
    }
    // Gate 3: the database must still exist with the recorded OID. Gone → finalize.
    // Different OID → the name was reused by a new database; drop the stale record,
    // never the replacement.
    match crate::cmd::db::database_oid(client, &rec.name)? {
        None => return store.remove(&rec.name),
        Some(oid) if oid != rec.oid => return store.remove(&rec.name),
        Some(_) => {}
    }
    // Gate 4: re-read under the per-database lock held by run_cycle. CLI TTL
    // updates hold the same lock, through SQL and metadata, so cancellation
    // cannot be acknowledged between this check and DROP.
    // A concurrent `pgx ttl … unlimited` (record removed) or extension (deadline
    // moved) cancels the deletion.
    let now = crate::fsutil::now_epoch();
    match store.load(&rec.name)? {
        None => return Ok(()),
        Some(cur) => {
            if cur.oid != rec.oid || cur.cluster_id != ctx.cluster_id() || !cur.is_expired(now) {
                return Ok(());
            }
        }
    }

    // Commit to deletion: mark DELETING (crash here → next run re-verifies and
    // either finishes the drop or finds it gone and finalizes).
    let mut deleting = rec.clone();
    deleting.state = State::Deleting;
    deleting.last_attempt = Some(now);
    deleting.last_error = None;
    store.save(&deleting)?;

    // Terminate sessions on THIS database only (never other databases, never the
    // shared runtime), then drop with FORCE.
    let _ = client.simple_query(&format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = {} AND pid <> pg_backend_pid()",
        crate::pgwire::quote_literal(&rec.name)
    ));
    let name = crate::names::validate(&rec.name)?; // safe identifier quoting
    client
        .simple_query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            name.quoted()
        ))
        .map_err(|e| {
            crate::cmd::db_error(e, &format!("cannot drop expired database {}", rec.name))
        })?;

    // Verify the drop, then finalize metadata.
    if crate::cmd::db::database_oid(client, &rec.name)?.is_some() {
        return Err(PgxError::failure(format!(
            "database {} still present after a forced drop",
            rec.name
        )));
    }
    store.remove(&rec.name)
}
