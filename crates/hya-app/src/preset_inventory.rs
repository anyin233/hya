//! Read-only inventory of the trusted first-party presets and internal bundles
//! loaded at runtime.

use hya_bundle::{
    BuiltinBundleSelection, Bundle, BundleError, PreparedCatalog, first_party_bundle_optional,
};
use serde::Deserialize;

const TOOL_FAMILY_BUNDLES: [&str; 5] = [
    "hya/hashline",
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network",
    "hya/channel-tools",
];

#[derive(Debug, Default, Deserialize)]
struct ExposureHeader {
    #[serde(default)]
    tools: Vec<ExposureToolName>,
}

#[derive(Debug, Deserialize)]
struct ExposureToolName {
    name: String,
}

/// Display metadata for one immutable trusted preset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedPresetDescriptor {
    /// Stable preset identity.
    pub id: String,
    /// Prepared payload kind (`Plugin` or `Bundle`).
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

/// Return trusted presets and internal bundles in stable identity order using
/// the active `config.yaml` [`BuiltinBundleSelection`].
///
/// Disabled or missing built-in bundles are omitted cleanly without panicking.
///
/// # Errors
/// Returns a bundle integrity error if an enabled, present first-party bundle
/// fails to prepare or validate.
pub fn trusted_preset_inventory() -> Result<Vec<TrustedPresetDescriptor>, BundleError> {
    trusted_preset_inventory_for(&crate::config::configured_builtin_bundles())
}

/// Return trusted presets and internal bundles enabled by `selection` in
/// stable identity order.
///
/// Disabled or missing built-in bundles are omitted cleanly without panicking.
///
/// # Errors
/// Returns a bundle integrity error if an enabled, present first-party bundle
/// fails to prepare or validate.
pub fn trusted_preset_inventory_for(
    selection: &BuiltinBundleSelection,
) -> Result<Vec<TrustedPresetDescriptor>, BundleError> {
    let mut presets = Vec::new();
    let load_enabled = |identity: &str| -> Result<Option<&'static PreparedCatalog>, BundleError> {
        if !selection.is_enabled(identity) {
            return Ok(None);
        }
        first_party_bundle_optional(identity)
    };

    for identity in TOOL_FAMILY_BUNDLES {
        let Some(catalog) = load_enabled(identity)? else {
            continue;
        };
        let bundle = single_bundle(catalog.bundles(), identity)?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: catalog.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: tool_resource_ids(bundle)?,
        });
    }

    if let Some(core_commands) = load_enabled("hya/core-commands")? {
        let bundle = single_bundle(core_commands.bundles(), "hya/core-commands")?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: core_commands.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            // Template files; `commands` is the declaration manifest, not a command.
            resource_ids: bundle
                .extensions()
                .iter()
                .filter(|asset| asset.local_id != "commands")
                .map(|asset| asset.local_id.clone())
                .collect(),
        });
    }

    if let Some(core_skills) = load_enabled("hya/core-skills")? {
        let bundle = single_bundle(core_skills.bundles(), "hya/core-skills")?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: core_skills.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: bundle
                .skills()
                .iter()
                .map(|skill| skill.local_id.clone())
                .collect(),
        });
    }

    if let Some(channels) = load_enabled(hya_engine::AGENT_CHANNELS_PRESET_ID)? {
        let bundle = single_bundle(channels.bundles(), hya_engine::AGENT_CHANNELS_PRESET_ID)?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: bundle.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: bundle
                .channels()
                .iter()
                .map(|channel| channel.id.clone())
                .collect(),
        });
    }

    if let Some(core_agents) = load_enabled("hya/core-agents")? {
        let bundle = single_bundle(core_agents.bundles(), "hya/core-agents")?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: core_agents.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: bundle
                .agents()
                .iter()
                .map(|agent| agent.id.as_str().to_string())
                .collect(),
            resource_ids: Vec::new(),
        });
    }

    // Internal bundles load into the kernel, not the public catalog; listing them
    // here makes them visible and immutable like the presets.
    for identity in hya_bundle::INTERNAL_BUNDLES {
        let Some(catalog) = load_enabled(identity)? else {
            continue;
        };
        let bundle = single_bundle(catalog.bundles(), identity)?;
        presets.push(TrustedPresetDescriptor {
            id: bundle.identity().id.clone(),
            kind: bundle.kind().as_str().to_string(),
            version: bundle.identity().version.clone(),
            digest: catalog.digest().to_string(),
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: Vec::new(),
        });
    }
    presets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(presets)
}

fn tool_resource_ids(bundle: &Bundle) -> Result<Vec<String>, BundleError> {
    if !bundle.tools().is_empty() {
        return Ok(bundle
            .tools()
            .iter()
            .map(|tool| tool.local_id.clone())
            .collect());
    }
    let Some(exposure) = bundle
        .extensions()
        .iter()
        .find(|asset| asset.local_id == "exposure")
    else {
        return Ok(Vec::new());
    };
    let parsed: ExposureHeader = serde_norway::from_str(&exposure.content).map_err(|error| {
        BundleError::InvalidManifest {
            source_name: bundle.identity().id.clone(),
            detail: format!(
                "invalid tool descriptor asset `{}`: {error}",
                exposure.source_path
            ),
        }
    })?;
    Ok(parsed.tools.into_iter().map(|tool| tool.name).collect())
}

fn single_bundle<'a>(bundles: &'a [Bundle], identity: &str) -> Result<&'a Bundle, BundleError> {
    bundles.first().ok_or_else(|| BundleError::InvalidManifest {
        source_name: identity.to_string(),
        detail: "trusted preset catalog is empty".to_string(),
    })
}
