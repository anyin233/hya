//! PKCE (RFC 7636) helpers for authorization-code OAuth.

use super::OAuthError;
use base64::Engine as _;
use rand_core::{OsRng, TryRngCore as _};
use sha2::{Digest, Sha256};

/// Generate a high-entropy `code_verifier` and its S256 `code_challenge`.
pub fn generate_pkce_pair() -> Result<(String, String), OAuthError> {
    let mut bytes = [0u8; 64];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| OAuthError::Protocol(format!("PKCE entropy: {e}")))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Ok((verifier, challenge))
}

/// Generate a random URL-safe state string for CSRF protection.
pub fn generate_state() -> Result<String, OAuthError> {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| OAuthError::Protocol(format!("OAuth state entropy: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let (verifier, challenge) = generate_pkce_pair().unwrap();
        assert!(verifier.len() >= 43);
        let digest = Sha256::digest(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(challenge, expected);
        assert_ne!(generate_state().unwrap(), generate_state().unwrap());
    }
}
