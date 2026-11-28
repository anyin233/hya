//! Runtime evaluation of declarative AgentSet channel policies (ADR-0016, ADR-0039).

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use hya_bundle::{
    BundleCatalog, BundleError, ChannelCapability, ChannelParticipantRole, ChannelTemplateKind,
    PreparedChannelParticipant, PreparedChannelTemplate, first_party_bundle_optional,
};
use hya_proto::{ChannelKind, ChannelPolicySnapshot, MailEndpoint, scope};
use hya_tool::extension::TeamProjection;

/// Stable identity of the trusted first-party `hya/agent-channels` bundle.
pub const AGENT_CHANNELS_PRESET_ID: &str = "hya/agent-channels";

/// Bit index for [`ChannelCapability::Send`] in [`ChannelPolicySnapshot`].
pub const BIT_SEND: u8 = 1 << 0;
/// Bit index for [`ChannelCapability::Report`] in [`ChannelPolicySnapshot`].
pub const BIT_REPORT: u8 = 1 << 1;
/// Bit index for [`ChannelCapability::Steer`] in [`ChannelPolicySnapshot`].
pub const BIT_STEER: u8 = 1 << 2;
/// Bit index for [`ChannelCapability::FollowUp`] in [`ChannelPolicySnapshot`].
pub const BIT_FOLLOW_UP: u8 = 1 << 3;
/// Bit index for [`ChannelCapability::ResidentMail`] in [`ChannelPolicySnapshot`].
pub const BIT_RESIDENT_MAIL: u8 = 1 << 4;

/// Trusted defaults plus installed bundle restrictions for channel operations.
#[derive(Clone, Debug)]
pub struct ChannelPolicy {
    unit_defaults: BTreeSet<ChannelCapability>,
    parent_dm_defaults: BTreeSet<ChannelCapability>,
    restrictions: Vec<PreparedChannelTemplate>,
}

impl ChannelPolicy {
    /// Empty policy with zero channel capabilities (bare mode / no `hya/agent-channels`).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            unit_defaults: BTreeSet::new(),
            parent_dm_defaults: BTreeSet::new(),
            restrictions: Vec::new(),
        }
    }

    /// Compile policy from the trusted first-party preset and installed declarations.
    ///
    /// # Errors
    /// Returns [`BundleError`] if the trusted first-party `hya/agent-channels`
    /// manifest is malformed.
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

    /// Check a unit-group operation for an acting agent and topology role.
    #[must_use]
    pub fn allows_unit(
        &self,
        capability: ChannelCapability,
        agent_type: &str,
        role: ChannelParticipantRole,
    ) -> bool {
        self.allows(ChannelTemplateKind::Unit, capability, agent_type, role)
    }

    /// Check a parent-child DM operation for an acting agent and topology role.
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

/// Check whether `policy` permits `capability_bit` for the given channel kind and role.
#[must_use]
pub fn snapshot_allows_bit(
    policy: ChannelPolicySnapshot,
    kind: ChannelTemplateKind,
    role: ChannelParticipantRole,
    capability_bit: u8,
) -> bool {
    let bits = match (kind, role) {
        (ChannelTemplateKind::Unit, ChannelParticipantRole::UnitLeader) => policy.unit_leader,
        (ChannelTemplateKind::Unit, ChannelParticipantRole::DirectReports) => policy.unit_member,
        (ChannelTemplateKind::ParentDm, ChannelParticipantRole::Parent) => policy.dm_parent,
        (ChannelTemplateKind::ParentDm, ChannelParticipantRole::Child) => policy.dm_child,
        _ => 0,
    };
    bits & capability_bit != 0
}

/// Check whether `policy` permits in-turn mail steering for `(kind, role)`.
#[must_use]
pub fn snapshot_allows_steer(
    policy: ChannelPolicySnapshot,
    kind: ChannelTemplateKind,
    role: ChannelParticipantRole,
) -> bool {
    snapshot_allows_bit(policy, kind, role, BIT_STEER)
}

/// Compute `handle`'s channel memberships and `(ChannelTemplateKind, ChannelParticipantRole)`
/// map from `team`.
#[must_use]
pub fn channel_topology(
    team: &TeamProjection,
    handle: &str,
) -> (
    Vec<String>,
    HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
) {
    let channels: Vec<String> = team
        .channels
        .iter()
        .filter(|(_, channel)| channel.members.contains(handle))
        .map(|(key, _)| key.clone())
        .collect();
    let mut channel_roles = HashMap::new();
    for (id, channel) in &team.channels {
        if channel.members.contains(handle) {
            let topology = if channel.kind == ChannelKind::Group {
                (
                    ChannelTemplateKind::Unit,
                    if channel.unit.as_deref() == Some(handle) {
                        ChannelParticipantRole::UnitLeader
                    } else {
                        ChannelParticipantRole::DirectReports
                    },
                )
            } else {
                (
                    ChannelTemplateKind::ParentDm,
                    channel
                        .members
                        .iter()
                        .find(|member| member.as_str() != handle)
                        .map_or(ChannelParticipantRole::Child, |peer| {
                            if scope::parent_path(peer) == Some(handle) {
                                ChannelParticipantRole::Parent
                            } else {
                                ChannelParticipantRole::Child
                            }
                        }),
                )
            };
            channel_roles.insert(id.clone(), topology);
        }
    }
    (channels, channel_roles)
}

/// Whether `handle` is allowed to receive in-turn steer notices for a message
/// from `from` addressed to `to` under `policy` and `channel_roles`.
#[must_use]
pub fn allows_steer_message(
    policy: Option<ChannelPolicySnapshot>,
    channel_roles: &HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
    from: &str,
    to: &MailEndpoint,
) -> bool {
    // Harness mail (e.g. leader-failed wrap-up notice) is a control message,
    // never filtered by the channel policy.
    if from == scope::HARNESS_HANDLE {
        return true;
    }
    let Some(policy) = policy else {
        return false;
    };
    let MailEndpoint::Channel(channel) = to else {
        return false;
    };
    let Some(&(kind, role)) = channel_roles.get(channel) else {
        return false;
    };
    snapshot_allows_steer(policy, kind, role)
}

/// Resolve the recipient handles of a `MailSent` message excluding `from`.
#[must_use]
pub fn mail_recipient_handles(team: &TeamProjection, from: &str, to: &MailEndpoint) -> Vec<String> {
    match to {
        MailEndpoint::Handle(handle) => vec![handle.clone()],
        MailEndpoint::Channel(channel) => team
            .channels
            .get(channel)
            .map(|ch| {
                ch.members
                    .iter()
                    .filter(|member| member.as_str() != from)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    }
}

type TrustedDefaults = (BTreeSet<ChannelCapability>, BTreeSet<ChannelCapability>);

fn trusted_defaults() -> Result<&'static TrustedDefaults, BundleError> {
    static DEFAULTS: OnceLock<Result<TrustedDefaults, String>> = OnceLock::new();
    DEFAULTS
        .get_or_init(|| {
            let Some(preset) = first_party_bundle_optional(AGENT_CHANNELS_PRESET_ID)
                .map_err(|error| error.to_string())?
            else {
                return Ok((BTreeSet::new(), BTreeSet::new()));
            };
            let channels = preset
                .bundles()
                .first()
                .map_or(&[][..], hya_bundle::Bundle::channels);
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
