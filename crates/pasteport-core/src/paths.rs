use std::path::PathBuf;

use crate::error::{Error, Result};

/// Where Pasteport keeps its state.
///
/// macOS: `~/Library/Application Support/Pasteport`
/// Linux: `$XDG_DATA_HOME/pasteport` (default `~/.local/share/pasteport`)
///
/// `PASTEPORT_DATA_DIR` overrides both, which is what the tests and the
/// `--data-dir` flag use.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("PASTEPORT_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = directories::BaseDirs::new().ok_or(Error::NoDataDir)?;

    // `BaseDirs::data_dir()` is `~/Library/Application Support` on macOS and
    // `$XDG_DATA_HOME` (default `~/.local/share`) on Linux, so one join covers
    // both. The leaf differs because the platform conventions differ: title case
    // on macOS, lowercase on Linux.
    //
    // Deliberately not `ProjectDirs::from("com", "pasteport", …)`, which yields
    // `~/Library/Application Support/com.pasteport.Pasteport`. That is a legal
    // location but an unfriendly one to tell somebody to open in Finder.
    let leaf = if cfg!(target_os = "macos") {
        "Pasteport"
    } else {
        "pasteport"
    };
    let dir = base.data_dir().join(leaf);

    migrate_legacy_dir(&dir, base.data_dir());
    Ok(dir)
}

/// Move a pre-0.1 data directory to the current location, once.
///
/// 0.1.0 development builds used the reverse-DNS name that `ProjectDirs`
/// produces. Anyone who ran one of those has a history there, and silently
/// starting from an empty database would look like data loss. Only renames when
/// the new location does not exist yet, so it can never clobber anything.
///
/// Delete this after 0.1 ships; it exists for a window of a few days.
fn migrate_legacy_dir(current: &std::path::Path, base: &std::path::Path) {
    if current.exists() {
        return;
    }
    let legacy = base.join("com.pasteport.Pasteport");
    if !legacy.is_dir() {
        return;
    }
    match std::fs::rename(&legacy, current) {
        Ok(()) => tracing::info!(
            from = %legacy.display(), to = %current.display(),
            "moved clipboard history to its current location"
        ),
        // Not fatal: the caller creates a fresh directory and carries on.
        Err(e) => tracing::warn!(error = %e, "could not move the legacy data directory"),
    }
}

pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("history.sqlite3"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.toml"))
}

/// Unix domain socket paths are bounded by `sun_path`: 104 bytes on macOS and
/// the BSDs, 108 on Linux. Take the smaller, minus room for the NUL.
const MAX_SOCKET_PATH_LEN: usize = 100;

/// Path of the daemon's control socket.
///
/// Normally this sits in the data dir, so it inherits the same `0700` parent and
/// cannot be pre-created by another local user.
///
/// When the data dir is deep enough that the socket path would exceed
/// `sun_path`, it falls back to a short path in the per-user runtime directory.
/// Without that fallback, a long `--data-dir` produces `path must be shorter
/// than SUN_LEN`, which is an unhelpful thing to hand somebody.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PASTEPORT_SOCKET") {
        // An explicit override is the caller's business, length included.
        return Ok(PathBuf::from(p));
    }

    let dir = data_dir()?;
    let preferred = dir.join("daemon.sock");
    if preferred.as_os_str().len() <= MAX_SOCKET_PATH_LEN {
        return Ok(preferred);
    }

    // Name the fallback after a digest of the data dir, so two data dirs never
    // share a socket and the mapping stays stable across restarts.
    let digest = blake3::hash(dir.as_os_str().as_encoded_bytes()).to_hex();
    let short = runtime_dir()?.join(format!("{}.sock", &digest[..16]));
    if short.as_os_str().len() > MAX_SOCKET_PATH_LEN {
        return Err(Error::SocketPathTooLong(short));
    }
    Ok(short)
}

/// Per-user directory for runtime files such as the fallback socket.
///
/// `XDG_RUNTIME_DIR` on Linux is exactly this and is already `0700`. macOS
/// gives every user a private `$TMPDIR` under `/var/folders`. Where neither
/// applies we fall back to the system temp dir and create our own `0700`
/// subdirectory, refusing to use one somebody else owns.
fn runtime_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    let dir = base.join("pasteport");
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    restrict_to_owner(&dir)?;
    ensure_owned_by_us(&dir)?;
    Ok(dir)
}

/// Refuse to place a socket inside a directory another user owns.
///
/// The data dir is under `$HOME` and needs no such check, but the runtime
/// fallback can land in a shared `/tmp` where a hostile local user could have
/// pre-created the path.
fn ensure_owned_by_us(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        let us = rustix::process::getuid().as_raw();
        if meta.uid() != us {
            return Err(Error::NotOurDirectory(path.to_path_buf()));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Whether a socket path fits in `sun_path` on this platform.
pub fn socket_path_fits(path: &std::path::Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH_LEN
}

/// Create the data dir if needed, owner-only.
pub fn ensure_data_dir() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    restrict_to_owner(&dir)?;
    Ok(dir)
}

/// Clipboard history is among the most sensitive data on a machine. Every file
/// and directory we create is owner-only, on every platform.
pub fn restrict_to_owner(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        let mode = if meta.is_dir() { 0o700 } else { 0o600 };
        let mut perms = meta.permissions();
        if perms.mode() & 0o777 != mode {
            perms.set_mode(mode);
            std::fs::set_permissions(path, perms).map_err(|e| Error::io(path, e))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests mutate process-wide environment variables, so they share one
    /// mutex rather than racing each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct DataDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl DataDirGuard {
        fn set(dir: &std::path::Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var_os("PASTEPORT_DATA_DIR");
            std::env::set_var("PASTEPORT_DATA_DIR", dir);
            DataDirGuard {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => std::env::set_var("PASTEPORT_DATA_DIR", v),
                None => std::env::remove_var("PASTEPORT_DATA_DIR"),
            }
        }
    }

    #[test]
    fn short_data_dir_keeps_the_socket_beside_the_database() {
        let dir = std::path::Path::new("/tmp/pp");
        let _guard = DataDirGuard::set(dir);

        let socket = socket_path().unwrap();
        assert_eq!(socket, dir.join("daemon.sock"));
        assert!(socket_path_fits(&socket));
    }

    #[test]
    fn long_data_dir_falls_back_to_a_short_runtime_path() {
        // Regression: a deep --data-dir used to produce a path longer than
        // sun_path, and the daemon failed with "path must be shorter than
        // SUN_LEN" instead of anything actionable.
        let deep = std::path::PathBuf::from("/tmp").join("x".repeat(150));
        let _guard = DataDirGuard::set(&deep);

        let socket = socket_path().unwrap();
        assert!(
            socket_path_fits(&socket),
            "fallback socket path must fit in sun_path, got {} bytes: {}",
            socket.as_os_str().len(),
            socket.display()
        );
        assert!(
            !socket.starts_with(&deep),
            "the fallback must leave the deep directory"
        );
        assert_eq!(socket.extension().and_then(|e| e.to_str()), Some("sock"));
    }

    #[test]
    fn the_fallback_path_is_stable_for_a_given_data_dir() {
        let deep = std::path::PathBuf::from("/tmp").join("y".repeat(150));

        let first = {
            let _g = DataDirGuard::set(&deep);
            socket_path().unwrap()
        };
        let second = {
            let _g = DataDirGuard::set(&deep);
            socket_path().unwrap()
        };
        assert_eq!(first, second, "restarting must find the same socket");
    }

    #[test]
    fn different_long_data_dirs_get_different_sockets() {
        let a = std::path::PathBuf::from("/tmp").join("a".repeat(150));
        let b = std::path::PathBuf::from("/tmp").join("b".repeat(150));

        let socket_a = {
            let _g = DataDirGuard::set(&a);
            socket_path().unwrap()
        };
        let socket_b = {
            let _g = DataDirGuard::set(&b);
            socket_path().unwrap()
        };
        assert_ne!(socket_a, socket_b, "two data dirs must not share a socket");
    }

    #[test]
    fn explicit_socket_override_is_honoured_verbatim() {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PASTEPORT_SOCKET");
        std::env::set_var("PASTEPORT_SOCKET", "/tmp/my-own.sock");

        let socket = socket_path().unwrap();
        assert_eq!(socket, std::path::Path::new("/tmp/my-own.sock"));

        match previous {
            Some(v) => std::env::set_var("PASTEPORT_SOCKET", v),
            None => std::env::remove_var("PASTEPORT_SOCKET"),
        }
        drop(lock);
    }

    #[test]
    fn runtime_dir_is_owner_only() {
        let dir = runtime_dir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the runtime dir must not be shared");
        }
        assert!(dir.ends_with("pasteport"));
    }

    #[test]
    fn default_data_dir_is_the_friendly_platform_path() {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PASTEPORT_DATA_DIR");
        std::env::remove_var("PASTEPORT_DATA_DIR");

        let dir = data_dir().unwrap();
        if cfg!(target_os = "macos") {
            assert!(
                dir.ends_with("Library/Application Support/Pasteport"),
                "expected the documented macOS path, got {}",
                dir.display()
            );
        } else {
            assert!(dir.ends_with("pasteport"), "got {}", dir.display());
        }
        // The reverse-DNS name ProjectDirs would have produced is not it.
        assert!(!dir.to_string_lossy().contains("com.pasteport"));

        match previous {
            Some(v) => std::env::set_var("PASTEPORT_DATA_DIR", v),
            None => std::env::remove_var("PASTEPORT_DATA_DIR"),
        }
        drop(lock);
    }

    #[test]
    fn legacy_directory_is_adopted_but_never_clobbers() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let current = base.join("Pasteport");
        let legacy = base.join("com.pasteport.Pasteport");

        // A legacy dir with no current one is moved across.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("history.sqlite3"), b"old").unwrap();
        migrate_legacy_dir(&current, base);
        assert!(
            current.join("history.sqlite3").exists(),
            "history should have moved"
        );
        assert!(!legacy.exists());

        // With both present, the current one wins and the legacy is left alone.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("history.sqlite3"), b"stale").unwrap();
        migrate_legacy_dir(&current, base);
        assert_eq!(
            std::fs::read(current.join("history.sqlite3")).unwrap(),
            b"old"
        );
        assert!(
            legacy.exists(),
            "an existing current dir must not be replaced"
        );
    }

    #[test]
    fn derived_paths_all_live_under_the_data_dir() {
        let dir = std::path::Path::new("/tmp/pp-derived");
        let _guard = DataDirGuard::set(dir);

        assert_eq!(database_path().unwrap(), dir.join("history.sqlite3"));
        assert_eq!(config_path().unwrap(), dir.join("config.toml"));
    }
}
