use hya_proto::{ActorClaim, MailEndpoint, MailKind, SessionId, scope};
use hya_tool::MailReceipt;
use hya_tool::extension::team_projection;

use crate::engine::SessionEngine;
use crate::error::CoreError;
use crate::store_bridge::SessionStoreExt as _;

impl SessionEngine {
    /// Send mail from `from_session` to a handle or `#channel`. Appends a single
    /// `MailSent` to the team-root log; the reducer fans a channel send out to
    /// every current subscriber. Returns a receipt with the resolved sender handle
    /// and the recipient count at send time.
    ///
    /// Public so callers outside the mailbox service (the resident supervisor's
    /// tests, integration drivers) can inject team mail directly; the normal path
    /// is still the `MailboxPlane` → [`run_mailbox_service`](crate::run_mailbox_service).
    pub async fn mail_send(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
    ) -> Result<MailReceipt, CoreError> {
        let policy = self.session_channel_policy(from_session).ok_or_else(|| {
            CoreError::Invalid("channel policy snapshot missing for session send".to_string())
        })?;
        self.mail_send_for_actor_with_policy(from_session, to, kind, body, None, Some(policy))
            .await
    }

    pub(crate) async fn mail_send_for_actor_with_policy(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        self.mail_send_checked(
            from_session,
            to,
            kind,
            body,
            actor_claim,
            channel_policy,
            false,
        )
        .await
    }

    /// The mail send. `user_steer`: the user, not the sending agent, wrote
    /// this mail, so a target the user stopped accepts it (it stays held
    /// until the caller lifts the stop).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn mail_send_checked(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
        user_steer: bool,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        let policy_projection = self.read_projection_shared(root).await?;
        let team = team_projection(&policy_projection);
        let channel = match to {
            MailEndpoint::Channel(channel) => channel.trim().to_string(),
            MailEndpoint::Handle(handle) => team
                .resolve_dm(&from, &handle)
                .ok_or_else(|| CoreError::MailAuthorizationRejected {
                    sender: from.clone(),
                    channel: format!("direct:{handle}"),
                    reason:
                        "no DM channel with that target; run `list_channel` to see your channels"
                            .into(),
                })?,
        };
        let to = MailEndpoint::Channel(channel.clone());
        // A subagent the user stopped (`cancel_user_turn`) is not restarted
        // by mail from above it: the user decides when it continues.
        if !user_steer && let Some(reviver) = self.archive_reviver() {
            let peer = team
                .channels
                .get(&channel)
                .filter(|state| {
                    state.kind == hya_proto::ChannelKind::Dm && state.members.contains(&from)
                })
                .and_then(|state| state.members.iter().find(|member| **member != from))
                .cloned();
            if let Some(peer) = peer
                && peer
                    .strip_prefix(from.as_str())
                    .is_some_and(|rest| rest.starts_with(scope::PATH_SEPARATOR))
                && reviver.stopped_by_user(root, &peer)
            {
                return Err(CoreError::Invalid(format!(
                    "the user stopped `{peer}`; it stays stopped until the user prompts or steers it. Do not try to restart it: tell the user what you need from it instead"
                )));
            }
        }
        drop(policy_projection);
        #[cfg(test)]
        if let Some(gate) = self.mail_pre_append_gate.as_ref() {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let append = self
            .store()
            .append_channel_mail(
                root,
                actor_claim,
                from.clone(),
                channel.clone(),
                kind,
                body.clone(),
                channel_policy,
            )
            .await;
        let (envelope, recipients) = match append {
            Ok(result) => result,
            Err(ref error @ CoreError::MailRevivalRequired { ref target, .. }) => {
                let Some(supervisor) = self.resident_supervisor() else {
                    return Err(error.clone());
                };
                let recipients = supervisor
                    .revive_episode(
                        root,
                        &from,
                        target,
                        channel.clone(),
                        kind,
                        body,
                        actor_claim,
                        channel_policy,
                    )
                    .await?;
                return Ok(MailReceipt {
                    from,
                    to,
                    recipients,
                });
            }
            Err(error) => return Err(error),
        };
        self.publish_envelope(envelope);
        Ok(MailReceipt {
            from,
            to,
            recipients,
        })
    }

    /// Post a one-way announcement to the unit the acting agent leads (R6).
    ///
    /// Reaches direct reports only. A whole-swarm announcement costs one call per
    /// level, each made deliberately by that level's leader.
    pub async fn mail_announce(
        &self,
        from_session: SessionId,
        body: String,
    ) -> Result<MailReceipt, CoreError> {
        let policy = self.session_channel_policy(from_session).ok_or_else(|| {
            CoreError::Invalid("channel policy snapshot missing for session broadcast".to_string())
        })?;
        self.mail_announce_for_actor_with_policy(from_session, body, None, Some(policy))
            .await
    }

    async fn mail_announce_for_actor_with_policy(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        // ADR-0016: broadcast rides the unit's minted group channel
        // (`announce-{8}`, leader-only posting).
        let projection = self.read_projection_shared(root).await?;
        let Some(group) = team_projection(&projection)
            .channels
            .iter()
            .find(|(_, channel)| {
                channel.kind == hya_proto::ChannelKind::Group
                    && channel.unit.as_deref() == Some(from.as_str())
            })
            .map(|(key, _)| key.clone())
        else {
            return Err(CoreError::Invalid(
                "you do not lead a unit yet; broadcast reaches your direct reports only"
                    .to_string(),
            ));
        };
        drop(projection);
        let (envelope, recipients) = self
            .store()
            .append_channel_mail(
                root,
                actor_claim,
                from.clone(),
                group.clone(),
                MailKind::Announcement,
                body,
                channel_policy,
            )
            .await?;
        self.publish_envelope(envelope);
        Ok(MailReceipt {
            to: MailEndpoint::Channel(group),
            from,
            recipients,
        })
    }

    pub(crate) async fn mail_send_default_for_actor_with_policy(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        let projection = self.read_projection_shared(root).await?;
        let leads_unit = team_projection(&projection)
            .channels
            .iter()
            .any(|(_, channel)| {
                channel.kind == hya_proto::ChannelKind::Group
                    && channel.unit.as_deref() == Some(from.as_str())
            });
        drop(projection);
        if leads_unit {
            return self
                .mail_announce_for_actor_with_policy(
                    from_session,
                    body,
                    actor_claim,
                    channel_policy,
                )
                .await;
        }
        if let Some(parent) = hya_proto::scope::parent_path(&from) {
            return self
                .mail_send_for_actor_with_policy(
                    from_session,
                    MailEndpoint::Handle(parent.to_string()),
                    MailKind::Message,
                    body,
                    actor_claim,
                    channel_policy,
                )
                .await;
        }
        Err(CoreError::Invalid(
            "no default channel: you neither lead a unit nor report to one; address a channel explicitly with `#channel` or a peer handle".to_string(),
        ))
    }
}
