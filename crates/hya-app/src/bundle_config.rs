//! Per-bundle configuration location: one `config.yml` for every bundle.
//!
//! A bundle's user-owned configuration file lives beside the bundle's scope:
//!
//! * user installs and first-party bundles use
//!   `<hya config dir>/bundles/<percent-encoded-bundle-id>/config.yml`, where
//!   the Hya config dir holds the active `config.yaml`;
//! * project bundles (`hya bundle install --project`) use their own source
//!   directory, `.hya/bundles/<dir>/config.yml`.
//!
//! The same file holds per-Agent model defaults (`agents.<id>.model`) and any
//! keys the bundle's own code reads. Executable bundle code finds it through
//! [`HYA_BUNDLE_CONFIG_DIR`] and [`HYA_BUNDLE_CONFIG_FILE`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use sha2::{Digest as _, Sha256};

/// Environment variable naming the absolute bundle configuration directory.
pub const HYA_BUNDLE_CONFIG_DIR: &str = "HYA_BUNDLE_CONFIG_DIR";
/// Environment variable naming the absolute bundle configuration file.
pub const HYA_BUNDLE_CONFIG_FILE: &str = "HYA_BUNDLE_CONFIG_FILE";
/// File name of every bundle configuration file.
pub const BUNDLE_CONFIG_FILE_NAME: &str = "config.yml";
/// Directory beside the Hya `config.yaml` holding user-scope bundle configs.
pub const USER_BUNDLE_CONFIG_DIR_NAME: &str = "bundles";

/// Where one bundle's configuration is resolved from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleConfigScope<'a> {
    /// User/global installs and first-party bundles.
    User,
    /// A project bundle rooted at its source directory under `.hya/bundles`.
    Project(&'a Path),
}

/// Absolute configuration directory and file for one bundle.
///
/// The file need not exist; absence means an empty configuration.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct BundleConfigLocation {
    dir: PathBuf,
    file: PathBuf,
}

impl BundleConfigLocation {
    /// Absolute configuration directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Absolute `config.yml` path inside [`Self::dir`].
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// `HYA_BUNDLE_CONFIG_DIR` / `HYA_BUNDLE_CONFIG_FILE` for executable code.
    #[must_use]
    pub fn env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                HYA_BUNDLE_CONFIG_DIR.to_string(),
                self.dir.to_string_lossy().into_owned(),
            ),
            (
                HYA_BUNDLE_CONFIG_FILE.to_string(),
                self.file.to_string_lossy().into_owned(),
            ),
        ])
    }

    /// SHA-256 of the current file content, or `None` when it is absent or
    /// unreadable. Runtime sources fold this into their fingerprint.
    #[must_use]
    pub fn content_digest(&self) -> Option<[u8; 32]> {
        std::fs::read(&self.file)
            .ok()
            .map(|bytes| Sha256::digest(bytes).into())
    }
}

/// Resolve one bundle's configuration location.
///
/// `config_file` is the active Hya `config.yaml` path (it need not exist); its
/// parent is the Hya config dir. Relative inputs are made absolute against the
/// current directory without touching the filesystem.
///
/// # Errors
///
/// Returns an error when the bundle id is empty or a path cannot be made
/// absolute.
pub fn bundle_config_location(
    config_file: &Path,
    bundle_id: &str,
    scope: BundleConfigScope<'_>,
) -> anyhow::Result<BundleConfigLocation> {
    let dir = match scope {
        BundleConfigScope::User => {
            user_bundle_config_root(config_file).join(encode_bundle_leaf(bundle_id)?)
        }
        BundleConfigScope::Project(dir) => {
            if bundle_id.is_empty() {
                bail!("bundle identity must not be empty");
            }
            dir.to_path_buf()
        }
    };
    let dir = std::path::absolute(&dir)
        .with_context(|| format!("resolve bundle config directory {}", dir.display()))?;
    let file = dir.join(BUNDLE_CONFIG_FILE_NAME);
    Ok(BundleConfigLocation { dir, file })
}

/// `<hya config dir>/bundles`: parent of every user-scope bundle config dir.
#[must_use]
pub fn user_bundle_config_root(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(USER_BUNDLE_CONFIG_DIR_NAME)
}

/// Snapshot resolver: which bundle ids are project-scoped, and where.
///
/// A project bundle shadows a user install of the same id, so an id present
/// in the project map resolves to its project source directory and every
/// other id resolves to the user scope.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BundleConfigResolver {
    config_file: PathBuf,
    project: BTreeMap<String, PathBuf>,
}

impl BundleConfigResolver {
    /// Resolver over an explicit project map (`bundle id -> source dir`).
    #[must_use]
    pub fn new(config_file: PathBuf, project: BTreeMap<String, PathBuf>) -> Self {
        Self {
            config_file,
            project,
        }
    }

    /// Resolver that scans the valid project bundles under `project_dir`.
    #[must_use]
    pub fn discover(config_file: PathBuf, project_dir: Option<&Path>) -> Self {
        let mut project = BTreeMap::new();
        if let Some(dir) = project_dir {
            for bundle in crate::project_bundles::project_bundles(dir) {
                project
                    .entry(bundle.bundle_id().to_string())
                    .or_insert_with(|| bundle.dir().to_path_buf());
            }
        }
        Self::new(config_file, project)
    }

    /// Active Hya `config.yaml` path this resolver is rooted at.
    #[must_use]
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// Project-scoped bundle ids and their source directories.
    #[must_use]
    pub fn project_dirs(&self) -> &BTreeMap<String, PathBuf> {
        &self.project
    }

    /// Resolve `bundle_id` in whichever scope currently provides it.
    ///
    /// # Errors
    ///
    /// See [`bundle_config_location`].
    pub fn location(&self, bundle_id: &str) -> anyhow::Result<BundleConfigLocation> {
        let scope = self
            .project
            .get(bundle_id)
            .map_or(BundleConfigScope::User, |dir| {
                BundleConfigScope::Project(dir)
            });
        bundle_config_location(&self.config_file, bundle_id, scope)
    }
}

/// Whether a bundle-relative source path is the user-owned configuration file
/// or one of its lock/temporary siblings. Such paths are never part of a
/// project bundle's sources, closure, or content fingerprint.
#[must_use]
pub(crate) fn is_bundle_config_entry(relative: &str) -> bool {
    relative == BUNDLE_CONFIG_FILE_NAME
        || relative
            .strip_prefix('.')
            .and_then(|rest| rest.strip_prefix(BUNDLE_CONFIG_FILE_NAME))
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Encode a bundle identity as one canonical percent-encoded path leaf.
pub(crate) fn encode_bundle_leaf(bundle_id: &str) -> anyhow::Result<String> {
    if bundle_id.is_empty() {
        bail!("bundle identity must not be empty");
    }
    let special_dot = matches!(bundle_id, "." | "..");
    let mut encoded = String::with_capacity(bundle_id.len());
    for byte in bundle_id.as_bytes() {
        if !special_dot && is_unreserved(*byte) {
            encoded.push(char::from(*byte));
        } else {
            push_percent_encoded(&mut encoded, *byte);
        }
    }
    if encoded.is_empty() {
        bail!("bundle identity must produce a non-empty path leaf");
    }
    Ok(encoded)
}

/// Decode a canonical leaf produced by [`encode_bundle_leaf`].
pub(crate) fn decode_bundle_leaf(leaf: &str) -> anyhow::Result<String> {
    if leaf.is_empty() {
        bail!("bundle configuration directory leaf must not be empty");
    }
    let bytes = leaf.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    bail!("bundle configuration leaf `{leaf}` has an incomplete escape");
                }
                let high = decode_hex(bytes[index + 1])?;
                let low = decode_hex(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte if is_unreserved(byte) => {
                decoded.push(byte);
                index += 1;
            }
            _ => {
                bail!("bundle configuration leaf `{leaf}` contains an unescaped byte");
            }
        }
    }
    let bundle_id = String::from_utf8(decoded)
        .with_context(|| format!("bundle configuration leaf `{leaf}` is not UTF-8"))?;
    let canonical = encode_bundle_leaf(&bundle_id)?;
    if canonical != leaf {
        bail!("bundle configuration leaf `{leaf}` is not canonical (use `{canonical}`)");
    }
    Ok(bundle_id)
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn push_percent_encoded(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push('%');
    output.push(char::from(HEX[(byte >> 4) as usize]));
    output.push(char::from(HEX[(byte & 0x0F) as usize]));
}

fn decode_hex(byte: u8) -> anyhow::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid percent escape byte 0x{byte:02X}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn user_scope_uses_encoded_leaf_under_config_dir_bundles() {
        let location = bundle_config_location(
            Path::new("/cfg/hya/config.yaml"),
            "hya/plan-impl-review",
            BundleConfigScope::User,
        )
        .unwrap();
        assert_eq!(
            location.dir(),
            Path::new("/cfg/hya/bundles/hya%2Fplan-impl-review")
        );
        assert_eq!(
            location.file(),
            Path::new("/cfg/hya/bundles/hya%2Fplan-impl-review/config.yml")
        );
        assert_eq!(
            location
                .env()
                .get(HYA_BUNDLE_CONFIG_FILE)
                .map(String::as_str),
            Some("/cfg/hya/bundles/hya%2Fplan-impl-review/config.yml")
        );
    }

    #[test]
    fn project_scope_uses_the_bundle_source_directory() {
        let resolver = BundleConfigResolver::new(
            PathBuf::from("/cfg/hya/config.yaml"),
            BTreeMap::from([(
                "acme/tools".to_string(),
                PathBuf::from("/work/.hya/bundles/acme__tools"),
            )]),
        );
        let project = resolver.location("acme/tools").unwrap();
        assert_eq!(
            project.file(),
            Path::new("/work/.hya/bundles/acme__tools/config.yml")
        );
        let user = resolver.location("acme/other").unwrap();
        assert_eq!(
            user.file(),
            Path::new("/cfg/hya/bundles/acme%2Fother/config.yml")
        );
        assert!(resolver.location("").is_err());
    }

    #[test]
    fn config_entries_cover_the_file_and_its_lock_and_temporaries() {
        assert!(is_bundle_config_entry("config.yml"));
        assert!(is_bundle_config_entry(".config.yml.lock"));
        assert!(is_bundle_config_entry(".config.yml.tmp-1-2"));
        assert!(!is_bundle_config_entry("nested/config.yml"));
        assert!(!is_bundle_config_entry("config.yaml"));
        assert!(!is_bundle_config_entry(".config.ymlx"));
    }
}
