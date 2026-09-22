//! Runtime evaluation of declarative AgentSet channel restrictions.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, ChannelCapability, ChannelParticipantRole,
    ChannelTemplateKind, PreparedChannelParticipant, PreparedChannelTemplate, SourceFile,
    prepare_package,
};
use hya_tool::ChannelPolicySnapshot;

use crate::TurnBinding;

/// Stable identity of the trusted first-party channel-policy preset.
pub const AGENT_CHANNELS_PRESET_ID: &str = "hya/agent-channels";

const PRESET_SOURCE: &str = include_str!("../../../bundles/presets/agent-channels/bundle.yaml");

/// Trusted defaults plus installed bundle restrictions for channel operations.
#[derive(Clone, Debug)]
pub struct ChannelPolicy {
    unit_defaults: BTreeSet<ChannelCapability>,
    parent_dm_defaults: BTreeSet<ChannelCapability>,
    restrictions: Vec<PreparedChannelTemplate>,
}

impl ChannelPolicy {
    /// Compile policy from the trusted embedded preset and installed declarations.
    pub fn from_bundle_catalog(catalog: &BundleCatalog) -> Result<Self, BundleError> {
        let (unit_defaults, parent_dm_defaults) = trusted_defaults()?;
        let restrictions = catalog
            .bundles()
            .iter()
            .flat_map(|bundle| bundle.channels().iter().cloned())
            .collect();
        Ok(Self {
            unit_defaults: unit_defaults.clone(),
            parent_dm_defaults: parent_dm_defaults.clone(),
            restrictions,
        })
    }

    /// Freeze this policy for one admitted actor so tool requests remain binding-pinned.
    #[must_use]
    pub fn snapshot_for(&self, agent_type: &str) -> ChannelPolicySnapshot {
        ChannelPolicySnapshot {
            unit_leader: self.bits_for(
                ChannelTemplateKind::Unit,
                agent_type,
                ChannelParticipantRole::UnitLeader,
            ),
            unit_member: self.bits_for(
                ChannelTemplateKind::Unit,
                agent_type,
                ChannelParticipantRole::DirectReports,
            ),
            dm_parent: self.bits_for(
                ChannelTemplateKind::ParentDm,
                agent_type,
                ChannelParticipantRole::Parent,
            ),
            dm_child: self.bits_for(
                ChannelTemplateKind::ParentDm,
                agent_type,
                ChannelParticipantRole::Child,
            ),
        }
    }

    fn bits_for(
        &self,
        kind: ChannelTemplateKind,
        agent_type: &str,
        role: ChannelParticipantRole,
    ) -> u8 {
        [
            ChannelCapability::Send,
            ChannelCapability::Report,
            ChannelCapability::Steer,
            ChannelCapability::FollowUp,
            ChannelCapability::ResidentMail,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |bits, (index, capability)| {
            if self.allows(kind, capability, agent_type, role) {
                bits | (1 << index)
            } else {
                bits
            }
        })
    }

    /// Compile policy from the immutable catalog retained by an admitted turn.
    pub fn from_binding(binding: &TurnBinding) -> Result<Self, BundleError> {
        Self::from_bundle_catalog(binding.bundle_catalog())
    }

    /// Check a unit-group operation for an acting Agent and topology role.
    #[must_use]
    pub fn allows_unit(
        &self,
        capability: ChannelCapability,
        agent_type: &str,
        role: ChannelParticipantRole,
    ) -> bool {
        self.allows(ChannelTemplateKind::Unit, capability, agent_type, role)
    }

    /// Check a parent-child DM operation for an acting Agent and topology role.
    #[must_use]
    pub fn allows_parent_dm(
        &self,
        capability: ChannelCapability,
        agent_type: &str,
        role: ChannelParticipantRole,
    ) -> bool {
        self.allows(ChannelTemplateKind::ParentDm, capability, agent_type, role)
    }

    fn allows(
        &self,
        kind: ChannelTemplateKind,
        capability: ChannelCapability,
        agent_type: &str,
        role: ChannelParticipantRole,
    ) -> bool {
        let defaults = match kind {
            ChannelTemplateKind::Unit => &self.unit_defaults,
            ChannelTemplateKind::ParentDm => &self.parent_dm_defaults,
        };
        defaults.contains(&capability)
            && self
                .restrictions
                .iter()
                .filter(|template| {
                    template.kind == kind
                        && template
                            .participants
                            .iter()
                            .any(|participant| match participant {
                                PreparedChannelParticipant::Agent { agent } => agent == agent_type,
                                PreparedChannelParticipant::Role { role: selected } => {
                                    *selected == role
                                }
                            })
                })
                .all(|template| template.capabilities.contains(&capability))
    }
}

type TrustedDefaults = (BTreeSet<ChannelCapability>, BTreeSet<ChannelCapability>);

fn trusted_defaults() -> Result<&'static TrustedDefaults, BundleError> {
    static DEFAULTS: OnceLock<Result<TrustedDefaults, String>> = OnceLock::new();
    DEFAULTS
        .get_or_init(|| {
            let preset = prepare_package(BundleSource::new(
                AGENT_CHANNELS_PRESET_ID,
                vec![SourceFile::new("bundle.yaml", PRESET_SOURCE)],
            ))
            .map_err(|error| error.to_string())?;
            let channels = preset
                .bundles()
                .first()
                .map_or(&[][..], hya_bundle::PreparedInstallableBundle::channels);
            Ok((
                default_capabilities(channels, ChannelTemplateKind::Unit)
                    .map_err(|error| error.to_string())?,
                default_capabilities(channels, ChannelTemplateKind::ParentDm)
                    .map_err(|error| error.to_string())?,
            ))
        })
        .as_ref()
        .map_err(|detail| BundleError::InvalidManifest {
            source_name: AGENT_CHANNELS_PRESET_ID.to_string(),
            detail: detail.clone(),
        })
}

fn default_capabilities(
    templates: &[PreparedChannelTemplate],
    kind: ChannelTemplateKind,
) -> Result<BTreeSet<ChannelCapability>, BundleError> {
    templates
        .iter()
        .find(|template| template.kind == kind)
        .map(|template| template.capabilities.iter().copied().collect())
        .ok_or_else(|| BundleError::InvalidManifest {
            source_name: AGENT_CHANNELS_PRESET_ID.to_string(),
            detail: format!("trusted preset is missing {kind:?}"),
        })
}
