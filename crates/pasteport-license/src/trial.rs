use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Length of the evaluation period. Pasteport is paid-only: after this, a
/// license is required.
pub const TRIAL_DAYS: i64 = 14;

const SECS_PER_DAY: i64 = 86_400;

/// On-disk record of when the trial began.
///
/// This is a courtesy timer, not DRM. Someone who deletes the file gets a fresh
/// 14 days, and that is a deliberate choice: locking the machine down would
/// cost honest users more than it costs anyone else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    pub started_at: i64,
    /// Fingerprint of the install that started it, so a copied home directory
    /// is visible in support logs.
    pub machine: String,
}

impl Trial {
    /// Load the trial record, starting one if this is the first run.
    pub fn load_or_start(path: &Path, machine: &str, now: i64) -> Result<Trial> {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Trial>(&raw) {
                Ok(trial) => Ok(trial),
                Err(e) => {
                    // A corrupt file must not hand out an unlimited trial, nor
                    // lock the user out. Restart the clock from now.
                    tracing::warn!(error = %e, "trial record unreadable; restarting trial clock");
                    let trial = Trial {
                        started_at: now,
                        machine: machine.to_string(),
                    };
                    trial.save(path)?;
                    Ok(trial)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let trial = Trial {
                    started_at: now,
                    machine: machine.to_string(),
                };
                trial.save(path)?;
                Ok(trial)
            }
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Read the trial record without creating one.
    pub fn peek(path: &Path) -> Result<Option<Trial>> {
        match std::fs::read_to_string(path) {
            Ok(raw) => Ok(serde_json::from_str(&raw).ok()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let body = serde_json::to_string_pretty(self).map_err(Error::Encode)?;
        std::fs::write(path, body).map_err(|e| Error::io(path, e))?;
        restrict(path)
    }

    pub fn expires_at(&self) -> i64 {
        self.started_at + TRIAL_DAYS * SECS_PER_DAY
    }

    pub fn is_active_at(&self, now: i64) -> bool {
        // A clock rolled backwards past the start date is treated as still in
        // trial rather than as expired: the user is not the one at fault.
        now < self.expires_at()
    }

    /// Whole days remaining, rounded up, saturating at zero.
    pub fn days_left_at(&self, now: i64) -> u32 {
        let remaining = self.expires_at() - now;
        if remaining <= 0 {
            return 0;
        }
        ((remaining + SECS_PER_DAY - 1) / SECS_PER_DAY) as u32
    }
}

fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| Error::io(path, e))?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|e| Error::io(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trial.json");
        (dir, path)
    }

    #[test]
    fn first_run_starts_the_clock_and_persists_it() {
        let (_d, path) = tmp();
        let t = Trial::load_or_start(&path, "machine-a", 1_000).unwrap();
        assert_eq!(t.started_at, 1_000);

        // A later run must not reset it.
        let again = Trial::load_or_start(&path, "machine-a", 9_999_999).unwrap();
        assert_eq!(again.started_at, 1_000);
    }

    #[test]
    fn counts_down_and_expires() {
        let t = Trial {
            started_at: 0,
            machine: "m".into(),
        };
        assert_eq!(t.days_left_at(0), TRIAL_DAYS as u32);
        assert!(t.is_active_at(0));

        // Halfway through.
        assert_eq!(t.days_left_at(7 * SECS_PER_DAY), 7);
        assert!(t.is_active_at(7 * SECS_PER_DAY));

        // Final second.
        assert_eq!(t.days_left_at(t.expires_at() - 1), 1);
        assert!(t.is_active_at(t.expires_at() - 1));

        // Expired.
        assert_eq!(t.days_left_at(t.expires_at()), 0);
        assert!(!t.is_active_at(t.expires_at()));
        assert_eq!(t.days_left_at(t.expires_at() + 999_999), 0);
    }

    #[test]
    fn a_backwards_clock_does_not_expire_the_trial() {
        let t = Trial {
            started_at: 1_000_000,
            machine: "m".into(),
        };
        assert!(
            t.is_active_at(0),
            "a wrong system clock must not lock the user out"
        );
    }

    #[test]
    fn corrupt_record_restarts_rather_than_granting_forever() {
        let (_d, path) = tmp();
        std::fs::write(&path, "{ not json").unwrap();
        let t = Trial::load_or_start(&path, "m", 5_000).unwrap();
        assert_eq!(t.started_at, 5_000);
        // And it is repaired on disk.
        assert_eq!(Trial::peek(&path).unwrap().unwrap().started_at, 5_000);
    }

    #[test]
    fn peek_does_not_create_a_trial() {
        let (_d, path) = tmp();
        assert!(Trial::peek(&path).unwrap().is_none());
        assert!(!path.exists(), "peek must not start the clock");
    }

    #[test]
    fn record_is_owner_only() {
        let (_d, path) = tmp();
        Trial::load_or_start(&path, "m", 1).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
