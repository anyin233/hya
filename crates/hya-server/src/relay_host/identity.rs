//! The backend's relay identity (ADR-0025 D3): the Ed25519 room key, the
//! Noise static X25519 key, and the pre-shared key of the link.
//!
//! A backend on a file database keeps it in `<db>.relay-identity.json`
//! (mode 0600, next to `<db>.lock`), so its link survives restarts:
//!
//! ```json
//! {"version":1,"ed25519":"<b64url>","x25519":"<b64url>","psk":"<b64url>"}
//! ```
//!
//! Each value is a 32-byte secret, unpadded base64url. Only the lock holder
//! of the database writes the file (atomically: a 0600 temporary file, then
//! rename). An in-memory database, or `--relay-ephemeral`, uses a throwaway
//! identity that is never written anywhere.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::SigningKey;
use hya_relay::keys::{OpenToken, Psk, StaticKeypair};
use hya_relay::link::RoomId;
use rand_core::{OsRng, TryRngCore as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Version of the identity file format.
const VERSION: u32 = 1;

/// The relay identity of one backend.
#[derive(Clone)]
pub struct RelayIdentity {
    signing: SigningKey,
    noise: StaticKeypair,
    psk: Psk,
}

impl std::fmt::Debug for RelayIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayIdentity")
            .field("room_id", &self.room_id().as_str())
            .finish_non_exhaustive()
    }
}

/// A failure loading, creating, or saving an identity. Messages name the
/// file, never key material.
#[derive(Debug)]
pub enum IdentityError {
    /// Reading or writing the file failed.
    Io(String),
    /// The file is not a valid identity.
    Invalid(String),
    /// The OS random source failed.
    Random(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::Io(message)
            | IdentityError::Invalid(message)
            | IdentityError::Random(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for IdentityError {}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    version: u32,
    ed25519: String,
    x25519: String,
    psk: String,
}

impl RelayIdentity {
    /// A fresh random identity.
    ///
    /// # Errors
    /// The OS random source failed.
    pub fn generate() -> Result<Self, IdentityError> {
        let signing = SigningKey::from_bytes(&*random32()?);
        let noise = StaticKeypair::from_secret(*random32()?)
            .map_err(|error| IdentityError::Random(error.to_string()))?;
        let psk = Psk::from_bytes(*random32()?);
        Ok(Self {
            signing,
            noise,
            psk,
        })
    }

    /// The room this identity owns: `base32(sha256(ed25519_pub))[..26]`.
    #[must_use]
    pub fn room_id(&self) -> RoomId {
        RoomId::from_ed25519(self.signing.verifying_key().as_bytes())
    }

    /// The Ed25519 key that signs room registrations.
    #[must_use]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// The Noise static keypair.
    #[must_use]
    pub fn noise(&self) -> &StaticKeypair {
        &self.noise
    }

    /// The link's pre-shared key.
    #[must_use]
    pub fn psk(&self) -> &Psk {
        &self.psk
    }

    /// `sha256` of the room's open token (derived from the PSK): what the
    /// host registers with the proxy.
    #[must_use]
    pub fn open_token_hash(&self) -> [u8; 32] {
        OpenToken::derive(&self.psk, &self.room_id()).hash()
    }

    /// The same room and Noise keys with a new random PSK.
    ///
    /// # Errors
    /// The OS random source failed.
    pub fn rotated(&self) -> Result<Self, IdentityError> {
        Ok(Self {
            signing: self.signing.clone(),
            noise: self.noise.clone(),
            psk: Psk::from_bytes(*random32()?),
        })
    }

    /// Load the identity at `path`, creating (and saving) a new one when the
    /// file does not exist.
    ///
    /// # Errors
    /// The file is unreadable, malformed, readable by group or others, or
    /// cannot be created.
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        match Self::load(path)? {
            Some(identity) => Ok(identity),
            None => {
                let identity = Self::generate()?;
                identity.save(path)?;
                Ok(identity)
            }
        }
    }

    /// Load the identity at `path`; `None` when there is no file.
    ///
    /// The file is opened without following a final symbolic link
    /// (`O_NOFOLLOW`), and the checks run on the opened descriptor (`fstat`),
    /// so the file read is the file checked: it must be a regular file owned
    /// by the effective user, with no group or other permission bits.
    /// Extended ACLs (macOS `chmod +a`, POSIX ACLs) are not inspected; keep
    /// the database directory private.
    ///
    /// # Errors
    /// As [`RelayIdentity::load_or_create`].
    pub fn load(path: &Path) -> Result<Option<Self>, IdentityError> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            // A FIFO must not block the open; it is refused below anyway.
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path);
        let mut file = match file {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(IdentityError::Invalid(format!(
                    "{} is a symbolic link; the relay identity must be a regular file",
                    path.display()
                )));
            }
            Err(error) => return Err(io_error(path, &error)),
        };
        let metadata = file.metadata().map_err(|error| io_error(path, &error))?;
        check_file(path, &FileFacts::of(&metadata), effective_uid())?;
        let mut text = Zeroizing::new(Vec::new());
        std::io::Read::read_to_end(&mut file, &mut text).map_err(|error| io_error(path, &error))?;
        let file: IdentityFile = serde_json::from_slice(&text).map_err(|_| {
            IdentityError::Invalid(format!("{} is not a relay identity file", path.display()))
        })?;
        if file.version != VERSION {
            return Err(IdentityError::Invalid(format!(
                "{} has unsupported version {} (expected {VERSION})",
                path.display(),
                file.version
            )));
        }
        let key = |field: &str, value: &str| -> Result<Zeroizing<[u8; 32]>, IdentityError> {
            let bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(value).map_err(|_| {
                IdentityError::Invalid(format!("{}: `{field}` is not base64url", path.display()))
            })?);
            let array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                IdentityError::Invalid(format!("{}: `{field}` is not 32 bytes", path.display()))
            })?;
            Ok(Zeroizing::new(array))
        };
        let signing = SigningKey::from_bytes(&*key("ed25519", &file.ed25519)?);
        let noise = StaticKeypair::from_secret(*key("x25519", &file.x25519)?)
            .map_err(|error| IdentityError::Invalid(error.to_string()))?;
        let psk = Psk::from_bytes(*key("psk", &file.psk)?);
        Ok(Some(Self {
            signing,
            noise,
            psk,
        }))
    }

    /// Write the identity to `path` (mode 0600), atomically.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), IdentityError> {
        let file = IdentityFile {
            version: VERSION,
            ed25519: URL_SAFE_NO_PAD.encode(self.signing.to_bytes()),
            x25519: URL_SAFE_NO_PAD.encode(self.noise.secret()),
            psk: URL_SAFE_NO_PAD.encode(self.psk.as_bytes()),
        };
        let body = Zeroizing::new(
            serde_json::to_vec(&file).map_err(|error| IdentityError::Io(error.to_string()))?,
        );
        let temp = temp_path(path);
        let _ = std::fs::remove_file(&temp);
        let write = || -> std::io::Result<()> {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            out.write_all(&body)?;
            out.sync_all()?;
            std::fs::rename(&temp, path)
        };
        write().map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            io_error(path, &error)
        })
    }
}

/// What [`RelayIdentity::load`] checks about the opened file.
#[derive(Clone, Copy, Debug)]
struct FileFacts {
    regular: bool,
    uid: u32,
    mode: u32,
}

impl FileFacts {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            regular: metadata.is_file(),
            uid: metadata.uid(),
            mode: metadata.mode(),
        }
    }
}

/// A regular file owned by `euid` with no group or other permission bits.
fn check_file(path: &Path, facts: &FileFacts, euid: u32) -> Result<(), IdentityError> {
    if !facts.regular {
        return Err(IdentityError::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if facts.uid != euid {
        return Err(IdentityError::Invalid(format!(
            "{} is owned by uid {}, not by this user (uid {euid}); it holds the relay link's secret keys",
            path.display(),
            facts.uid
        )));
    }
    if facts.mode & 0o077 != 0 {
        return Err(IdentityError::Invalid(format!(
            "{} is accessible by other users; it holds the relay link's secret keys (run `chmod 600 {}`)",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no
    // memory owned by Rust.
    unsafe { libc::geteuid() }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(temp)
}

fn io_error(path: &Path, error: &std::io::Error) -> IdentityError {
    IdentityError::Io(format!("{}: {error}", path.display()))
}

fn random32() -> Result<Zeroizing<[u8; 32]>, IdentityError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    OsRng
        .try_fill_bytes(bytes.as_mut())
        .map_err(|error| IdentityError::Random(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hya-relay-identity-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_identity_is_created_once_with_mode_0600_and_loaded_back() {
        let dir = scratch("create");
        let path = dir.join("sessions.db.relay-identity.json");
        let first = RelayIdentity::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let again = RelayIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.room_id(), again.room_id());
        assert_eq!(first.noise().public(), again.noise().public());
        assert_eq!(first.psk().as_bytes(), again.psk().as_bytes());
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rotation_keeps_the_room_and_noise_key_and_changes_the_psk() {
        let identity = RelayIdentity::generate().unwrap();
        let rotated = identity.rotated().unwrap();
        assert_eq!(identity.room_id(), rotated.room_id());
        assert_eq!(identity.noise().public(), rotated.noise().public());
        assert_ne!(identity.psk().as_bytes(), rotated.psk().as_bytes());
    }

    #[test]
    fn a_file_accessible_by_others_or_malformed_is_refused() {
        let dir = scratch("refuse");
        let path = dir.join("id.json");
        RelayIdentity::generate().unwrap().save(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = RelayIdentity::load(&path).unwrap_err().to_string();
        assert!(error.contains("chmod 600"), "{error}");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let error = RelayIdentity::load(&path).unwrap_err().to_string();
        assert!(error.contains("not a relay identity"), "{error}");
        assert!(
            RelayIdentity::load(&dir.join("missing.json"))
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_symlinked_identity_file_is_refused() {
        let dir = scratch("symlink");
        let real = dir.join("real.json");
        RelayIdentity::generate().unwrap().save(&real).unwrap();
        let link = dir.join("sessions.db.relay-identity.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = RelayIdentity::load(&link).unwrap_err().to_string();
        assert!(error.contains("symbolic link"), "{error}");
        // `load_or_create` must not replace it with a fresh identity either.
        assert!(RelayIdentity::load_or_create(&link).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_non_regular_identity_path_is_refused() {
        let dir = scratch("dir");
        let error = RelayIdentity::load(&dir).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_an_owner_only_file_of_this_user_is_accepted() {
        let path = Path::new("/x/id.json");
        let euid = 501;
        let ok = FileFacts {
            regular: true,
            uid: euid,
            mode: 0o100_600,
        };
        assert!(check_file(path, &ok, euid).is_ok());
        let foreign = FileFacts { uid: 0, ..ok };
        let error = check_file(path, &foreign, euid).unwrap_err().to_string();
        assert!(error.contains("owned by uid 0"), "{error}");
        for mode in [0o100_640, 0o100_604, 0o100_660, 0o100_700 | 0o010] {
            let open = FileFacts { mode, ..ok };
            let error = check_file(path, &open, euid).unwrap_err().to_string();
            assert!(error.contains("chmod 600"), "{mode:o}: {error}");
        }
        let fifo = FileFacts {
            regular: false,
            ..ok
        };
        assert!(check_file(path, &fifo, euid).is_err());
    }

    #[test]
    fn debug_never_prints_secrets() {
        let identity = RelayIdentity::generate().unwrap();
        let debug = format!("{identity:?}");
        assert!(debug.contains(identity.room_id().as_str()));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode(identity.psk().as_bytes())));
    }
}
