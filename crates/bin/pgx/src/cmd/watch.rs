//! Own the TTL worker and restart it after an unexpected exit. In `serve`
//! mode this same monitor owns the engine, so worker exits are never reaped by
//! the postmaster (which interprets an unknown child's crash as an engine crash).
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::layout::Home;
use crate::out::Out;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: i32) {
    SIGNAL.fetch_or(1 << signal, Ordering::Relaxed);
}

fn signals() {
    // A process only enters one monitor. The handler does no allocation or I/O.
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
            libc::signal(signal, on_signal as *const () as libc::sighandler_t);
        }
    }
}

fn lock(home: &Home) -> Result<Option<std::fs::File>> {
    crate::fsutil::lock_file(&home.root.join("watchdog.lock"), false)
}

pub fn running(home: &Home) -> bool {
    matches!(lock(home), Ok(None))
}

pub fn run(out: &Out, root: &str) -> Result<ExitCode> {
    // The explicit home in argv also lets stop validate the pidfile's identity.
    std::env::set_var("PGX_HOME", root);
    let home = Home::resolve();
    if !home.is_initialized() {
        return Ok(ExitCode::Ok);
    }
    let Some(_lock) = lock(&home)? else {
        return Ok(ExitCode::Ok);
    };
    let pid_path = home.root.join("watchdog.pid");
    std::fs::write(&pid_path, std::process::id().to_string())
        .map_err(|e| PgxError::io("cannot write", &pid_path, &e))?;
    signals();
    let result = monitor(out, &home, None);
    let _ = std::fs::remove_file(pid_path);
    result
}

pub fn serve(out: &Out, ctx: &Ctx, spawn: impl FnOnce() -> Result<Child>) -> Result<ExitCode> {
    // serve has already excluded another engine. A lingering daemon watchdog
    // must be stopped before taking its lock for this new runtime.
    stop(&ctx.home);
    let Some(_lock) = lock(&ctx.home)? else {
        return Err(PgxError::failure("another TTL watchdog is still running"));
    };
    signals();
    monitor(out, &ctx.home, Some(spawn()?))
}

fn spawn_worker(home: &Home) -> Result<Child> {
    let exe = std::env::current_exe()
        .map_err(|e| PgxError::failure(format!("cannot locate pgx: {e}")))?;
    Command::new(exe)
        .arg("supervise")
        .arg(std::fs::canonicalize(&home.root).unwrap_or_else(|_| home.root.clone()))
        .env("PGX_HOME", &home.root)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| PgxError::failure(format!("cannot start TTL supervisor: {e}")))
}

fn worker_locked(home: &Home) -> bool {
    // Only EWOULDBLOCK establishes a live owner; permission and filesystem
    // errors remain visible in the worker's own startup error output.
    matches!(crate::fsutil::lock_file(&home.root.join("supervisor.lock"), false), Ok(None))
}

fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn monitor(out: &Out, home: &Home, mut engine: Option<Child>) -> Result<ExitCode> {
    let mut worker: Option<Child> = None;
    let mut engine_pid = engine.as_ref().map(Child::id);
    let mut retry_at = Instant::now();
    let startup_deadline = Instant::now() + Duration::from_secs(60);
    let result = loop {
        let pending = SIGNAL.swap(0, Ordering::Relaxed);
        if pending != 0 {
            if let Some(child) = engine.as_mut() {
                for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT, libc::SIGQUIT] {
                    if pending & (1 << signal) != 0 {
                        unsafe {
                            libc::kill(child.id() as i32, signal);
                        }
                    }
                }
            } else {
                break Ok(ExitCode::Ok);
            }
        }
        if let Some(child) = engine.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break if status.success() {
                        Ok(ExitCode::Ok)
                    } else {
                        Err(PgxError::failure(format!("engine exited: {status}")))
                    }
                }
                Err(e) => break Err(PgxError::failure(format!("cannot wait for engine: {e}"))),
                Ok(None) => {}
            }
        }
        let current = Ctx::load().ok().and_then(|ctx| ctx.running_pid());
        if !home.is_initialized() {
            break Ok(ExitCode::Ok);
        }
        match (engine_pid, current) {
            (None, Some(pid)) => engine_pid = Some(pid),
            (Some(expected), Some(actual)) if expected != actual => break Ok(ExitCode::Ok),
            (Some(_), None) if engine.is_none() => break Ok(ExitCode::Ok),
            (None, None) if Instant::now() >= startup_deadline => break Ok(ExitCode::Ok),
            _ => {}
        }
        if let Some(child) = worker.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    out.note(&format!(
                        "pgx watch: TTL supervisor exited ({status}); retrying in 1s"
                    ));
                    worker = None;
                    retry_at = Instant::now() + Duration::from_secs(1);
                }
                Err(e) => {
                    break Err(PgxError::failure(format!(
                        "cannot wait for TTL supervisor: {e}"
                    )))
                }
                Ok(None) => {}
            }
        }
        if current.is_some()
            && worker.is_none()
            && Instant::now() >= retry_at
            && !worker_locked(home)
        {
            match spawn_worker(home) {
                Ok(child) => worker = Some(child),
                Err(e) => {
                    out.note(&format!("pgx watch: {e}; retrying in 1s"));
                    retry_at = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(child) = worker.as_mut() {
        stop_child(child);
        let _ = std::fs::remove_file(home.root.join("supervisor.pid"));
    }
    // Error paths must not orphan the foreground engine.
    if let Some(child) = engine.as_mut() {
        stop_child(child);
    }
    result
}

/// A stale pidfile is not authority to signal a process: require the watch lock
/// and the exact command/home pairing too. Never signal a foreign reused PID.
pub fn stop(home: &Home) {
    let probe = match lock(home) {
        Ok(Some(file)) => Some(file),
        Ok(None) => None,
        Err(_) => return,
    };
    if probe.is_none() {
        let pid_path = home.root.join("watchdog.pid");
        let pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        if let Some(pid) = pid.filter(|p| *p > 1) {
            let root = std::fs::canonicalize(&home.root).unwrap_or_else(|_| home.root.clone());
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let expected = format!("{} watch {}", exe.display(), root.display());
            if crate::process::pid_alive(pid)
                && crate::process::command_line(pid).is_some_and(|s| s.trim_end() == expected)
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(4);
                while crate::process::pid_alive(pid) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        return;
    };
    let _ = std::fs::remove_file(home.root.join("watchdog.pid"));
    drop(probe);
}
