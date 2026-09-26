//! Tunnel key material: the backend's Noise static X25519 keypair and the
//! relay pre-shared key.
//!
//! Both are 32 bytes and appear in the relay link fragment (the public key
//! and the PSK); the static secret never leaves the backend. Secret bytes are
//! zeroized on drop and never printed by `Debug`. Persisting them
//! (`identity.json`) is the host connector's job; this module only generates
//! and wraps raw bytes.

use std::fmt;

use rand_core::{OsRng, TryRngCore};
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use zeroize::{Zeroize, Zeroizing};

/// Length of every tunnel key (X25519 keys and the PSK).
pub const KEY_LEN: usize = 32;

/// Failure generating key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The operating system random source failed.
    #[error("system random source failed: {0}")]
    Random(String),
    /// The X25519 implementation is unavailable (build misconfiguration).
    #[error("x25519 is unavailable in this build")]
    Unavailable,
}

/// The backend's Noise static X25519 keypair (the Noise `s` of the
/// responder). The public half goes into relay links.
pub struct StaticKeypair {
    secret: Zeroizing<[u8; KEY_LEN]>,
    public: [u8; KEY_LEN],
}

impl StaticKeypair {
    /// Generate a fresh keypair from the system random source.
    ///
    /// # Errors
    /// [`KeyError::Random`] when the random source fails.
    pub fn generate() -> Result<Self, KeyError> {
        let mut secret = Zeroizing::new([0u8; KEY_LEN]);
        OsRng
            .try_fill_bytes(secret.as_mut())
            .map_err(|error| KeyError::Random(error.to_string()))?;
        Self::from_secret(*secret)
    }

    /// Rebuild a keypair from stored secret bytes, deriving the public key.
    ///
    /// Any 32 bytes are a valid X25519 secret (it is clamped on use).
    ///
    /// # Errors
    /// [`KeyError::Unavailable`] if the build lacks X25519 (never with the
    /// workspace feature set).
    pub fn from_secret(mut secret: [u8; KEY_LEN]) -> Result<Self, KeyError> {
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .ok_or(KeyError::Unavailable)?;
        dh.set(&secret);
        let mut public = [0u8; KEY_LEN];
        let derived = dh.pubkey();
        if derived.len() != KEY_LEN {
            secret.zeroize();
            return Err(KeyError::Unavailable);
        }
        public.copy_from_slice(derived);
        let pair = StaticKeypair {
            secret: Zeroizing::new(secret),
            public,
        };
        secret.zeroize();
        Ok(pair)
    }

    /// The secret key bytes (store them with owner-only permissions).
    #[must_use]
    pub fn secret(&self) -> &[u8; KEY_LEN] {
        &self.secret
    }

    /// The public key bytes (the server key in a relay link).
    #[must_use]
    pub fn public(&self) -> &[u8; KEY_LEN] {
        &self.public
    }
}

impl Clone for StaticKeypair {
    fn clone(&self) -> Self {
        StaticKeypair {
            secret: self.secret.clone(),
            public: self.public,
        }
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticKeypair")
            .field("secret", &"<redacted>")
            .field("public", &self.public)
            .finish()
    }
}

/// The relay pre-shared key (Noise `psk0`). Whoever holds it (through the
/// link) may open tunnels to the backend.
#[derive(Clone)]
pub struct Psk(Zeroizing<[u8; KEY_LEN]>);

impl Psk {
    /// Generate a fresh PSK from the system random source.
    ///
    /// # Errors
    /// [`KeyError::Random`] when the random source fails.
    pub fn generate() -> Result<Self, KeyError> {
        let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
        OsRng
            .try_fill_bytes(bytes.as_mut())
            .map_err(|error| KeyError::Random(error.to_string()))?;
        Ok(Psk(bytes))
    }

    /// Wrap existing PSK bytes (from a link or stored identity).
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Psk(Zeroizing::new(bytes))
    }

    /// The PSK bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}
