//! Offline license verification for Pasteport.
//!
//! Pasteport is paid software with an open source codebase, so licensing is
//! built around three rules:
//!
//! 1. **Offline.** A key is an Ed25519 signature over a small JSON payload.
//!    Verification is local; nothing phones home, ever.
//! 2. **No DRM.** The trial timer is a courtesy, and a build compiled from
//!    source is fully functional. The paid product is the signed, notarized
//!    binary and the support that comes with it.
//! 3. **No signing code in shipped builds.** Minting lives behind the `mint`
//!    feature, which release binaries do not enable.
//!
//! ```
//! use pasteport_license::{Licensing, Status};
//!
//! // A build with no key compiled in reports itself as self-built and works.
//! let licensing = Licensing::unconfigured();
//! assert!(matches!(licensing.status_at(0), Status::SelfBuilt));
//! assert!(licensing.status_at(0).is_functional());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

use std::path::{Path, PathBuf};

use base64::Engine as _;
use ed25519_dalek::VerifyingKey;

mod error;
mod license;
mod machine;
mod trial;

#[cfg(feature = "mint")]
pub mod mint;

pub use error::{Error, Result};
pub use license::{License, LicensePayload, Plan, KEY_PREFIX};
pub use trial::{Trial, TRIAL_DAYS};

/// The public key compiled into this build, base64url encoded.
///
/// Set at build time by the release pipeline:
/// `PASTEPORT_LICENSE_PUBKEY=<base64url> cargo build --release`
///
/// When absent, the build is a source build and licensing is bypassed. That is
/// deliberate: the code is AGPL, so anyone can compile it, and pretending
/// otherwise would only mean a broken `cargo build` for contributors.
pub const EMBEDDED_PUBKEY_B64: Option<&str> = option_env!("PASTEPORT_LICENSE_PUBKEY");

/// Decode a base64url-encoded Ed25519 public key.
pub fn verifying_key_from_b64(b64: &str) -> Result<VerifyingKey> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64.trim())
        .map_err(|_| Error::InvalidVerifyingKey("not base64url"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::InvalidVerifyingKey("not 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| Error::InvalidVerifyingKey("not a valid Ed25519 point"))
}

/// The entitlement state of this install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// A valid license is installed.
    Licensed { license: Box<License> },
    /// The license verified but its term has ended.
    Expired {
        license: Box<License>,
        expired_at: i64,
    },
    /// Inside the evaluation window.
    Trial { days_left: u32, expires_at: i64 },
    /// Evaluation window is over and no license is installed.
    TrialExpired { expired_at: i64 },
    /// A license file exists but does not verify.
    Invalid { reason: String },
    /// Built from source without a verifying key. Fully functional.
    SelfBuilt,
}

impl Status {
    /// Whether the app should run its full feature set.
    pub fn is_functional(&self) -> bool {
        matches!(
            self,
            Status::Licensed { .. } | Status::Trial { .. } | Status::SelfBuilt
        )
    }

    /// Whether the UI should nag. Distinct from [`is_functional`](Self::is_functional):
    /// a trial with two days left works fine but is worth mentioning.
    pub fn needs_attention(&self) -> bool {
        match self {
            Status::Licensed { .. } | Status::SelfBuilt => false,
            Status::Trial { days_left, .. } => *days_left <= 3,
            _ => true,
        }
    }

    /// One-line summary for `pasteport status` and the about box.
    pub fn summary(&self) -> String {
        match self {
            Status::Licensed { license } => {
                let p = &license.payload;
                match p.expires_at {
                    Some(_) => format!("Licensed ({}, {})", p.plan.as_str(), p.email),
                    None => format!("Licensed ({}, perpetual)", p.plan.as_str()),
                }
            }
            Status::Expired { license, .. } => {
                format!("License expired ({})", license.payload.email)
            }
            Status::Trial { days_left, .. } => match days_left {
                0 => "Trial ends today".to_string(),
                1 => "Trial: 1 day left".to_string(),
                n => format!("Trial: {n} days left"),
            },
            Status::TrialExpired { .. } => "Trial expired; a license is required".to_string(),
            Status::Invalid { reason } => format!("License not valid: {reason}"),
            Status::SelfBuilt => "Built from source (unlicensed build)".to_string(),
        }
    }
}

/// Resolves entitlement from the license file, trial record, and build key.
#[derive(Debug)]
pub struct Licensing {
    verifying_key: Option<VerifyingKey>,
    license_path: Option<PathBuf>,
    trial_path: Option<PathBuf>,
    machine_path: Option<PathBuf>,
}

impl Licensing {
    /// Standard setup: keys and records under `data_dir`, verifying key from the
    /// build environment.
    pub fn new(data_dir: &Path) -> Self {
        let verifying_key = EMBEDDED_PUBKEY_B64.and_then(|b64| match verifying_key_from_b64(b64) {
            Ok(k) => Some(k),
            Err(e) => {
                // A broken build-time key must not silently unlock the app.
                tracing::error!(error = %e, "embedded license key is invalid");
                None
            }
        });
        Licensing {
            verifying_key,
            license_path: Some(data_dir.join("license.key")),
            trial_path: Some(data_dir.join("trial.json")),
            machine_path: Some(data_dir.join("machine-id")),
        }
    }

    /// A source build with no verifying key: always [`Status::SelfBuilt`].
    pub fn unconfigured() -> Self {
        Licensing {
            verifying_key: None,
            license_path: None,
            trial_path: None,
            machine_path: None,
        }
    }

    /// Explicit wiring, used by tests and by the minting tool.
    pub fn with_key(key: VerifyingKey, data_dir: &Path) -> Self {
        Licensing {
            verifying_key: Some(key),
            license_path: Some(data_dir.join("license.key")),
            trial_path: Some(data_dir.join("trial.json")),
            machine_path: Some(data_dir.join("machine-id")),
        }
    }

    pub fn is_licensing_enforced(&self) -> bool {
        self.verifying_key.is_some()
    }

    /// This install's opaque identifier, created on first call.
    pub fn machine_fingerprint(&self) -> Result<String> {
        match &self.machine_path {
            Some(p) => machine::fingerprint(p),
            None => Ok("unconfigured".to_string()),
        }
    }

    /// Validate and store a license key the user pasted in.
    ///
    /// Verifies before writing, so a bad key never replaces a good one.
    pub fn install_key(&self, raw: &str) -> Result<License> {
        let key = self.verifying_key.ok_or(Error::NoVerifyingKey)?;
        let license = License::verify(raw, &key)?;
        let path = self.license_path.as_ref().ok_or(Error::NoVerifyingKey)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        std::fs::write(path, &license.raw).map_err(|e| Error::io(path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| Error::io(path, e))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| Error::io(path, e))?;
        }
        Ok(license)
    }

    /// Remove the stored license. Used by "deactivate this machine".
    pub fn remove_key(&self) -> Result<()> {
        if let Some(path) = &self.license_path {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(path, e)),
            }
        }
        Ok(())
    }

    /// Current entitlement, using the system clock.
    pub fn status(&self) -> Status {
        self.status_at(now())
    }

    /// Current entitlement against an explicit clock. Tests use this; so does
    /// anything that needs a deterministic answer.
    pub fn status_at(&self, now: i64) -> Status {
        let Some(key) = self.verifying_key else {
            return Status::SelfBuilt;
        };

        // An installed license wins over the trial, expired or not: the user
        // paid, and the message they see should say so.
        if let Some(path) = &self.license_path {
            match std::fs::read_to_string(path) {
                Ok(raw) => match License::verify(&raw, &key) {
                    Ok(license) => {
                        return match license.payload.expires_at {
                            Some(exp) if now >= exp => Status::Expired {
                                license: Box::new(license),
                                expired_at: exp,
                            },
                            _ => Status::Licensed {
                                license: Box::new(license),
                            },
                        };
                    }
                    Err(e) => {
                        return Status::Invalid {
                            reason: e.to_string(),
                        }
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Status::Invalid {
                        reason: e.to_string(),
                    }
                }
            }
        }

        // No license: fall back to the trial clock.
        let Some(trial_path) = &self.trial_path else {
            return Status::TrialExpired { expired_at: now };
        };
        let machine = self
            .machine_fingerprint()
            .unwrap_or_else(|_| "unknown".to_string());
        match Trial::load_or_start(trial_path, &machine, now) {
            Ok(trial) if trial.is_active_at(now) => Status::Trial {
                days_left: trial.days_left_at(now),
                expires_at: trial.expires_at(),
            },
            Ok(trial) => Status::TrialExpired {
                expired_at: trial.expires_at(),
            },
            Err(e) => Status::Invalid {
                reason: e.to_string(),
            },
        }
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(all(test, feature = "mint"))]
mod tests {
    use super::*;
    use crate::mint::Minter;

    const DAY: i64 = 86_400;

    fn setup() -> (tempfile::TempDir, Minter, Licensing) {
        let dir = tempfile::tempdir().unwrap();
        let minter = Minter::from_seed(&[42u8; 32]);
        let licensing = Licensing::with_key(minter.verifying_key(), dir.path());
        (dir, minter, licensing)
    }

    fn payload(expires_at: Option<i64>) -> LicensePayload {
        LicensePayload {
            v: 1,
            id: "lic_status".into(),
            email: "buyer@example.com".into(),
            plan: if expires_at.is_some() {
                Plan::Personal
            } else {
                Plan::Lifetime
            },
            seats: 1,
            issued_at: 0,
            expires_at,
        }
    }

    #[test]
    fn fresh_install_starts_a_trial() {
        let (_d, _m, licensing) = setup();
        let status = licensing.status_at(0);
        assert!(
            matches!(status, Status::Trial { days_left, .. } if days_left == TRIAL_DAYS as u32)
        );
        assert!(status.is_functional());
        assert!(!status.needs_attention());
    }

    #[test]
    fn trial_warns_near_the_end_then_expires() {
        let (_d, _m, licensing) = setup();
        licensing.status_at(0); // start the clock at t=0

        let late = licensing.status_at(12 * DAY);
        assert!(late.is_functional());
        assert!(late.needs_attention(), "two days left should prompt");

        let over = licensing.status_at(TRIAL_DAYS * DAY + 1);
        assert!(matches!(over, Status::TrialExpired { .. }));
        assert!(!over.is_functional());
        assert!(over.needs_attention());
    }

    #[test]
    fn a_valid_license_supersedes_the_trial() {
        let (_d, minter, licensing) = setup();
        licensing.status_at(0);
        assert!(matches!(licensing.status_at(0), Status::Trial { .. }));

        let key = minter.sign(&payload(None)).unwrap();
        licensing.install_key(&key).unwrap();

        let status = licensing.status_at(TRIAL_DAYS * DAY + 1);
        assert!(matches!(status, Status::Licensed { .. }));
        assert!(status.is_functional());
        assert!(!status.needs_attention());
        assert!(status.summary().contains("perpetual"));
    }

    #[test]
    fn a_perpetual_license_never_expires() {
        let (_d, minter, licensing) = setup();
        licensing
            .install_key(&minter.sign(&payload(None)).unwrap())
            .unwrap();
        assert!(matches!(
            licensing.status_at(i64::MAX / 2),
            Status::Licensed { .. }
        ));
    }

    #[test]
    fn a_subscription_license_expires_but_says_who_it_was() {
        let (_d, minter, licensing) = setup();
        let expires = 100 * DAY;
        licensing
            .install_key(&minter.sign(&payload(Some(expires))).unwrap())
            .unwrap();

        assert!(matches!(
            licensing.status_at(expires - 1),
            Status::Licensed { .. }
        ));

        let status = licensing.status_at(expires);
        assert!(matches!(status, Status::Expired { .. }));
        assert!(!status.is_functional());
        assert!(status.summary().contains("buyer@example.com"));
    }

    #[test]
    fn a_forged_license_is_rejected_not_accepted() {
        let (dir, _m, licensing) = setup();
        let attacker = Minter::from_seed(&[1u8; 32]);
        let forged = attacker.sign(&payload(None)).unwrap();

        // Written directly, bypassing install_key's validation.
        std::fs::write(dir.path().join("license.key"), &forged).unwrap();

        let status = licensing.status_at(0);
        assert!(matches!(status, Status::Invalid { .. }));
        assert!(!status.is_functional());
    }

    #[test]
    fn install_key_refuses_a_bad_key_and_keeps_the_old_one() {
        let (_d, minter, licensing) = setup();
        licensing
            .install_key(&minter.sign(&payload(None)).unwrap())
            .unwrap();

        let attacker = Minter::from_seed(&[2u8; 32]);
        let bad = attacker.sign(&payload(None)).unwrap();
        assert!(licensing.install_key(&bad).is_err());

        // The good license is still in place.
        assert!(matches!(licensing.status_at(0), Status::Licensed { .. }));
    }

    #[test]
    fn removing_a_key_falls_back_to_the_trial() {
        let (_d, minter, licensing) = setup();
        licensing
            .install_key(&minter.sign(&payload(None)).unwrap())
            .unwrap();
        assert!(matches!(licensing.status_at(0), Status::Licensed { .. }));

        licensing.remove_key().unwrap();
        assert!(matches!(licensing.status_at(0), Status::Trial { .. }));
        // Removing twice is not an error.
        licensing.remove_key().unwrap();
    }

    #[test]
    fn license_file_is_owner_only() {
        let (dir, minter, licensing) = setup();
        licensing
            .install_key(&minter.sign(&payload(None)).unwrap())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join("license.key");
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn source_builds_are_fully_functional() {
        let licensing = Licensing::unconfigured();
        assert!(!licensing.is_licensing_enforced());
        let status = licensing.status_at(0);
        assert_eq!(status, Status::SelfBuilt);
        assert!(status.is_functional());
        assert!(!status.needs_attention());
    }

    #[test]
    fn unconfigured_build_cannot_install_a_key() {
        let licensing = Licensing::unconfigured();
        assert!(matches!(
            licensing.install_key("PP1.a.b"),
            Err(Error::NoVerifyingKey)
        ));
    }

    #[test]
    fn summaries_are_human_readable() {
        let (_d, minter, licensing) = setup();
        licensing.status_at(0);
        assert!(licensing
            .status_at(13 * DAY)
            .summary()
            .contains("1 day left"));

        licensing
            .install_key(&minter.sign(&payload(Some(100 * DAY))).unwrap())
            .unwrap();
        assert!(licensing
            .status_at(0)
            .summary()
            .starts_with("Licensed (personal"));
    }
}
