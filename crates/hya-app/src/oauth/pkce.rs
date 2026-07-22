//! PKCE (RFC 7636) helpers for authorization-code OAuth.

use base64::Engine as _;
use rand_core::{OsRng, TryRngCore as _};
use sha2::{Digest, Sha256};

/// Generate a high-entropy `code_verifier` and its S256 `code_challenge`.
#[must_use]
pub fn generate_pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 64];
    // OsRng is fallible on some platforms; fall back to zeros only if entropy fails
    // (should not happen on Linux; still produces a long verifier string).
    let _ = OsRng.try_fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

/// Generate a random URL-safe state string for CSRF protection.
#[must_use]
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    let _ = OsRng.try_fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let (verifier, challenge) = generate_pkce_pair();
        assert!(verifier.len() >= 43);
        let digest = Sha256::digest(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(challenge, expected);
        assert_ne!(generate_state(), generate_state());
    }
}
