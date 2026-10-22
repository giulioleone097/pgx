//! The engine process: spawn detached, wait until it answers, stop it, and
//! tell whether a pid in postmaster.pid is still ours.
use crate::cli::ExitCode;
use crate::config::Config;
use crate::errors::{PgxError, Result};
use crate::layout::{Home, Install};
use crate::pgwire::{Client, WireError};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const STACK_BYTES: u64 = 65520 * 1024;
pub const RUST_MIN_STACK: &str = "33554432";

/// Only libc calls: safe to call from Command::pre_exec after fork as well as
/// before launching the foreground engine.
pub fn raise_stack_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut limit) } == 0 {
        let wanted = STACK_BYTES as libc::rlim_t;
        if limit.rlim_cur < wanted
            && (limit.rlim_max == libc::RLIM_INFINITY || limit.rlim_max >= wanted)
        {
            limit.rlim_cur = wanted;
            unsafe { libc::setrlimit(libc::RLIMIT_STACK, &limit) };
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidFile {
    pub pid: u32,
    pub data_dir: PathBuf,
    pub port: Option<u16>,
    pub sock_dir: Option<PathBuf>,
}

/// postmaster.pid: pid, data dir, start time, port, socket dir, listen addr, shmem key, status.
pub fn read_pid_file(data: &Path) -> Option<PidFile> {
    let text = std::fs::read_to_string(data.join("postmaster.pid")).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let data_dir = PathBuf::from(lines.next()?.trim());
    let _start = lines.next();
    let port = lines.next().and_then(|l| l.trim().parse().ok());
    let sock_dir = lines
        .next()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty());
    Some(PidFile {
        pid,
        data_dir,
        port,
        sock_dir,
    })
}

/// An unreadable/unrecognised command line is not evidence that a live engine
/// is dead. Preserve its pidfile and ask the caller to resolve the identity.
pub fn require_stale_pidfile(data: &Path) -> Result<()> {
    if let Some(pf) = read_pid_file(data) {
        if pid_alive(pf.pid) {
            // A readable executable identity can prove PID reuse by an
            // unrelated process. Unknown identity (or another postgres) cannot.
            if let Some(exe) = executable_path(pf.pid) {
                if let Some(name) = exe.file_name().and_then(|s| s.to_str()) {
                    if name != "postgres" && name != "pgrust" && !name.ends_with(" (deleted)") {
                        return Ok(());
                    }
                }
            }
            return Err(PgxError::new(
                ExitCode::Conflict,
                format!(
                    "postmaster.pid names a live process (pid {}); refusing to remove it",
                    pf.pid
                ),
            )
            .hint("check the recorded process and stop it before retrying"));
        }
    }
    Ok(())
}

fn executable_path(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let name = String::from_utf8(output.stdout).ok()?;
        let name = name.trim();
        (!name.is_empty()).then(|| PathBuf::from(name))
    }
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    let exists = r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    // A zombie still "exists" for kill(2) until its parent reaps it. The engine
    // has already exited, so for liveness it is stopped. This matters when the
    // engine's parent (the detached `pgx start`) is gone and PID 1 does not reap
    // children — common in containers without an init and in CI.
    exists && !is_zombie(pid)
}

#[cfg(target_os = "linux")]
fn is_zombie(pid: u32) -> bool {
    // /proc/<pid>/stat: "<pid> (<comm>) <state> ...". comm may contain spaces
    // and parentheses, so read the state after the LAST ')'.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat.rfind(')') {
            Some(i) => stat[i + 1..].trim_start().starts_with('Z'),
            None => false,
        },
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn is_zombie(_pid: u32) -> bool {
    false
}

/// The process's command line, or None if it is gone.
pub fn command_line(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            return Some(String::from_utf8_lossy(&raw).replace('\0', " "));
        }
    }
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

/// True when `cmd` is an engine process launched on exactly `data_dir`, i.e. it
/// carries the `-D <data_dir>` launch flag. Matching the flag (not just the path
/// as a substring) avoids treating an unrelated process whose argv merely
/// mentions the data directory (a backup, an editor) as our engine.
pub fn engine_cmdline_matches(cmd: &str, data_dir: &Path) -> bool {
    // ps renders paths with spaces as text rather than preserving argv. Keep
    // the exact-path case before the token fallback used for path aliases.
    let needle = format!("-D {}", data_dir.display());
    if cmd.match_indices(&needle).any(|(at, _)| {
        (at == 0 || cmd.as_bytes()[at - 1].is_ascii_whitespace())
            && cmd[at + needle.len()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    }) {
        return true;
    }
    let args: Vec<_> = cmd.split_whitespace().collect();
    args.windows(2)
        .any(|pair| pair[0] == "-D" && same_path(Path::new(pair[1]), data_dir))
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// A pidfile alone is never authority to signal a process: PIDs are reused.
/// Check the singleton lock and the exact executable, command and home argv.
pub fn supervisor_pid(home: &Home) -> Option<u32> {
    background_pid(home, "supervisor", "supervise")
}

pub fn watchdog_pid(home: &Home) -> Option<u32> {
    background_pid(home, "watchdog", "watch")
}

fn background_pid(home: &Home, name: &str, command: &str) -> Option<u32> {
    let pid = std::fs::read_to_string(home.root.join(format!("{name}.pid")))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if pid <= 1 || pid > i32::MAX as u32 || !pid_alive(pid) {
        return None;
    }
    // A held singleton lock is necessary, but PID/executable/home checks below
    // also prevent stale metadata from authorizing a signal to another process.
    if !matches!(
        crate::fsutil::lock_file(&home.root.join(format!("{name}.lock")), false),
        Ok(None)
    ) {
        return None;
    }
    let root = std::fs::canonicalize(&home.root).ok()?;
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        if !same_path(&std::fs::read_link(format!("/proc/{pid}/exe")).ok()?, &exe) {
            return None;
        }
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let args: Vec<_> = raw
            .split(|b| *b == 0)
            .filter(|arg| !arg.is_empty())
            .collect();
        if args.len() != 3
            || args[1] != command.as_bytes()
            || args[2] != root.as_os_str().as_bytes()
        {
            return None;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let cmd = command_line(pid)?;
        if cmd != format!("{} {command} {}", exe.display(), root.display()) {
            return None;
        }
    }
    Some(pid)
}

/// True when `pid` is alive and is an engine started on this data directory.
pub fn is_our_engine(pid: u32, data_dir: &Path) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
        use std::os::unix::ffi::OsStrExt;
        let args: Vec<_> = raw.split(|b| *b == 0).collect();
        return args.windows(2).any(|pair| {
            pair[0] == b"-D" && same_path(Path::new(std::ffi::OsStr::from_bytes(pair[1])), data_dir)
        });
    }
    match command_line(pid) {
        Some(cmd) => engine_cmdline_matches(&cmd, data_dir),
        None => false,
    }
}

pub fn engine_env(install: &Install) -> Vec<(String, String)> {
    vec![
        ("RUST_MIN_STACK".into(), RUST_MIN_STACK.into()),
        (
            "PGRUST_PGSHAREDIR".into(),
            install.share.display().to_string(),
        ),
        (
            "PGRUST_TZDIR".into(),
            install.share.join("timezone").display().to_string(),
        ),
    ]
}

pub fn engine_args(home: &Home, cfg: &Config, sock_dir: &Path) -> Vec<String> {
    vec![
        "-D".into(),
        home.data.display().to_string(),
        "-k".into(),
        sock_dir.display().to_string(),
        "-p".into(),
        cfg.port.to_string(),
        "-c".into(),
        "io_method=sync".into(),
        "-c".into(),
        "max_stack_depth=60000".into(),
    ]
}

/// Starts the engine detached (own session, stdio to the log file).
pub fn spawn(
    install: &Install,
    home: &Home,
    cfg: &Config,
    sock_dir: &Path,
    extra_args: &[&str],
) -> Result<Child> {
    use std::os::unix::process::CommandExt;
    if let Some(dir) = home.log_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PgxError::io("cannot create", dir, &e))?;
    }
    crate::fsutil::private_dir(sock_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&home.log_file)
        .map_err(|e| PgxError::io("cannot open log", &home.log_file, &e))?;
    let log2 = log
        .try_clone()
        .map_err(|e| PgxError::io("cannot dup log", &home.log_file, &e))?;
    let mut cmd = Command::new(&install.engine);
    cmd.args(engine_args(home, cfg, sock_dir))
        .args(extra_args)
        .envs(engine_env(install))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log2);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            raise_stack_limit();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        PgxError::failure(format!(
            "cannot start the engine {}: {e}",
            install.engine.display()
        ))
    })
}

/// Removes a leftover Unix-socket lock file (and socket) for our port when the
/// postmaster it names is not alive. The engine does this itself for a dead
/// PID, but a zombie (unreaped by a non-init PID 1, as in a bare container)
/// looks alive to it and blocks startup; our liveness check sees the zombie as
/// dead, so we clear the stale files. Safe: only when the named PID is gone.
pub fn clear_stale_socket_lock(sock_dir: &Path, port: u16) {
    let lock = sock_dir.join(format!(".s.PGSQL.{port}.lock"));
    let named_pid = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|t| t.lines().next().and_then(|l| l.trim().parse::<u32>().ok()));
    match named_pid {
        Some(pid) if pid_alive(pid) => {} // a live postmaster owns it; leave it
        _ => {
            let _ = std::fs::remove_file(&lock);
            let _ = std::fs::remove_file(sock_dir.join(format!(".s.PGSQL.{port}")));
        }
    }
}

/// Keep the append-only engine log bounded without changing the engine's open
/// descriptor: snapshot the last 10 MiB then truncate the same inode. The TTL
/// supervisor checks once per sweep; one previous snapshot is retained.
pub fn rotate_log(home: &Home) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    const LIMIT: u64 = 10 * 1024 * 1024;
    let mut source = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&home.log_file)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(PgxError::io("cannot open log", &home.log_file, &e)),
    };
    let io = || -> std::io::Result<()> {
        let len = source.metadata()?.len();
        if len <= LIMIT {
            return Ok(());
        }
        source.seek(SeekFrom::Start(len - LIMIT))?;
        let mut tail = Vec::with_capacity(LIMIT as usize);
        source.take(LIMIT).read_to_end(&mut tail)?;
        let previous = home.log_file.with_extension("log.1");
        let mut snapshot = std::fs::File::create(previous)?;
        snapshot.write_all(&tail)?;
        snapshot.sync_all()?;
        // take() consumed the handle; reopen only to truncate, not replace.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&home.log_file)?
            .set_len(0)
    };
    io().map_err(|e| PgxError::io("cannot rotate log", &home.log_file, &e))
}

pub fn log_tail(home: &Home, n: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    // Diagnostics must stay bounded even after weeks of engine output or a
    // malformed multi-gigabyte line. Read at most the final 64 KiB.
    let read = || -> std::io::Result<String> {
        let mut file = std::fs::File::open(&home.log_file)?;
        let offset = file.metadata()?.len().saturating_sub(64 * 1024);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(64 * 1024).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<_> = text.lines().collect();
        Ok(lines[lines.len().saturating_sub(n)..].join("\n"))
    };
    read().unwrap_or_default()
}

/// Connects repeatedly until the engine answers `SELECT 1`, the child exits, or the timeout passes.
pub fn wait_ready(
    home: &Home,
    sock_dir: &Path,
    port: u16,
    child: Option<&mut Child>,
    timeout: Duration,
) -> Result<Client<UnixStream>> {
    let start = Instant::now();
    let mut child = child;
    loop {
        if let Some(c) = child.as_deref_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                return Err(PgxError::failure(format!(
                    "the engine exited during startup ({status})\n--- {} (last lines)\n{}",
                    home.log_file.display(),
                    log_tail(home, 15)
                )));
            }
        }
        match Client::connect_unix(
            sock_dir,
            port,
            "postgres",
            "postgres",
            Duration::from_secs(5),
        ) {
            Ok(mut client) => match client.simple_query("SELECT 1") {
                Ok(_) => return Ok(client),
                Err(WireError::Db(e)) if e.sqlstate == "57P03" => {}
                Err(e) => {
                    return Err(PgxError::new(
                        ExitCode::NotReady,
                        format!("engine readiness query failed: {e}"),
                    ))
                }
            },
            Err(WireError::Db(e)) if e.sqlstate == "57P03" => {} // starting up
            Err(WireError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => {
                return Err(PgxError::new(
                    ExitCode::NotReady,
                    format!("cannot check engine readiness: {e}"),
                ))
            }
        }
        if start.elapsed() > timeout {
            return Err(PgxError::failure(format!(
                "the engine did not become ready within {}s\n--- {} (last lines)\n{}",
                timeout.as_secs(),
                home.log_file.display(),
                log_tail(home, 15)
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// SIGINT: disconnect sessions, checkpoint, exit.
    Fast,
    /// SIGKILL after the fast timeout.
    Force,
}

fn signal(pid: u32, sig: i32) -> Result<()> {
    if unsafe { libc::kill(pid as i32, sig) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(PgxError::failure(format!(
            "cannot signal engine pid {pid}: {e}"
        )));
    }
    Ok(())
}

fn wait_gone(pid: u32, child: Option<&mut Child>, timeout: Duration) -> bool {
    let start = Instant::now();
    let mut child = child;
    loop {
        let alive = match child.as_deref_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => pid_alive(pid),
        };
        if !alive {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Stops the engine. `child` (when we spawned it in this process) lets us reap it.
pub fn stop(
    pid: u32,
    mode: StopMode,
    timeout: Duration,
    mut child: Option<&mut Child>,
) -> Result<()> {
    signal(pid, libc::SIGINT)?;
    if wait_gone(pid, child.as_deref_mut(), timeout) {
        return Ok(());
    }
    match mode {
        StopMode::Fast => Err(PgxError::new(
            ExitCode::Failure,
            format!(
                "the engine (pid {pid}) did not stop within {}s",
                timeout.as_secs()
            ),
        )
        .hint("retry, or `pgx stop --force` to kill it")),
        StopMode::Force => {
            signal(pid, libc::SIGKILL)?;
            if wait_gone(pid, child, Duration::from_secs(10)) {
                Ok(())
            } else {
                Err(PgxError::failure(format!(
                    "the engine (pid {pid}) survived SIGKILL"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_parses_postmaster_pid() {
        let dir = std::env::temp_dir().join(format!("pgx-pidfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("postmaster.pid"), "4242\n/Users/x/.pgx/data\n1760000000\n5433\n/Users/x/.pgx/run\n127.0.0.1\n  5433001        64\nready\n").unwrap();
        let p = read_pid_file(&dir).unwrap();
        assert_eq!(p.pid, 4242);
        assert_eq!(p.data_dir, PathBuf::from("/Users/x/.pgx/data"));
        assert_eq!(p.port, Some(5433));
        assert_eq!(p.sock_dir, Some(PathBuf::from("/Users/x/.pgx/run")));
        assert!(read_pid_file(&dir.join("nope")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn engine_cmdline_matches_requires_dash_d_flag() {
        let data = std::path::Path::new("/home/u/.pgx/data");
        assert!(engine_cmdline_matches(
            "/opt/pgx/bin/postgres -D /home/u/.pgx/data -k /home/u/.pgx/run -p 5432",
            data
        ));
        // an unrelated process whose argv merely contains the data dir path must NOT match
        assert!(!engine_cmdline_matches(
            "tar czf backup.tgz /home/u/.pgx/data",
            data
        ));
        assert!(!engine_cmdline_matches(
            "vim /home/u/.pgx/data/postgresql.conf",
            data
        ));
        // a different cluster must not match
        assert!(!engine_cmdline_matches(
            "/opt/pgx/bin/postgres -D /home/u/.pgx-other/data",
            data
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn zombie_is_not_alive() {
        // Fork a child that exits immediately; without waitpid it is a zombie.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe { libc::_exit(0) };
        }
        assert!(pid > 0);
        // give it a moment to exit
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !pid_alive(pid as u32),
            "an unreaped exited child (zombie) must count as not alive"
        );
        // reap it so we leave nothing behind
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }

    #[test]
    fn own_pid_is_alive_and_not_an_engine() {
        let me = std::process::id();
        assert!(pid_alive(me));
        assert!(!is_our_engine(me, Path::new("/definitely/not/a/data/dir")));
        assert!(!pid_alive(0));
    }

    #[test]
    fn diagnostics_read_a_bounded_tail_and_rotate_the_open_log_inode() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("pgx-log-test-{}", std::process::id()));
        let home = Home::at(root.clone());
        std::fs::create_dir_all(root.join("log")).unwrap();
        let mut writer = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&home.log_file)
            .unwrap();
        writer.set_len(12 * 1024 * 1024).unwrap();
        writer.write_all(b"\nlast line\n").unwrap();
        assert_eq!(log_tail(&home, 1), "last line");
        assert!(log_tail(&home, usize::MAX).len() <= 64 * 1024);
        rotate_log(&home).unwrap();
        assert_eq!(std::fs::metadata(&home.log_file).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(home.log_file.with_extension("log.1"))
                .unwrap()
                .len(),
            10 * 1024 * 1024
        );
        writer.write_all(b"after rotation\n").unwrap();
        assert_eq!(log_tail(&home, 1), "after rotation");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engine_args_name_data_socket_and_port() {
        let home = Home::at(PathBuf::from("/h/.pgx"));
        let cfg = Config {
            port: 5455,
            password: "p".into(),
            durability: Default::default(),
            listen: vec![],
            run_dir: None,
            created_at: "t".into(),
        };
        let a = engine_args(&home, &cfg, Path::new("/h/.pgx/run"));
        assert_eq!(
            a[..6],
            ["-D", "/h/.pgx/data", "-k", "/h/.pgx/run", "-p", "5455"]
        );
        assert!(a.contains(&"io_method=sync".to_string()));
    }
}
