//! Read-only inventory of the trusted first-party presets loaded at runtime.

use hya_bundle::{BundleError, PreparedInstallableBundle, first_party_bundle};

/// Display metadata for one immutable trusted preset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedPresetDescriptor {
    /// Stable preset identity.
    pub id: String,
    /// Prepared payload kind (`Plugin` or `AgentSetBundle`).
    pub kind: String,
    /// Preset payload version.
    pub version: String,
    /// Canonical prepared document digest.
    pub digest: String,
    /// Presets ship with the executable and cannot be mutated in place.
    pub immutable: bool,
    /// Presets do not participate in public install/uninstall lifecycle.
    pub installable: bool,
    /// Stable Agent ids exported by the preset.
    pub agent_ids: Vec<String>,
    /// Canonical tool/resource ids exposed by the preset.
    pub resource_ids: Vec<String>,
}

/// Return trusted presets in stable identity order.
///
/// This is a parallel display inventory. It never inserts preset payloads into
/// the public [`hya_bundle::BundleCatalog`], so listing cannot introduce
/// self-shadowing or make presets installable.
///
/// # Errors
/// Returns a bundle integrity error if a trusted first-party bundle fails to load.
pub fn trusted_preset_inventory() -> Result<Vec<TrustedPresetDescriptor>, BundleError> {
    let core = hya_core::core_agents_preset()?;
    let core_skills = first_party_bundle("hya/core-skills")?;
    let core_skills_bundle = single_bundle(core_skills.bundles(), "hya/core-skills")?;
    let core_commands = first_party_bundle("hya/core-commands")?;
    let core_commands_bundle = single_bundle(core_commands.bundles(), "hya/core-commands")?;
    let channels = first_party_bundle(hya_core::AGENT_CHANNELS_PRESET_ID)?;
    let channel_bundle = single_bundle(channels.bundles(), hya_core::AGENT_CHANNELS_PRESET_ID)?;
    let mut presets = hya_tool::tool_bundle_presets()
        .iter()
        .map(|policy| {
            let catalog = first_party_bundle(policy.identity())?;
            let bundle = single_bundle(catalog.bundles(), policy.identity())?;
            Ok(TrustedPresetDescriptor {
                id: bundle.identity().id.clone(),
                kind: bundle.kind().as_str().to_string(),
                version: bundle.identity().version.clone(),
                digest: catalog.digest().to_string(),
                immutable: true,
                installable: false,
                agent_ids: Vec::new(),
                resource_ids: policy
                    .tools()
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, BundleError>>()?;
    presets.extend([
        TrustedPresetDescriptor {
            id: core_commands_bundle.identity().id.clone(),
            kind: core_commands_bundle.kind().as_str().to_string(),
            version: core_commands_bundle.identity().version.clone(),
            digest: core_commands.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            // Template files; `commands` is the declaration manifest, not a command.
            resource_ids: core_commands_bundle
                .extensions()
                .iter()
                .filter(|asset| asset.local_id != "commands")
                .map(|asset| asset.local_id.clone())
                .collect(),
        },
        TrustedPresetDescriptor {
            id: core_skills_bundle.identity().id.clone(),
            kind: core_skills_bundle.kind().as_str().to_string(),
            version: core_skills_bundle.identity().version.clone(),
            digest: core_skills.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: core_skills_bundle
                .skills()
                .iter()
                .map(|skill| skill.local_id.clone())
                .collect(),
        },
        TrustedPresetDescriptor {
            id: channel_bundle.identity().id.clone(),
            kind: channel_bundle.kind().as_str().to_string(),
            version: channel_bundle.identity().version.clone(),
            digest: channel_bundle.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: channel_bundle
                .channels()
                .iter()
                .map(|channel| channel.id.clone())
                .collect(),
        },
        TrustedPresetDescriptor {
            id: core.bundle_id().to_string(),
            kind: "AgentSetBundle".to_string(),
            version: core.version().to_string(),
            digest: core.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: core
                .agents()
                .iter()
                .map(|agent| agent.id.as_str().to_string())
                .collect(),
            resource_ids: Vec::new(),
        },
    ]);
    presets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(presets)
}

fn single_bundle<'a>(
    bundles: &'a [PreparedInstallableBundle],
    identity: &str,
) -> Result<&'a PreparedInstallableBundle, BundleError> {
    bundles.first().ok_or_else(|| BundleError::InvalidManifest {
        source_name: identity.to_string(),
        detail: "trusted preset catalog is empty".to_string(),
    })
}
