//! Filesystem helpers: recursive copy with private modes, random secrets.
use crate::errors::{PgxError, Result};
use std::path::Path;

/// Open and hold an advisory file lock. A busy non-blocking lock is None;
/// filesystem errors remain errors rather than masquerading as another worker.
pub fn lock_file(path: &Path, blocking: bool) -> Result<Option<std::fs::File>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| PgxError::io("cannot open lock", path, &e))?;
    let operation = libc::LOCK_EX | if blocking { 0 } else { libc::LOCK_NB };
    if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
        let error = std::io::Error::last_os_error();
        if !blocking && error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(None);
        }
        return Err(PgxError::io("cannot lock", path, &error));
    }
    Ok(Some(file))
}

/// Create a private directory, or secure an existing directory owned by us.
/// Opening without following the final symlink prevents chmod/socket use of an
/// attacker-supplied path. Reject writable ancestors (except sticky directories
/// owned by root or us), where another user could replace our directory.
pub fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute = std::env::current_dir()
            .map_err(|e| PgxError::failure(format!("cannot resolve current directory: {e}")))?
            .join(path);
        absolute.as_path()
    };
    let uid = unsafe { libc::geteuid() };
    for parent in path.ancestors().skip(1) {
        if parent.as_os_str().is_empty() {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(parent) {
            if meta.mode() & 0o022 != 0
                && !((meta.uid() == 0 || meta.uid() == uid) && meta.mode() & 0o1000 != 0)
            {
                return Err(PgxError::failure(format!(
                    "unsafe writable parent directory {}",
                    parent.display()
                )));
            }
        }
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| PgxError::io("cannot create private directory", path, &e))?;
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| PgxError::io("cannot open private directory", path, &e))?;
    let meta = dir
        .metadata()
        .map_err(|e| PgxError::io("cannot stat", path, &e))?;
    if meta.uid() != uid {
        return Err(PgxError::failure(format!(
            "directory {} is owned by another user",
            path.display()
        )));
    }
    dir.set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|e| PgxError::io("cannot secure directory", path, &e))
}

/// Copies `src` into `dst` (created), directories 0700, files 0600.
pub fn copy_tree_private(src: &Path, dst: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir(dst).map_err(|e| PgxError::io("cannot create", dst, &e))?;
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| PgxError::io("cannot chmod", dst, &e))?;
    for entry in std::fs::read_dir(src).map_err(|e| PgxError::io("cannot read", src, &e))? {
        let entry = entry.map_err(|e| PgxError::io("cannot read entry in", src, &e))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry
            .file_type()
            .map_err(|e| PgxError::io("cannot stat", &from, &e))?;
        if ft.is_dir() {
            copy_tree_private(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to).map_err(|e| PgxError::io("cannot copy", &from, &e))?;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| PgxError::io("cannot chmod", &to, &e))?;
        } else {
            return Err(PgxError::failure(format!(
                "unexpected non-regular file in the base cluster: {}",
                from.display()
            )));
        }
    }
    Ok(())
}

/// 32 characters from [A-Za-z0-9], from the OS random source.
pub fn random_password() -> Result<String> {
    use std::io::Read;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| PgxError::failure(format!("cannot open /dev/urandom: {e}")))?;
    let mut out = String::with_capacity(32);
    let mut buf = [0u8; 64];
    while out.len() < 32 {
        f.read_exact(&mut buf)
            .map_err(|e| PgxError::failure(format!("cannot read /dev/urandom: {e}")))?;
        for b in buf {
            // rejection sampling keeps the distribution uniform
            if (b as usize) < 62 * 4 {
                out.push(ALPHABET[(b % 62) as usize] as char);
                if out.len() == 32 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Current time as whole Unix epoch seconds (UTC). Monotonic enough for TTL
/// deadlines; TTL math never relies on sub-second precision.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    rfc3339(now_epoch())
}

/// Format Unix epoch seconds as an RFC 3339 UTC timestamp.
pub fn rfc3339(secs: u64) -> String {
    // civil-from-days (Howard Hinnant), UTC
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_32_alnum() {
        let p = random_password().unwrap();
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(p, random_password().unwrap());
    }

    #[test]
    fn private_directory_rejects_symlinks_and_repairs_modes() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("pgx-private-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        assert!(private_dir(&base.join("link")).is_err());
        private_dir(&base.join("real")).unwrap();
        assert_eq!(
            std::fs::metadata(base.join("real"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::set_permissions(base.join("real"), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(
            private_dir(&base.join("real/child")).is_err(),
            "our own shared writable parent also allows replacement by another user"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rfc3339_shape() {
        let t = now_rfc3339();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.starts_with("20"), "{t}");
        assert!(t.ends_with('Z'));
    }

    #[test]
    fn copy_tree_sets_private_modes() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("pgx-copytree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src/sub")).unwrap();
        std::fs::write(base.join("src/a"), b"a").unwrap();
        std::fs::write(base.join("src/sub/b"), b"bb").unwrap();
        copy_tree_private(&base.join("src"), &base.join("dst")).unwrap();
        assert_eq!(std::fs::read(base.join("dst/sub/b")).unwrap(), b"bb");
        assert_eq!(
            std::fs::metadata(base.join("dst"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(base.join("dst/a"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            copy_tree_private(&base.join("src"), &base.join("dst")).is_err(),
            "existing destination must be refused"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
