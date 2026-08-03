//! License signing. Behind the `mint` feature so that no shipped binary
//! contains the ability to sign, only to verify.

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};

use crate::error::{Error, Result};
use crate::license::{encode_key, LicensePayload};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Holds the private half of the license signing key.
///
/// This lives on the vendor's machine (or a signing service), never in a
/// distributed build.
#[derive(Debug)]
pub struct Minter {
    signing_key: SigningKey,
}

impl Minter {
    /// Build a minter from a 32-byte seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Minter {
            signing_key: SigningKey::from_bytes(seed),
        }
    }

    /// Build a minter from a base64url-encoded 32-byte seed.
    pub fn from_seed_b64(seed_b64: &str) -> Result<Self> {
        let bytes = B64
            .decode(seed_b64.trim())
            .map_err(|_| Error::InvalidVerifyingKey("seed is not base64url"))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::InvalidVerifyingKey("seed is not 32 bytes"))?;
        Ok(Minter::from_seed(&seed))
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// The public key to compile into release builds, base64url encoded.
    pub fn verifying_key_b64(&self) -> String {
        B64.encode(self.verifying_key().to_bytes())
    }

    /// The private seed, base64url encoded. Treat as a secret.
    pub fn seed_b64(&self) -> String {
        B64.encode(self.signing_key.to_bytes())
    }

    /// Sign a payload into a distributable license key.
    pub fn sign(&self, payload: &LicensePayload) -> Result<String> {
        let bytes = payload.signing_bytes()?;
        let sig = self.signing_key.sign(&bytes);
        Ok(encode_key(&bytes, &sig.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::{License, Plan};

    #[test]
    fn seed_round_trips_through_base64() {
        let m = Minter::from_seed(&[3u8; 32]);
        let again = Minter::from_seed_b64(&m.seed_b64()).unwrap();
        assert_eq!(m.verifying_key_b64(), again.verifying_key_b64());
    }

    #[test]
    fn rejects_a_bad_seed() {
        assert!(Minter::from_seed_b64("not base64!").is_err());
        assert!(
            Minter::from_seed_b64("aGVsbG8").is_err(),
            "wrong length must be rejected"
        );
    }

    #[test]
    fn signed_keys_verify_against_the_published_public_key() {
        let m = Minter::from_seed(&[11u8; 32]);
        let payload = LicensePayload {
            v: 1,
            id: "lic_mint_test".into(),
            email: "a@b.co".into(),
            plan: Plan::Team,
            seats: 5,
            issued_at: 1_700_000_000,
            expires_at: Some(1_800_000_000),
        };
        let key = m.sign(&payload).unwrap();

        let published = crate::verifying_key_from_b64(&m.verifying_key_b64()).unwrap();
        let license = License::verify(&key, &published).unwrap();
        assert_eq!(license.payload.seats, 5);
    }
}
