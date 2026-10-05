//! Dependency-inverted control port for bundle management: list, install,
//! uninstall, enable, and disable bundles (`ListBundles`, `InstallBundle`,
//! `UninstallBundle`, `SetBundleEnabled`).
//!
//! The server owns the wire contract and route-facing error codes. The
//! application runtime (`hya-app`) supplies the implementation, which owns
//! the user bundle registry, `<directory>/.hya/bundles`, and package
//! validation; `hya bundle` uses the same implementation. The server
//! refreshes the scope's runtime catalog after every change.

use std::path::PathBuf;

use futures::future::BoxFuture;

/// Stable code: the application did not install a bundle control.
pub const BUNDLE_CONTROL_UNAVAILABLE: &str = "BUNDLE_CONTROL_UNAVAILABLE";
/// Stable code: malformed path or package, or a package that fails validation or its self-check.
pub const BUNDLE_INVALID_REQUEST: &str = "BUNDLE_INVALID_REQUEST";
/// Stable code: no bundle with that id in the named scope.
pub const BUNDLE_NOT_FOUND: &str = "BUNDLE_NOT_FOUND";
/// Stable code: first-party bundles and trusted presets cannot be removed or overridden.
pub const BUNDLE_IMMUTABLE: &str = "BUNDLE_IMMUTABLE";
/// Stable code: the install conflicts with an installed bundle (namespace, downgrade, content); `overwrite` replaces it.
pub const BUNDLE_CONFLICT: &str = "BUNDLE_CONFLICT";
/// Stable code: registry or filesystem failure.
pub const BUNDLE_CONTROL_FAILURE: &str = "BUNDLE_CONTROL_FAILURE";

/// What one bundle contributes, by id.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BundleListingComponents {
    /// Agent ids.
    pub agents: Vec<String>,
    /// Skill ids.
    pub skills: Vec<String>,
    /// Tool names.
    pub tools: Vec<String>,
    /// MCP server ids.
    pub mcp_servers: Vec<String>,
    /// Workflow ids.
    pub workflows: Vec<String>,
    /// Permission mode ids.
    pub permission_modes: Vec<String>,
    /// API endpoint names.
    pub apis: Vec<String>,
    /// Hook count.
    pub hooks: u32,
    /// The bundle declares a TUI extension.
    pub tui: bool,
    /// The TUI extension's permissions.
    pub tui_permissions: Vec<String>,
}

/// One bundle as installed or shipped (`ListBundles` row).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BundleListing {
    /// Bundle id.
    pub id: String,
    /// Bundle version.
    pub version: String,
    /// Publisher.
    pub publisher: String,
    /// `user`, `project`, or `first_party`.
    pub scope: String,
    /// Manifest kind.
    pub kind: String,
    /// `active`, `shadowed`, `disabled`, or `unreadable`.
    pub state: String,
    /// Not in the disabled set.
    pub enabled: bool,
    /// `UninstallBundle` can remove it.
    pub removable: bool,
    /// Manifest description.
    pub description: String,
    /// Prepared digest; empty when unreadable.
    pub prepared_digest: String,
    /// Why it is unreadable.
    pub error: String,
    /// What it contributes.
    pub components: BundleListingComponents,
}

/// Bounded structured failure returned by the bundle control port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleControlError {
    /// Machine-readable stable code.
    pub code: String,
    /// Human-readable diagnostic.
    pub message: String,
}

impl BundleControlError {
    /// Construct one failure.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BundleControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BundleControlError {}

/// Boxed asynchronous result returned by a [`BundleControl`] operation.
pub type BundleControlFuture<'a, T> = BoxFuture<'a, Result<T, BundleControlError>>;

/// Server-owned control port for bundle management. `project_dir` is the
/// `.hya/bundles` directory of the request's directory, `None` without one.
pub trait BundleControl: Send + Sync {
    /// Every bundle: first-party, installed, and `project_dir`'s, sorted by id then scope.
    fn list(&self, project_dir: Option<PathBuf>) -> BundleControlFuture<'_, Vec<BundleListing>>;

    /// Install the `.hyabundle` package at `package` into the user registry,
    /// or into `project_dir` when given. Returns the bundle id.
    fn install(
        &self,
        package: PathBuf,
        project_dir: Option<PathBuf>,
        overwrite: bool,
    ) -> BundleControlFuture<'_, String>;

    /// Remove an installed bundle from the user registry, or from `project_dir` when given.
    fn uninstall(
        &self,
        bundle_id: String,
        project_dir: Option<PathBuf>,
    ) -> BundleControlFuture<'_, ()>;

    /// Enable or disable a bundle id in every scope.
    fn set_enabled(&self, bundle_id: String, enabled: bool) -> BundleControlFuture<'_, ()>;
}

/// Default control used by callers that do not install an application runtime.
pub(crate) struct EmptyBundleControl;

fn unavailable<T>() -> BundleControlFuture<'static, T> {
    Box::pin(async {
        Err(BundleControlError::new(
            BUNDLE_CONTROL_UNAVAILABLE,
            "bundle control is unavailable",
        ))
    })
}

impl BundleControl for EmptyBundleControl {
    fn list(&self, _: Option<PathBuf>) -> BundleControlFuture<'_, Vec<BundleListing>> {
        unavailable()
    }

    fn install(&self, _: PathBuf, _: Option<PathBuf>, _: bool) -> BundleControlFuture<'_, String> {
        unavailable()
    }

    fn uninstall(&self, _: String, _: Option<PathBuf>) -> BundleControlFuture<'_, ()> {
        unavailable()
    }

    fn set_enabled(&self, _: String, _: bool) -> BundleControlFuture<'_, ()> {
        unavailable()
    }
}
