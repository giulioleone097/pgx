//! Where things are: the installation prefix (relative to the `pgx` binary)
//! and the data home (`$PGX_HOME`, default `~/.pgx`).
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub prefix: PathBuf,
    pub engine: PathBuf,
    pub share: PathBuf,
    pub base_cluster: PathBuf,
    pub dist_json: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// prefix-relative path or short name of the thing that is wrong
    pub what: String,
    pub detail: String,
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DistInfo {
    pub pgx_version: String,
    pub engine_commit: String,
    pub postgres_compat: String,
    pub initdb_version: String,
    pub os: String,
    pub arch: String,
    pub built_at: String,
}

impl Install {
    pub fn from_prefix(prefix: &Path) -> Install {
        Install {
            prefix: prefix.to_path_buf(),
            engine: prefix.join("bin/postgres"),
            share: prefix.join("share"),
            base_cluster: prefix.join("share/pgx/base-cluster"),
            dist_json: prefix.join("share/pgx/dist.json"),
        }
    }

    /// The prefix is the parent of the directory holding the (symlink-resolved) `pgx` binary.
    pub fn discover() -> Result<Install> {
        let exe = std::env::current_exe().map_err(|e| {
            PgxError::new(
                ExitCode::Corrupt,
                format!("cannot locate the pgx executable: {e}"),
            )
        })?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let bindir = exe.parent().ok_or_else(|| {
            PgxError::new(ExitCode::Corrupt, "pgx executable has no parent directory")
        })?;
        let prefix = bindir.parent().ok_or_else(|| {
            PgxError::new(
                ExitCode::Corrupt,
                "pgx executable is not inside a <prefix>/bin directory",
            )
        })?;
        Ok(Install::from_prefix(prefix))
    }

    pub fn verify(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut need = |rel: &str, kind: &str| {
            let p = self.prefix.join(rel);
            let ok = match kind {
                "dir" => p.is_dir(),
                _ => p.is_file(),
            };
            if !ok {
                out.push(Problem {
                    what: rel.to_string(),
                    detail: format!("missing {kind} {}", p.display()),
                    fatal: true,
                });
            }
        };
        need("bin/postgres", "file");
        need("share/timezone", "dir");
        need("share/timezonesets", "dir");
        need("share/tsearch_data", "dir");
        need("share/extension", "dir");
        need("share/pgx/base-cluster/PG_VERSION", "file");
        need("share/pgx/base-cluster/global/pg_control", "file");
        need("share/pgx/base-cluster/base", "dir");
        need("share/pgx/dist.json", "file");
        out
    }

    /// Fails with exit code 7 when the installation is incomplete.
    pub fn require_complete(&self) -> Result<()> {
        let problems = self.verify();
        if problems.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = problems.iter().map(|p| p.detail.clone()).collect();
        Err(PgxError::new(
            ExitCode::Corrupt,
            format!(
                "incomplete PGX installation at {}:\n  {}",
                self.prefix.display(),
                list.join("\n  ")
            ),
        )
        .hint("reinstall PGX (curl -fsSL https://pgx.dev/install | sh) or run `pgx doctor`"))
    }

    pub fn dist_info(&self) -> Result<DistInfo> {
        let text = std::fs::read_to_string(&self.dist_json).map_err(|e| {
            PgxError::new(
                ExitCode::Corrupt,
                format!("cannot read {}: {e}", self.dist_json.display()),
            )
        })?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            PgxError::new(
                ExitCode::Corrupt,
                format!("{} is not valid JSON: {e}", self.dist_json.display()),
            )
        })?;
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
                .to_string()
        };
        Ok(DistInfo {
            pgx_version: s("pgx_version"),
            engine_commit: s("engine_commit"),
            postgres_compat: s("postgres_compat"),
            initdb_version: s("initdb_version"),
            os: s("os"),
            arch: s("arch"),
            built_at: s("built_at"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub root: PathBuf,
    pub data: PathBuf,
    pub run: PathBuf,
    pub log_file: PathBuf,
    pub config_file: PathBuf,
}

impl Home {
    pub fn from_env(pgx_home: Option<&str>, home: Option<&str>) -> Home {
        let root = match (pgx_home, home) {
            (Some(p), _) if !p.is_empty() => PathBuf::from(p),
            (_, Some(h)) if !h.is_empty() => Path::new(h).join(".pgx"),
            _ => PathBuf::from(".pgx"),
        };
        Home::at(root)
    }

    pub fn at(root: PathBuf) -> Home {
        Home {
            data: root.join("data"),
            run: root.join("run"),
            log_file: root.join("log/pgx.log"),
            config_file: root.join("config.json"),
            root,
        }
    }

    pub fn resolve() -> Home {
        let pgx_home = std::env::var("PGX_HOME").ok();
        let home = std::env::var("HOME").ok();
        let home = Home::from_env(pgx_home.as_deref(), home.as_deref());
        // PGX_HOME is an explicit user-selected location; aliases such as
        // /tmp -> /private/tmp and home symlinks must share process identity.
        Home::at(std::fs::canonicalize(&home.root).unwrap_or(home.root))
    }

    pub fn is_initialized(&self) -> bool {
        self.data.join("PG_VERSION").is_file() && self.config_file.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn home_defaults_to_dot_pgx() {
        let h = Home::from_env(None, Some("/Users/someone"));
        assert_eq!(h.root, Path::new("/Users/someone/.pgx"));
        assert_eq!(h.data, Path::new("/Users/someone/.pgx/data"));
        assert_eq!(h.run, Path::new("/Users/someone/.pgx/run"));
        assert_eq!(h.log_file, Path::new("/Users/someone/.pgx/log/pgx.log"));
        assert_eq!(h.config_file, Path::new("/Users/someone/.pgx/config.json"));
    }

    #[test]
    fn home_env_override() {
        let h = Home::from_env(Some("/tmp/my pgx"), Some("/Users/someone"));
        assert_eq!(h.root, Path::new("/tmp/my pgx"));
        assert_eq!(h.data, Path::new("/tmp/my pgx/data"));
    }

    #[test]
    fn install_from_prefix_names_every_file() {
        let i = Install::from_prefix(Path::new("/opt/pgx dist"));
        assert_eq!(i.engine, Path::new("/opt/pgx dist/bin/postgres"));
        assert_eq!(i.share, Path::new("/opt/pgx dist/share"));
        assert_eq!(
            i.base_cluster,
            Path::new("/opt/pgx dist/share/pgx/base-cluster")
        );
        assert_eq!(i.dist_json, Path::new("/opt/pgx dist/share/pgx/dist.json"));
    }

    #[test]
    fn install_verify_reports_each_missing_file() {
        let tmp = std::env::temp_dir().join(format!("pgx-layout-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bin")).unwrap();
        std::fs::create_dir_all(tmp.join("share/timezone")).unwrap();
        std::fs::write(tmp.join("bin/postgres"), b"").unwrap();
        let i = Install::from_prefix(&tmp);
        let problems = i.verify();
        let names: Vec<&str> = problems.iter().map(|p| p.what.as_str()).collect();
        assert!(names.contains(&"share/timezonesets"), "{names:?}");
        assert!(names.contains(&"share/tsearch_data"), "{names:?}");
        assert!(names.contains(&"share/extension"), "{names:?}");
        assert!(
            names.contains(&"share/pgx/base-cluster/PG_VERSION"),
            "{names:?}"
        );
        assert!(names.contains(&"share/pgx/dist.json"), "{names:?}");
        assert!(!names.contains(&"bin/postgres"), "{names:?}");
        assert!(!names.contains(&"share/timezone"), "{names:?}");
        assert!(problems.iter().all(|p| p.fatal));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn install_verify_clean_layout_has_no_problems() {
        let tmp = std::env::temp_dir().join(format!("pgx-layout-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in [
            "bin",
            "share/timezone",
            "share/timezonesets",
            "share/tsearch_data",
            "share/extension",
            "share/pgx/base-cluster/global",
            "share/pgx/base-cluster/base",
        ] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        std::fs::write(tmp.join("bin/postgres"), b"").unwrap();
        std::fs::write(tmp.join("share/pgx/base-cluster/PG_VERSION"), b"18\n").unwrap();
        std::fs::write(tmp.join("share/pgx/base-cluster/global/pg_control"), b"").unwrap();
        std::fs::write(tmp.join("share/pgx/dist.json"), br#"{"pgx_version":"0.1.0","engine_commit":"abc","postgres_compat":"18.6","initdb_version":"18.6","os":"darwin","arch":"arm64","built_at":"now"}"#).unwrap();
        let i = Install::from_prefix(&tmp);
        assert!(i.verify().is_empty());
        let d = i.dist_info().unwrap();
        assert_eq!(d.pgx_version, "0.1.0");
        assert_eq!(d.engine_commit, "abc");
        assert_eq!(d.postgres_compat, "18.6");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
