//! Thin engine facade delegating channel list/read/status queries to
//! [`hya_agent_channels_core::AgentChannelsService`] through [`HostRuntime`].

use hya_agent_channels_core::{AgentChannelsService, ChannelHistoryRead};
use hya_proto::{ActorClaim, SessionId};
use hya_runtime_abi::HostError;

use crate::engine::SessionEngine;
use crate::error::CoreError;

fn map_host_error(error: HostError) -> CoreError {
    match error {
        HostError::InvalidInput(msg) | HostError::PermissionDenied(msg) => CoreError::Invalid(msg),
        HostError::StaleClaim(msg) => CoreError::Invalid(msg),
        other => CoreError::Invalid(other.to_string()),
    }
}

impl SessionEngine {
    /// `list_channel` rows for the acting agent (ADR-0016), delegated to `hya/agent-channels`.
    pub(crate) async fn team_channel_rows(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::ChannelRow>, CoreError> {
        let runtime = self.channel_host_runtime();
        AgentChannelsService::list_channels(runtime.as_ref(), session)
            .await
            .map_err(map_host_error)
    }

    /// Read one channel's recent history for the acting agent (ADR-0016),
    /// delegated to `hya/agent-channels`.
    pub async fn read_channel_history(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        channel: &str,
        last: Option<usize>,
    ) -> Result<ChannelHistoryRead, CoreError> {
        let runtime = self.channel_host_runtime();
        AgentChannelsService::read_channel_history(
            runtime.as_ref(),
            actor_claim.copied(),
            session,
            channel,
            last,
        )
        .await
        .map_err(map_host_error)
    }

    /// Direct-child liveness rows for the acting agent (ADR-0002), delegated to
    /// `hya/agent-channels`.
    pub async fn team_member_status(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::MemberStatusRow>, CoreError> {
        let runtime = self.channel_host_runtime();
        AgentChannelsService::team_status(runtime.as_ref(), session)
            .await
            .map_err(map_host_error)
    }
}
