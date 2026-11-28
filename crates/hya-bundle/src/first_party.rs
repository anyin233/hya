//! Runtime discovery of hya's trusted first-party bundles.
//!
//! First-party tools, agents, skills, commands, channel policy and workflows are bundle
//! sources. The backend loads them when it starts instead of compiling them
//! into the binary. An installed backend (`<prefix>/bin/hya`) trusts
//! only the allowlisted packages in `<prefix>/bundles/`. A Cargo build reads the
//! in-tree sources, so edits apply on restart and a stale staged package cannot
//! shadow them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::{
    Bundle, BundleError, BundleSource, PreparedCatalog, inspect_public_package, prepare_package,
};

/// Identities of every trusted first-party bundle, in load order.
pub const FIRST_PARTY_BUNDLES: &[&str] = &[
    "hya/hashline",
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network-tools",
    "hya/channel-tools",
    "hya/core-skills",
    "hya/core-commands",
    "hya/core-agents",
    "hya/agent-channels",
    "hya/goal-loop",
    "hya/plan-impl-review",
    "hya/basic-tui-components",
    "hya/compaction",
];

/// First-party internal bundles under `bundles/builtin/`: native libraries that
/// carry full-mode features out of the kernel (ADR-0033). Every entry is also in
/// [`FIRST_PARTY_BUNDLES`]; bare mode loads none of them.
pub const INTERNAL_BUNDLES: &[&str] = &["hya/compaction"];

/// In-tree source groups searched for a first-party bundle directory.
const SOURCE_GROUPS: &[&str] = &["builtin"];

/// Where one first-party bundle is loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstPartySource {
    /// An in-tree bundle source directory (Cargo builds).
    Directory(PathBuf),
    /// A public `.hyabundle` package (installed layouts).
    Package(PathBuf),
}

/// Environment override of [`first_party_source_root`]: a directory with the
/// same `builtin/` layout. A daemon rolling back a failed
/// restart sets it for its pinned previous build, so that build loads the
/// first-party sources it ran with instead of the since-edited tree.
pub const FIRST_PARTY_SOURCE_ROOT_ENV: &str = "HYA_FIRST_PARTY_SOURCE_ROOT";

/// Root of the in-tree first-party bundle sources for this build
/// ([`FIRST_PARTY_SOURCE_ROOT_ENV`] when set).
#[must_use]
pub fn first_party_source_root() -> PathBuf {
    std::env::var_os(FIRST_PARTY_SOURCE_ROOT_ENV)
        .filter(|root| !root.is_empty())
        .map_or_else(
            || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundles"),
            PathBuf::from,
        )
}

/// Package file name for a first-party identity, such as `hya-core-agents.hyabundle`.
#[must_use]
pub fn first_party_package_name(identity: &str) -> Option<String> {
    FIRST_PARTY_BUNDLES
        .contains(&identity)
        .then(|| identity.strip_prefix("hya/"))
        .flatten()
        .map(|name| format!("hya-{name}.hyabundle"))
}

/// In-tree source directory of a trusted first-party bundle.
#[must_use]
pub fn first_party_source_directory(identity: &str) -> Option<PathBuf> {
    first_party_package_name(identity)?;
    let name = identity.strip_prefix("hya/")?;
    let root = first_party_source_root();
    SOURCE_GROUPS
        .iter()
        .map(|group| root.join(group).join(name))
        .find(|directory| directory.join("bundle.yaml").is_file())
}

/// Resolve where `identity` loads from for an executable in `executable_dir`.
///
/// `bin/` is the installed layout and only uses `../bundles/`. Any other
/// directory is a Cargo layout: the in-tree source wins, then a package staged
/// under the profile's `bundles/` directory. Unknown identities never resolve.
#[must_use]
pub fn first_party_source(executable_dir: &Path, identity: &str) -> Option<FirstPartySource> {
    let package_name = first_party_package_name(identity)?;
    let file_name = executable_dir.file_name().and_then(std::ffi::OsStr::to_str);
    let prefix = match file_name {
        Some("bin" | "deps") => executable_dir.parent().unwrap_or(executable_dir),
        _ => executable_dir,
    };
    let package = prefix.join("bundles").join(package_name);
    if file_name == Some("bin") {
        return package
            .is_file()
            .then_some(FirstPartySource::Package(package));
    }
    first_party_source_directory(identity)
        .map(FirstPartySource::Directory)
        .or_else(|| {
            package
                .is_file()
                .then_some(FirstPartySource::Package(package))
        })
}

/// Prepare one first-party bundle and check that it is exactly `identity`.
///
/// # Errors
///
/// Returns the source, package, or preparation failure, or
/// [`BundleError::FirstPartyBundle`] when the catalog is not exactly the
/// requested bundle.
pub fn load_first_party(
    source: &FirstPartySource,
    identity: &str,
) -> Result<PreparedCatalog, BundleError> {
    let catalog = match source {
        FirstPartySource::Directory(directory) => {
            prepare_package(BundleSource::read_directory(directory)?)?
        }
        FirstPartySource::Package(package) => {
            let bytes = std::fs::read(package).map_err(|error| BundleError::Io {
                path: package.display().to_string(),
                detail: error.to_string(),
            })?;
            inspect_public_package(&bytes)?
        }
    };
    match catalog.bundles() {
        [bundle] if bundle.identity().id == identity => Ok(catalog),
        bundles => Err(BundleError::FirstPartyBundle {
            identity: identity.to_string(),
            detail: format!(
                "expected exactly `{identity}`, found [{}]",
                bundles
                    .iter()
                    .map(|bundle| bundle.identity().id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

/// Built-in first-party bundle profile (`config.yaml` -> `bundles.builtin.profile`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinBundleProfile {
    /// Enable all built-in first-party bundles by default (subject to `disable`).
    #[default]
    Full,
    /// Start with zero built-in bundles enabled (`enable` selectively adds built-ins).
    Bare,
}

/// Selective enable/disable configuration for built-in first-party bundles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinBundleSelection {
    /// Base profile (`full` or `bare`).
    pub profile: BuiltinBundleProfile,
    /// Built-in identities explicitly enabled (additive on `bare`, or overriding absence on `full`).
    pub enable: Vec<String>,
    /// Built-in identities explicitly disabled (takes precedence over `profile` and `enable`).
    pub disable: Vec<String>,
}

impl BuiltinBundleSelection {
    /// Select all built-in bundles (`profile: full`).
    #[must_use]
    pub fn full() -> Self {
        Self {
            profile: BuiltinBundleProfile::Full,
            enable: Vec::new(),
            disable: Vec::new(),
        }
    }

    /// Select no built-in bundles (`profile: bare`).
    #[must_use]
    pub fn bare() -> Self {
        Self {
            profile: BuiltinBundleProfile::Bare,
            enable: Vec::new(),
            disable: Vec::new(),
        }
    }

    /// Whether built-in bundle `identity` is enabled under this selection.
    ///
    /// `disable` takes precedence over both `profile` and `enable`.
    #[must_use]
    pub fn is_enabled(&self, identity: &str) -> bool {
        if self.disable.iter().any(|entry| entry == identity) {
            return false;
        }
        match self.profile {
            BuiltinBundleProfile::Full => true,
            BuiltinBundleProfile::Bare => self.enable.iter().any(|entry| entry == identity),
        }
    }

    /// All trusted first-party identities enabled by this selection, in load order.
    #[must_use]
    pub fn enabled_identities(&self) -> Vec<&'static str> {
        FIRST_PARTY_BUNDLES
            .iter()
            .copied()
            .filter(|identity| self.is_enabled(identity))
            .collect()
    }

    /// Any configured `enable` or `disable` entries not present in [`FIRST_PARTY_BUNDLES`].
    #[must_use]
    pub fn unknown_identities(&self) -> Vec<String> {
        let mut unknown = Vec::new();
        for entry in self.enable.iter().chain(&self.disable) {
            if !FIRST_PARTY_BUNDLES.contains(&entry.as_str()) && !unknown.contains(entry) {
                unknown.push(entry.clone());
            }
        }
        unknown
    }
}

/// Resolution status for one built-in first-party bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuiltinBundleStatus {
    /// Built-in bundle was enabled and loaded cleanly.
    Loaded,
    /// Built-in bundle was disabled by profile or `disable` configuration.
    Disabled,
    /// Built-in bundle was superseded by a project or user-installed bundle.
    Overridden {
        /// Identity of the overriding bundle.
        by: String,
    },
    /// Built-in bundle source or packaged `.hyabundle` was not found on disk.
    Missing {
        /// Diagnostic detail explaining why it was missing.
        reason: String,
    },
    /// Built-in bundle source/package existed, but failed to prepare or validate.
    Failed {
        /// Diagnostic error detail.
        reason: String,
    },
}

/// Resolution entry for a single built-in bundle in [`BuiltinResolutionReport`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinBundleEntry {
    /// Built-in bundle identity from [`FIRST_PARTY_BUNDLES`].
    pub identity: &'static str,
    /// Outcome of resolving this built-in bundle.
    pub status: BuiltinBundleStatus,
}

/// Non-panicking resolution outcome across all trusted first-party bundles.
#[derive(Clone, Debug)]
pub struct BuiltinResolutionReport {
    /// Active selection used to resolve built-ins.
    pub selection: BuiltinBundleSelection,
    /// Catalogs successfully loaded from enabled, non-overridden built-ins.
    pub loaded: Vec<PreparedCatalog>,
    /// Per-builtin status entries in [`FIRST_PARTY_BUNDLES`] order.
    pub entries: Vec<BuiltinBundleEntry>,
    /// Configured `enable`/`disable` identities not matching [`FIRST_PARTY_BUNDLES`].
    pub unknown_configured: Vec<String>,
}

impl BuiltinResolutionReport {
    /// Loaded [`Bundle`] payloads from enabled, non-overridden built-ins.
    pub fn loaded_bundles(&self) -> impl Iterator<Item = &Bundle> {
        self.loaded
            .iter()
            .flat_map(|catalog| catalog.bundles().iter())
    }

    /// Find a loaded built-in bundle by identity.
    #[must_use]
    pub fn bundle(&self, identity: &str) -> Option<&Bundle> {
        self.loaded_bundles()
            .find(|bundle| bundle.identity().id == identity)
    }

    /// Look up the resolution status for a built-in bundle identity.
    #[must_use]
    pub fn status(&self, identity: &str) -> Option<&BuiltinBundleStatus> {
        self.entries
            .iter()
            .find(|entry| entry.identity == identity)
            .map(|entry| &entry.status)
    }
}

/// Load a first-party bundle if its trusted source or package exists, without
/// treating missing bundles as a fatal error.
///
/// # Errors
///
/// Returns [`BundleError::FirstPartyBundle`] when the identity is unknown, or
/// the preparation/package error if a present source/package is invalid.
pub fn first_party_bundle_optional(
    identity: &str,
) -> Result<Option<&'static PreparedCatalog>, BundleError> {
    if !FIRST_PARTY_BUNDLES.contains(&identity) {
        return Err(BundleError::FirstPartyBundle {
            identity: identity.to_owned(),
            detail: "unknown first-party identity".into(),
        });
    }
    let missing = |detail: String| BundleError::FirstPartyBundle {
        identity: identity.to_string(),
        detail,
    };
    let executable = std::env::current_exe().map_err(|error| missing(error.to_string()))?;
    let directory = executable
        .parent()
        .ok_or_else(|| missing("executable has no parent directory".to_string()))?;
    if first_party_source(directory, identity).is_none() {
        return Ok(None);
    }
    first_party_bundle(identity).map(Some)
}

/// Resolve all [`FIRST_PARTY_BUNDLES`] according to `selection` and `external_bundles`
/// (project `.hya/bundles/` and user-installed bundles), filtering and reporting
/// disabled, overridden, missing, or invalid built-ins cleanly without panicking.
#[must_use]
pub fn resolve_first_party_bundles(
    selection: &BuiltinBundleSelection,
    external_bundles: &[&Bundle],
) -> BuiltinResolutionReport {
    resolve_first_party_bundles_with(selection, external_bundles, |identity| {
        first_party_bundle_optional(identity).map(|opt| opt.cloned())
    })
}

/// Resolve [`FIRST_PARTY_BUNDLES`] using a custom loader callback.
///
/// This enables deterministic handling and testing of missing or failing
/// built-in bundles without touching process-global environment or caches.
pub fn resolve_first_party_bundles_with<F>(
    selection: &BuiltinBundleSelection,
    external_bundles: &[&Bundle],
    mut load: F,
) -> BuiltinResolutionReport
where
    F: FnMut(&str) -> Result<Option<PreparedCatalog>, BundleError>,
{
    let mut loaded = Vec::new();
    let mut entries = Vec::with_capacity(FIRST_PARTY_BUNDLES.len());
    for &identity in FIRST_PARTY_BUNDLES {
        if !selection.is_enabled(identity) {
            entries.push(BuiltinBundleEntry {
                identity,
                status: BuiltinBundleStatus::Disabled,
            });
            continue;
        }
        if let Some(overrider) = external_bundles
            .iter()
            .find(|bundle| bundle.identity().id == identity || bundle.overrides() == Some(identity))
        {
            entries.push(BuiltinBundleEntry {
                identity,
                status: BuiltinBundleStatus::Overridden {
                    by: overrider.identity().id.clone(),
                },
            });
            continue;
        }
        match load(identity) {
            Ok(Some(catalog)) => {
                loaded.push(catalog);
                entries.push(BuiltinBundleEntry {
                    identity,
                    status: BuiltinBundleStatus::Loaded,
                });
            }
            Ok(None) => {
                entries.push(BuiltinBundleEntry {
                    identity,
                    status: BuiltinBundleStatus::Missing {
                        reason: format!("no trusted source or package found for `{identity}`"),
                    },
                });
            }
            Err(error) => {
                let status = if matches!(error, BundleError::FirstPartyBundle { .. })
                    && error.to_string().contains("no trusted source or package")
                {
                    BuiltinBundleStatus::Missing {
                        reason: error.to_string(),
                    }
                } else {
                    BuiltinBundleStatus::Failed {
                        reason: error.to_string(),
                    }
                };
                entries.push(BuiltinBundleEntry { identity, status });
            }
        }
    }
    BuiltinResolutionReport {
        selection: selection.clone(),
        loaded,
        entries,
        unknown_configured: selection.unknown_identities(),
    }
}

/// Load a first-party bundle for the running executable, once per process.
///
/// # Errors
///
/// Returns [`BundleError::FirstPartyBundle`] when the identity is unknown or
/// its source/package is missing, or the load failure from [`load_first_party`].
pub fn first_party_bundle(identity: &str) -> Result<&'static PreparedCatalog, BundleError> {
    type Slot = Arc<Mutex<Option<&'static PreparedCatalog>>>;
    static LOADED: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    if !FIRST_PARTY_BUNDLES.contains(&identity) {
        return Err(BundleError::FirstPartyBundle {
            identity: identity.to_owned(),
            detail: "unknown first-party identity".into(),
        });
    }
    let slot = LOADED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(identity.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone();
    let mut loaded = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(catalog) = *loaded {
        return Ok(catalog);
    }
    let missing = |detail: String| BundleError::FirstPartyBundle {
        identity: identity.to_string(),
        detail,
    };
    let executable = std::env::current_exe().map_err(|error| missing(error.to_string()))?;
    let directory = executable
        .parent()
        .ok_or_else(|| missing("executable has no parent directory".to_string()))?;
    let source = first_party_source(directory, identity).ok_or_else(|| {
        missing(format!(
            "no trusted source or package near {}",
            executable.display()
        ))
    })?;
    // Loaded bundles are immutable process-lifetime data, like mapped tool libraries.
    let catalog: &'static PreparedCatalog =
        Box::leak(Box::new(load_first_party(&source, identity)?));
    *loaded = Some(catalog);
    Ok(catalog)
}
