use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Key format prefix, so a future signing scheme can coexist with this one.
pub const KEY_PREFIX: &str = "PP1";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// What the customer bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    /// One person, all their machines.
    Personal,
    /// Seat-based, for organizations.
    Team,
    /// Perpetual: never expires.
    Lifetime,
}

impl Plan {
    pub fn as_str(self) -> &'static str {
        match self {
            Plan::Personal => "personal",
            Plan::Team => "team",
            Plan::Lifetime => "lifetime",
        }
    }
}

/// The signed contents of a license key.
///
/// Field names are short because they travel inside the key the customer pastes
/// in, and every byte shows up in that string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensePayload {
    /// Payload format version.
    pub v: u32,
    /// Opaque license id, for support lookups.
    pub id: String,
    /// Who it was issued to.
    pub email: String,
    pub plan: Plan,
    /// Seats for [`Plan::Team`], otherwise 1.
    #[serde(default = "one")]
    pub seats: u32,
    /// Unix seconds.
    pub issued_at: i64,
    /// Unix seconds, or `None` for a perpetual license.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

fn one() -> u32 {
    1
}

impl LicensePayload {
    pub fn is_expired_at(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|exp| now >= exp)
    }

    /// Canonical bytes that get signed. Must be byte-stable across versions,
    /// so it is plain `serde_json` over a struct with fixed field order.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(Error::Encode)
    }
}

/// A license key that has been parsed and cryptographically verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    pub payload: LicensePayload,
    /// The key string as the customer entered it, for display and re-saving.
    pub raw: String,
}

impl License {
    /// Parse and verify a license key against `key`.
    ///
    /// Signature verification happens before anything else is trusted: an
    /// unsigned or tampered key never reaches policy checks.
    pub fn verify(raw: &str, key: &VerifyingKey) -> Result<License> {
        let raw = normalize(raw);
        let mut parts = raw.split('.');
        let prefix = parts.next().unwrap_or_default();
        let payload_b64 = parts
            .next()
            .ok_or(Error::Malformed("missing payload segment"))?;
        let sig_b64 = parts
            .next()
            .ok_or(Error::Malformed("missing signature segment"))?;
        if parts.next().is_some() {
            return Err(Error::Malformed("too many segments"));
        }
        if prefix != KEY_PREFIX {
            return Err(Error::UnknownFormat(prefix.to_string()));
        }

        let payload_bytes = B64
            .decode(payload_b64)
            .map_err(|_| Error::Malformed("payload is not base64url"))?;
        let sig_bytes = B64
            .decode(sig_b64)
            .map_err(|_| Error::Malformed("signature is not base64url"))?;
        let sig_bytes: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| Error::Malformed("signature is not 64 bytes"))?;

        key.verify(&payload_bytes, &Signature::from_bytes(&sig_bytes))
            .map_err(|_| Error::BadSignature)?;

        let payload: LicensePayload =
            serde_json::from_slice(&payload_bytes).map_err(Error::Decode)?;
        if payload.v != 1 {
            return Err(Error::UnsupportedPayloadVersion(payload.v));
        }

        Ok(License { payload, raw })
    }

    /// Middle of the key, safe to show in a UI or a log.
    pub fn masked(&self) -> String {
        let id = &self.payload.id;
        let shown: String = id.chars().take(6).collect();
        format!("{shown}…")
    }
}

/// Accept keys pasted with the whitespace and line breaks an email adds.
fn normalize(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Encode a signed key from its parts.
///
/// Only the minting side ever assembles a key; verification takes one apart. So
/// this is gated on the `mint` feature, which also keeps a default-feature
/// release build free of dead code.
#[cfg(feature = "mint")]
pub fn encode_key(payload_bytes: &[u8], signature: &[u8; 64]) -> String {
    format!(
        "{KEY_PREFIX}.{}.{}",
        B64.encode(payload_bytes),
        B64.encode(signature)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mint::Minter;

    fn minter() -> Minter {
        Minter::from_seed(&[7u8; 32])
    }

    fn payload() -> LicensePayload {
        LicensePayload {
            v: 1,
            id: "lic_abcdef123".into(),
            email: "buyer@example.com".into(),
            plan: Plan::Personal,
            seats: 1,
            issued_at: 1_700_000_000,
            expires_at: None,
        }
    }

    #[test]
    fn verifies_a_well_formed_key() {
        let m = minter();
        let key = m.sign(&payload()).unwrap();
        let license = License::verify(&key, &m.verifying_key()).unwrap();
        assert_eq!(license.payload, payload());
    }

    #[test]
    fn tolerates_pasted_whitespace() {
        let m = minter();
        let key = m.sign(&payload()).unwrap();
        let mangled = format!("  {}\n  ", key.replace('.', ".\n"));
        assert!(License::verify(&mangled, &m.verifying_key()).is_ok());
    }

    #[test]
    fn rejects_a_tampered_payload() {
        let m = minter();
        let key = m.sign(&payload()).unwrap();

        // Re-encode a payload that says "lifetime" while keeping the signature.
        let mut parts: Vec<&str> = key.split('.').collect();
        let mut evil = payload();
        evil.plan = Plan::Lifetime;
        let forged = B64.encode(evil.signing_bytes().unwrap());
        parts[1] = &forged;
        let forged_key = parts.join(".");

        assert!(matches!(
            License::verify(&forged_key, &m.verifying_key()),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn rejects_a_key_from_another_signer() {
        let real = minter();
        let attacker = Minter::from_seed(&[9u8; 32]);
        let key = attacker.sign(&payload()).unwrap();
        assert!(matches!(
            License::verify(&key, &real.verifying_key()),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn rejects_malformed_keys() {
        let m = minter();
        let vk = m.verifying_key();
        for bad in [
            "",
            "PP1",
            "PP1.only-two",
            "PP1.a.b.c",
            "PP2.abc.def",
            "PP1.!!!notbase64!!!.abc",
        ] {
            assert!(
                License::verify(bad, &vk).is_err(),
                "{bad:?} must not verify"
            );
        }
    }

    #[test]
    fn rejects_a_short_signature() {
        let m = minter();
        let key = m.sign(&payload()).unwrap();
        let truncated = {
            let parts: Vec<&str> = key.split('.').collect();
            format!("{}.{}.{}", parts[0], parts[1], &parts[2][..20])
        };
        assert!(matches!(
            License::verify(&truncated, &m.verifying_key()),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn expiry_is_evaluated_against_a_supplied_clock() {
        let perpetual = payload();
        assert!(!perpetual.is_expired_at(i64::MAX - 1));

        let sub = LicensePayload {
            expires_at: Some(2_000),
            ..payload()
        };
        assert!(!sub.is_expired_at(1_999));
        assert!(sub.is_expired_at(2_000));
        assert!(sub.is_expired_at(2_001));
    }

    #[test]
    fn masked_id_does_not_leak_the_whole_key() {
        let m = minter();
        let key = m.sign(&payload()).unwrap();
        let license = License::verify(&key, &m.verifying_key()).unwrap();
        let masked = license.masked();
        assert!(masked.len() < license.payload.id.len() + 3);
        assert!(!masked.contains("123"));
    }
}
