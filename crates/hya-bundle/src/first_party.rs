//! Runtime discovery of hya's trusted first-party bundles.
//!
//! First-party tools, agents, skills, commands, channel policy and workflows are bundle
//! sources. The backend loads them when it starts instead of compiling them
//! into the binary. An installed backend (`<prefix>/bin/hya-backend`) trusts
//! only the allowlisted packages in `<prefix>/bundles/`. A Cargo build reads the
//! in-tree sources, so edits apply on restart and a stale staged package cannot
//! shadow them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::{BundleError, BundleSource, PreparedCatalog, inspect_public_package, prepare_package};

/// Identities of every trusted first-party bundle, in load order.
pub const FIRST_PARTY_BUNDLES: &[&str] = &[
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network-tools",
    "hya/channel-tools",
    "hya/todo-tools",
    "hya/core-skills",
    "hya/core-commands",
    "hya/core-agents",
    "hya/agent-channels",
    "hya/goal-loop",
    "hya/plan-impl-review",
    "hya/subagents",
];

/// In-tree source groups searched for a first-party bundle directory.
const SOURCE_GROUPS: &[&str] = &["presets", "first-party"];

/// Where one first-party bundle is loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstPartySource {
    /// An in-tree bundle source directory (Cargo builds).
    Directory(PathBuf),
    /// A public `.hyabundle` package (installed layouts).
    Package(PathBuf),
}

/// Root of the in-tree first-party bundle sources for this build.
#[must_use]
pub fn first_party_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundles")
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

/// Resolve where `identity` loads from for an executable in `executable_dir`.
///
/// `bin/` is the installed layout and only uses `../bundles/`. Any other
/// directory is a Cargo layout: the in-tree source wins, then a package staged
/// under the profile's `bundles/` directory. Unknown identities never resolve.
#[must_use]
pub fn first_party_source(executable_dir: &Path, identity: &str) -> Option<FirstPartySource> {
    let package_name = first_party_package_name(identity)?;
    let name = identity.strip_prefix("hya/")?;
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
    let root = first_party_source_root();
    SOURCE_GROUPS
        .iter()
        .map(|group| root.join(group).join(name))
        .find(|directory| directory.join("bundle.yaml").is_file())
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

/// Load a first-party bundle for the running executable, once per process.
///
/// # Errors
///
/// Returns [`BundleError::FirstPartyBundle`] when the identity is unknown or
/// its source/package is missing, or the load failure from [`load_first_party`].
pub fn first_party_bundle(identity: &str) -> Result<&'static PreparedCatalog, BundleError> {
    static LOADED: OnceLock<Mutex<HashMap<String, &'static PreparedCatalog>>> = OnceLock::new();
    let mut loaded = LOADED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(catalog) = loaded.get(identity) {
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
    loaded.insert(identity.to_string(), catalog);
    Ok(catalog)
}
