//! Read-only inventory of trusted presets embedded by runtime crates.

use hya_bundle::{BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package};
use sha2::{Digest, Sha256};

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
    /// Presets are compiled into the executable and cannot be mutated in place.
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
/// Returns a bundle integrity error if embedded prepared bytes fail validation.
pub fn trusted_preset_inventory() -> Result<Vec<TrustedPresetDescriptor>, BundleError> {
    let core = hya_core::core_agents_preset()?;
    let channels = prepare_package(BundleSource::new(
        hya_core::AGENT_CHANNELS_PRESET_ID,
        vec![SourceFile::new(
            "bundle.yaml",
            include_str!("../../../bundles/presets/agent-channels/bundle.yaml"),
        )],
    ))?;
    let channel_bundle =
        channels
            .bundles()
            .first()
            .ok_or_else(|| BundleError::InvalidManifest {
                source_name: hya_core::AGENT_CHANNELS_PRESET_ID.to_string(),
                detail: "embedded preset catalog is empty".to_string(),
            })?;
    let base = hya_tool::base_tools_preset();
    let prepared_digest = format!("{:x}", Sha256::digest(base.prepared_catalog_bytes()));
    let base_catalog = PreparedCatalog::decode(base.prepared_catalog_bytes(), &prepared_digest)?;
    let base_bundle =
        base_catalog
            .bundles()
            .first()
            .ok_or_else(|| BundleError::InvalidManifest {
                source_name: base.identity().to_string(),
                detail: "embedded preset catalog is empty".to_string(),
            })?;

    let mut presets = vec![
        TrustedPresetDescriptor {
            id: base_bundle.identity().id.clone(),
            kind: base_bundle.kind().as_str().to_string(),
            version: base_bundle.identity().version.clone(),
            digest: prepared_digest,
            immutable: true,
            installable: false,
            agent_ids: Vec::new(),
            resource_ids: base
                .tools()
                .iter()
                .map(|tool| tool.name().to_string())
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
    ];
    presets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(presets)
}
