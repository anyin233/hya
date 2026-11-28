//! The `hya` CLI commands that enabled bundles contribute (ADR-0040).

use anyhow::Context as _;
use hya_bundle::{BuiltinBundleSelection, CliCommand, PreparedCatalog};

/// One bundle's CLI contribution.
#[derive(Clone, Debug)]
pub struct CliContribution {
    /// Contributing bundle id.
    pub bundle: String,
    /// The declared command.
    pub command: CliCommand,
}

/// The CLI contributions of the enabled first-party bundles (none with
/// `bare`) and of the enabled user-installed bundles, in that order.
///
/// # Errors
/// A first-party bundle or the installed registry fails to load.
pub async fn cli_contributions(bare: bool) -> anyhow::Result<Vec<CliContribution>> {
    let selection = if bare {
        BuiltinBundleSelection::bare()
    } else {
        crate::config::configured_builtin_bundles()
    };
    let mut catalogs = crate::installed_bundle_refresh::first_party_catalogs_for(&selection)
        .context("load first-party bundle CLI contributions")?;
    let registry_path = crate::installed_bundle_refresh::bundle_registry_path();
    if registry_path.is_file() {
        let registry = crate::bundle_registry::BundleRegistry::connect(
            registry_path
                .to_str()
                .context("bundle registry path is not UTF-8")?,
        )
        .await
        .context("open the installed bundle registry")?;
        let disabled = registry
            .disabled_bundle_ids()
            .await
            .context("read disabled bundles")?;
        let snapshot = registry
            .snapshot_with_catalogs()
            .await
            .context("read installed bundles")?;
        catalogs.extend(
            snapshot
                .bundles
                .into_iter()
                .filter(|loaded| !disabled.contains(&loaded.record.bundle_id))
                .map(|loaded| loaded.prepared),
        );
    }
    Ok(contributions_of(&catalogs))
}

fn contributions_of(catalogs: &[PreparedCatalog]) -> Vec<CliContribution> {
    catalogs
        .iter()
        .flat_map(|catalog| catalog.bundles())
        .flat_map(|bundle| {
            bundle.cli().iter().map(|command| CliContribution {
                bundle: bundle.identity().id.clone(),
                command: command.clone(),
            })
        })
        .collect()
}
