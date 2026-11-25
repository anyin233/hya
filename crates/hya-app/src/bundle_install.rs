//! Install policy shared by the user registry and Project directories.

use hya_bundle::{Bundle, BundleCatalog};

use crate::bundle_registry::BundleRegistryError;

/// How an install reacts when the incoming bundle's namespace is contested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceInstallPolicy {
    /// Refuse the install on any namespace or downgrade conflict.
    DenyConflicts,
    /// Resolve a cross-bundle namespace conflict by replacing the incumbent
    /// bundle, and accept downgrades of the same bundle id.
    OverwriteConflicts,
}

/// What [`crate::bundle_registry::BundleRegistry::install`] would do with a candidate, as reported by
/// [`crate::bundle_registry::BundleRegistry::plan_install`] without writing anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleInstallPlan {
    /// Effect on the candidate's own bundle id.
    pub action: BundleInstallAction,
    /// Other installed bundle ids removed because the candidate takes over
    /// their namespace (only under [`NamespaceInstallPolicy::OverwriteConflicts`]).
    pub displaced: Vec<String>,
}

/// Effect of an install on the candidate's bundle id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BundleInstallAction {
    /// The bundle id is not installed yet.
    Install,
    /// A different version of the bundle id is installed and would be replaced.
    Replace {
        /// Version currently installed.
        installed_version: String,
    },
    /// The same package content is already installed.
    Unchanged,
}

/// Semver-aware downgrade check: `true` when `incoming` is strictly lower
/// than `installed`. Non-semver versions never count as downgrades (they keep
/// the legacy replace-always behavior). Shared with project-scope installs so
/// both scopes apply one rule.
#[must_use]
pub fn is_downgrade(incoming: &str, installed: &str) -> bool {
    match (
        semver::Version::parse(incoming),
        semver::Version::parse(installed),
    ) {
        (Ok(incoming), Ok(installed)) => incoming < installed,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallScope {
    User,
    Project,
}

pub(crate) fn check_reserved_agents(
    incoming: &Bundle,
    reserved_agent_ids: &[&str],
) -> Result<(), BundleRegistryError> {
    for agent in incoming.agents() {
        if reserved_agent_ids.contains(&agent.id.as_str()) {
            return Err(BundleRegistryError::BundleAgentIdReserved {
                bundle_id: incoming.identity().id.clone(),
                agent_id: agent.id.as_str().to_string(),
            });
        }
    }
    Ok(())
}

/// Plan against the readable installations. Each scope supplies its own content digest.
pub(crate) fn plan_install<'a, D: PartialEq + ?Sized + 'a>(
    installed: impl Iterator<Item = (&'a Bundle, &'a D)> + Clone,
    incoming: &Bundle,
    incoming_digest: &D,
    policy: NamespaceInstallPolicy,
    scope: InstallScope,
) -> Result<BundleInstallPlan, BundleRegistryError> {
    let identity = incoming.identity();
    let bundle_id = &identity.id;
    let version = &identity.version;
    let namespace = incoming.namespace();
    let existing = installed
        .clone()
        .find(|(bundle, _)| &bundle.identity().id == bundle_id);
    let installed_version = existing.map(|(bundle, _)| &bundle.identity().version);
    let same_content = existing.is_some_and(|(_, digest)| digest == incoming_digest);
    let deny_conflicts = policy == NamespaceInstallPolicy::DenyConflicts;

    if let Some(installed_version) = installed_version
        && !same_content
        && deny_conflicts
        && is_downgrade(version, installed_version)
    {
        return Err(BundleRegistryError::BundleDowngradeRequired {
            bundle_id: bundle_id.clone(),
            installed_version: installed_version.clone(),
            incoming_version: version.clone(),
        });
    }

    let displaced: Vec<String> = installed
        .clone()
        .map(|(bundle, _)| bundle)
        .filter(|bundle| &bundle.identity().id != bundle_id && bundle.namespace() == namespace)
        .map(|bundle| bundle.identity().id.clone())
        .collect();
    if let Some(owner) = displaced.first()
        && deny_conflicts
    {
        return Err(BundleRegistryError::NamespaceConflict {
            namespace: namespace.to_string(),
            existing_bundle_id: owner.clone(),
            incoming_bundle_id: bundle_id.clone(),
        });
    }

    if scope == InstallScope::User {
        // Retain the current catalog on same-content and same-version installs.
        let replaces = !same_content && installed_version.is_some_and(|old| old != version);
        let adds_candidate = existing.is_none() || replaces || !displaced.is_empty();
        validate_install_catalog(
            installed.map(|(bundle, _)| bundle),
            replaces.then_some(bundle_id.as_str()),
            &displaced,
            adds_candidate.then_some(incoming),
        )?;
    }

    let action = match installed_version {
        None => BundleInstallAction::Install,
        Some(_) if same_content => BundleInstallAction::Unchanged,
        Some(old) if old == version && (deny_conflicts || scope == InstallScope::User) => {
            return Err(BundleRegistryError::BundleContentConflict {
                bundle_id: bundle_id.clone(),
                version: version.clone(),
            });
        }
        Some(old) => BundleInstallAction::Replace {
            installed_version: old.clone(),
        },
    };
    Ok(BundleInstallPlan { action, displaced })
}

pub(crate) fn validate_install_catalog<'a>(
    installed: impl Iterator<Item = &'a Bundle>,
    replaced_id: Option<&str>,
    displaced: &[String],
    incoming: Option<&'a Bundle>,
) -> Result<(), BundleRegistryError> {
    let complete: Vec<_> = installed
        .filter(|bundle| {
            let id = &bundle.identity().id;
            replaced_id != Some(id.as_str()) && !displaced.contains(id)
        })
        .chain(incoming)
        .cloned()
        .collect();
    BundleCatalog::from_prepared(&complete)?;
    Ok(())
}
