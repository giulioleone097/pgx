//! `pgx init`: copy the base cluster, write the configuration, set the
//! superuser password, record config.json.
use crate::cli::ExitCode;
use crate::config::{Config, Durability};
use crate::errors::{PgxError, Result};
use crate::layout::{Home, Install};
use crate::out::Out;
use crate::pgwire::quote_literal;
use crate::{conf, fsdetect, fsutil, process};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// sun_path is 104 bytes on macOS (103 + NUL), 108 on Linux; use the stricter one.
pub const MAX_SOCKET_PATH: usize = 103;

/// Ok when both loopback addresses can be bound; Err names the address and the OS error.
pub fn port_check(port: u16) -> std::result::Result<(), String> {
    let v4 = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    let v6 = match TcpListener::bind(("::1", port)) {
        Ok(l) => Some(l),
        Err(e)
            if matches!(e.kind(), std::io::ErrorKind::AddrNotAvailable)
                || e.raw_os_error() == Some(libc::EAFNOSUPPORT) =>
        {
            None
        }
        Err(e) => return Err(format!("bind [::1]:{port}: {e}")),
    };
    drop(v4);
    drop(v6);
    Ok(())
}

pub fn port_is_free(port: u16) -> bool {
    port_check(port).is_ok()
}

pub fn choose_port(requested: Option<u16>) -> Result<u16> {
    if let Some(p) = requested {
        if let Err(why) = port_check(p) {
            return Err(
                PgxError::new(ExitCode::Conflict, format!("port {p} is in use ({why})"))
                    .hint("choose another with `pgx init --port N`, or stop the process using it"),
            );
        }
        return Ok(p);
    }
    (5432..=5500).find(|p| port_is_free(*p)).ok_or_else(|| {
        PgxError::new(ExitCode::Conflict, "no free port between 5432 and 5500")
            .hint("give one with `pgx init --port N`")
    })
}

pub fn socket_path_len(sock_dir: &Path, port: u16) -> usize {
    sock_dir.join(format!(".s.PGSQL.{port}")).as_os_str().len()
}

pub fn run(
    out: &Out,
    port: Option<u16>,
    durability: Option<String>,
    force: bool,
) -> Result<ExitCode> {
    let install = Install::discover()?;
    install.require_complete()?;
    let home = Home::resolve();
    // Resolve settings before touching an existing cluster. Explicit flags win
    // over the environment; every implicit init (run/serve) uses this same path.
    let port = match port {
        Some(p) => Some(p),
        None => match env_setting("PGX_PORT")? {
            Some(p) => Some(
                p.parse::<u16>()
                    .ok()
                    .filter(|p| *p >= 1024)
                    .ok_or_else(|| {
                        PgxError::usage(format!(
                            "PGX_PORT must be a number between 1024 and 65535, got {p:?}"
                        ))
                    })?,
            ),
            None => None,
        },
    };
    let durability = match durability {
        Some(d) => Durability::parse(&d)?,
        None => match env_setting("PGX_DURABILITY")? {
            Some(d) => Durability::parse(&d).map_err(|_| {
                PgxError::usage(format!(
                    "PGX_DURABILITY must be `ephemeral` or `full`, got {d:?}"
                ))
            })?,
            None => Durability::Ephemeral,
        },
    };

    if home.data.exists() || home.config_file.exists() {
        if !force {
            return Err(PgxError::new(ExitCode::Conflict, format!("PGX is already initialised at {}", home.root.display()))
                .hint("`pgx start` to use it, or `pgx init --force` to recreate it (this deletes every database)"));
        }
        if let Some(pf) = process::read_pid_file(&home.data) {
            if process::is_our_engine(pf.pid, &home.data) {
                return Err(PgxError::new(
                    ExitCode::Conflict,
                    format!("PGX is running (pid {})", pf.pid),
                )
                .hint("`pgx stop` first"));
            }
            // Even if we cannot prove it is our engine, refuse to delete the data
            // directory while the recorded PID is alive: it may be a running
            // postmaster we failed to recognise.
            if process::pid_alive(pf.pid) {
                return Err(PgxError::new(
                    ExitCode::Conflict,
                    format!("a process (pid {}) recorded in postmaster.pid is still alive", pf.pid),
                )
                .hint("stop it first; `pgx init --force` will not delete the data directory while that PID is alive"));
            }
        }
        out.note(&format!("removing {}", home.data.display()));
        if home.data.exists() {
            std::fs::remove_dir_all(&home.data)
                .map_err(|e| PgxError::io("cannot remove", &home.data, &e))?;
        }
        let _ = std::fs::remove_file(&home.config_file);
    }

    let run_override = std::env::var("PGX_RUN_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let sock_dir = run_override.clone().unwrap_or_else(|| home.run.clone());
    let port = choose_port(port)?;
    let len = socket_path_len(&sock_dir, port);
    if len > MAX_SOCKET_PATH {
        return Err(PgxError::new(
            ExitCode::Usage,
            format!("the Unix socket path would be {len} bytes ({}/.s.PGSQL.{port}); the limit is {MAX_SOCKET_PATH}", sock_dir.display()),
        )
        .hint("set PGX_RUN_DIR to a short directory (for example /tmp/pgx) and run `pgx init` again, or use a shorter PGX_HOME"));
    }

    let listen_for_hba: Vec<String> = match std::env::var("PGX_LISTEN") {
        Ok(v) if !v.trim().is_empty() => v
            .split(',')
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        _ => vec!["127.0.0.1".into(), "::1".into()],
    };
    std::fs::create_dir_all(&home.root)
        .map_err(|e| PgxError::io("cannot create", &home.root, &e))?;
    std::fs::create_dir_all(&sock_dir).map_err(|e| PgxError::io("cannot create", &sock_dir, &e))?;

    // The cluster: copy to data.tmp, configure, then rename into place.
    let tmp = home.root.join("data.tmp");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| PgxError::io("cannot remove", &tmp, &e))?;
    }
    out.note(&format!(
        "creating cluster from {}",
        install.base_cluster.display()
    ));
    let created = (|| -> Result<()> {
        fsutil::copy_tree_private(&install.base_cluster, &tmp)?;
        write(
            &tmp.join("postgresql.conf"),
            &conf::render_postgresql_conf(),
        )?;
        write(&tmp.join("pg_hba.conf"), &conf::render_hba(&listen_for_hba))?;
        write(
            &tmp.join("pg_ident.conf"),
            "# pg_ident.conf — written by pgx init.\n",
        )?;
        Ok(())
    })();
    if let Err(e) = created {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let password = match std::env::var("PGX_PASSWORD") {
        Ok(p) if p.len() >= 8 => p,
        Ok(p) if !p.is_empty() => {
            return Err(PgxError::usage(format!(
                "PGX_PASSWORD is too short ({} chars); use at least 8",
                p.len()
            )))
        }
        _ => fsutil::random_password()?,
    };
    let config = Config {
        port,
        password: password.clone(),
        durability,
        listen: listen_for_hba.clone(),
        run_dir: run_override,
        created_at: fsutil::now_rfc3339(),
    };
    let block_clone = fsdetect::supports_block_clone(&tmp);
    write(
        &tmp.join("pgx.conf"),
        &conf::render_pgx_conf(&config, &sock_dir, block_clone),
    )?;
    std::fs::rename(&tmp, &home.data)
        .map_err(|e| PgxError::io("cannot rename into place", &home.data, &e))?;

    // Set the superuser password: start on the socket only, ALTER ROLE, stop.
    out.note("setting the superuser password");
    let result = set_password(&install, &home, &config, &sock_dir, &password);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&home.data);
        return Err(e);
    }
    config.save(&home.config_file)?;

    let text = format!(
        "initialised PGX at {}\n  port        {}\n  durability  {} ({})\n  storage     {}\n\nnext: pgx start",
        home.root.display(),
        port,
        durability.as_str(),
        match durability {
            Durability::Ephemeral => "survives a server crash, not an OS crash",
            Durability::Full => "PostgreSQL defaults",
        },
        if block_clone { "block cloning available (clone = copy-on-write)" } else { "no block cloning (clone = file copy)" }
    );
    out.emit(
        &text,
        serde_json::json!({
            "home": home.root, "data_dir": home.data, "port": port, "durability": durability.as_str(),
            "socket_dir": sock_dir, "block_clone": block_clone, "config": home.config_file,
        }),
    );
    Ok(ExitCode::Ok)
}

fn env_setting(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(PgxError::usage(format!("{name} must be valid UTF-8")))
        }
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).map_err(|e| PgxError::io("cannot write", path, &e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| PgxError::io("cannot chmod", path, &e))
}

fn set_password(
    install: &Install,
    home: &Home,
    config: &Config,
    sock_dir: &Path,
    password: &str,
) -> Result<()> {
    let mut child = process::spawn(
        install,
        home,
        config,
        sock_dir,
        &["-c", "listen_addresses="],
    )?;
    let pid = child.id();
    let outcome = (|| -> Result<()> {
        let mut client = process::wait_ready(
            home,
            sock_dir,
            config.port,
            Some(&mut child),
            Duration::from_secs(60),
        )?;
        client
            .simple_query(&format!(
                "ALTER ROLE postgres PASSWORD {}",
                quote_literal(password)
            ))
            .map_err(|e| PgxError::failure(format!("cannot set the superuser password: {e}")))?;
        client.close();
        Ok(())
    })();
    let stopped = process::stop(
        pid,
        process::StopMode::Force,
        Duration::from_secs(60),
        Some(&mut child),
    );
    outcome.and(stopped)
}
