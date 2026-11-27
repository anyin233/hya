//! Resident delivery over an existing team-root projection.

use hya_bundle::{ChannelCapability, ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{ChannelKind, MailEndpoint, Projection, scope};
use hya_tool::extension::{MailMessage, TeamProjection, team_projection};

use crate::ChannelPolicy;

pub(super) struct Inbox<'a> {
    team: TeamProjection,
    handle: &'a str,
}

impl<'a> Inbox<'a> {
    pub(super) fn new(projection: &'a Projection, handle: &'a str) -> Self {
        Self {
            team: team_projection(projection),
            handle,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.team.inboxes.get(self.handle).map_or(0, Vec::len)
    }

    pub(super) fn durable_cursor(&self) -> Option<usize> {
        self.team
            .roster
            .get(self.handle)
            .map(|entry| usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX))
    }

    pub(super) fn incoming(&self, cursor: usize) -> impl Iterator<Item = &MailMessage> {
        self.team
            .unread_incoming_mail(self.handle, cursor as u64)
    }

    pub(super) fn resident_mail<'b>(
        &'b self,
        cursor: usize,
        agent: &'b str,
        policy: Option<&'b ChannelPolicy>,
    ) -> impl Iterator<Item = &'b MailMessage> + 'b {
        self.incoming(cursor).filter(move |message| {
            if message.from == scope::HARNESS_HANDLE {
                return true;
            }
            let (kind, role) =
                recipient_channel_role(&self.team, &message.to, self.handle, &message.from);
            policy.is_some_and(|policy| resident_delivery_allowed(policy, agent, kind, role))
        })
    }
}

pub(super) fn recipient_channel_role(
    team: &TeamProjection,
    to: &MailEndpoint,
    recipient: &str,
    sender: &str,
) -> (ChannelTemplateKind, Option<ChannelParticipantRole>) {
    if let MailEndpoint::Channel(channel) = to
        && let Some(state) = team.channels.get(channel)
        && state.kind == ChannelKind::Group
    {
        return (
            ChannelTemplateKind::Unit,
            Some(if state.unit.as_deref() == Some(recipient) {
                ChannelParticipantRole::UnitLeader
            } else {
                ChannelParticipantRole::DirectReports
            }),
        );
    }
    let role = if scope::parent_path(recipient) == Some(sender) {
        Some(ChannelParticipantRole::Child)
    } else if scope::parent_path(sender) == Some(recipient) {
        Some(ChannelParticipantRole::Parent)
    } else {
        None
    };
    let own_dm = match to {
        MailEndpoint::Handle(_) => false,
        MailEndpoint::Channel(channel) => team.channels.get(channel).is_some_and(|state| {
            state.kind == ChannelKind::Dm
                && state.members.len() == 2
                && state.members.contains(recipient)
                && state.members.contains(sender)
        }),
    };
    (ChannelTemplateKind::ParentDm, role.filter(|_| own_dm))
}

pub(super) fn resident_delivery_allowed(
    policy: &ChannelPolicy,
    agent: &str,
    kind: ChannelTemplateKind,
    role: Option<ChannelParticipantRole>,
) -> bool {
    let Some(role) = role else { return false };
    // The direct parent's own DM is the member's mandatory work address.
    if kind == ChannelTemplateKind::ParentDm && role == ChannelParticipantRole::Child {
        return true;
    }
    [ChannelCapability::ResidentMail, ChannelCapability::FollowUp]
        .into_iter()
        .all(|capability| match kind {
            ChannelTemplateKind::Unit => policy.allows_unit(capability, agent, role),
            ChannelTemplateKind::ParentDm => policy.allows_parent_dm(capability, agent, role),
        })
}
