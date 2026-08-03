use std::path::Path;

use crate::error::{Error, Result};

/// A stable, opaque per-install identifier.
///
/// Used for seat counting and support diagnostics. Intentionally random rather
/// than derived from hardware: it identifies an install, not a person or a
/// machine, and it never leaves the device unless the user pastes it into a
/// support ticket.
pub fn fingerprint(path: &Path) -> Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(trimmed.to_string());
        }
    }

    let id = random_hex16()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(path, &id).map_err(|e| Error::io(path, e))?;
    Ok(id)
}

/// 16 random bytes as lowercase hex, from the OS entropy source.
fn random_hex16() -> Result<String> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let path = Path::new("/dev/urandom");
        let mut file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
        let mut buf = [0u8; 16];
        file.read_exact(&mut buf).map_err(|e| Error::io(path, e))?;
        Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[cfg(not(unix))]
    {
        Err(Error::InvalidVerifyingKey(
            "no entropy source on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine-id");
        let first = fingerprint(&path).unwrap();
        let second = fingerprint(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn differs_between_installs() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = fingerprint(&a.path().join("id")).unwrap();
        let two = fingerprint(&b.path().join("id")).unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn replaces_a_corrupt_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine-id");
        std::fs::write(&path, "not-a-valid-id").unwrap();
        let id = fingerprint(&path).unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
