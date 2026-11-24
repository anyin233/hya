use hya_context::rewrite::RewriteCtx;
use hya_proto::{
    AgentName, Message, MessageId, ModelRef, PartProjection, Projection, Role, SessionId,
};

use super::turn::rewrite_host::{EpisodeTranscript, HostErrors, TurnLog, TurnModels, model_window};
use super::{SessionEngine, projection_workdir};
use crate::error::CoreError;

use hya_provider::COMPACT_CONTEXT_MARKER;

impl SessionEngine {
    /// Reduce the session's current episode now: the internal bundles'
    /// extensions (`hya/compaction` tries the route's native compaction),
    /// otherwise the kernel's own Rewrite. Returns the marker.
    pub async fn summarize_session(&self, session: SessionId) -> Result<MessageId, CoreError> {
        let _lease = self
            .begin_turn(session, &tokio_util::sync::CancellationToken::new())
            .await?
            .ok_or(CoreError::Cancelled)?;
        let projection = self.store.read_projection_shared(session).await?;
        // Empty projection has no session id; fail before the workdir lookup.
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_string()));
        }
        let workdir = projection_workdir(&projection).ok_or_else(|| {
            CoreError::Invalid("session workdir required for summarize_session".to_string())
        })?;
        let model = projection
            .session
            .model
            .clone()
            .unwrap_or_else(|| ModelRef::new("unknown"));
        let errors = HostErrors::default();
        let config = self.rewriter_config();
        let log = TurnLog {
            engine: self,
            actor_claim: None,
            errors: &errors,
        };
        let router = self.provider_router();
        let models = TurnModels {
            router: &router,
            errors: &errors,
        };
        // The kernel's Rewrite keeps the newest turn's user message; anchored
        // on a later user or marker message it would cut that message's own
        // parts.
        let message = projection
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(|message| message.id)
            .ok_or_else(|| CoreError::Invalid("nothing to summarize".to_string()))?;
        let cx = RewriteCtx {
            session,
            message,
            model: &model,
            projection: &projection,
            window: model_window(&router, &model),
            overhead_tokens: 0,
            pressure: None,
            workdir: &workdir,
            config: &config,
            log: &log,
            transcript: &EpisodeTranscript,
            models: Some(&models),
            hooks: Some(self),
        };
        let changed = hya_core::kernel::rewrite_now(cx, &self.rewriters).await;
        if !changed.map_err(|error| errors.into_error(error))? {
            return Err(CoreError::Invalid("nothing to summarize".to_string()));
        }
        // The marker is the newest system message: the turn lease keeps
        // every other writer out.
        self.store
            .read_projection_shared(session)
            .await?
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::System)
            .map(|message| message.id)
            .ok_or_else(|| CoreError::Invalid("summary marker missing".to_string()))
    }
}

/// Handoffs describe the whole current episode, including its local
/// compactions. The earlier handoff is supplied separately as the anchor.
pub(crate) fn episode_messages(projection: &Projection) -> &[hya_proto::MessageProjection] {
    let prefix = format!("{COMPACT_CONTEXT_MARKER}\nPRIOR HANDOFF (generation ");
    let start = projection
        .session
        .messages
        .iter()
        .rposition(|message| {
            message.role == Role::System && collect_text(&message.parts).starts_with(&prefix)
        })
        .map_or(0, |index| index + 1);
    &projection.session.messages[start..]
}

pub(crate) fn summary_messages(projection: &Projection) -> Result<Vec<Message>, CoreError> {
    if projection.session.id.is_none() {
        return Err(CoreError::Invalid("session not found".to_string()));
    }
    let agent = projection
        .session
        .agent
        .clone()
        .unwrap_or_else(|| AgentName::new(crate::MAIN_AGENT_ID));
    let model = projection
        .session
        .model
        .clone()
        .unwrap_or_else(|| ModelRef::new("unknown"));
    Ok(episode_messages(projection)
        .iter()
        .map(|message| match message.role {
            Role::User => Message::User {
                id: message.id,
                parts: super::turn::messages::map_parts(&message.parts),
            },
            Role::Assistant => Message::Assistant {
                id: message.id,
                agent: agent.clone(),
                model: model.clone(),
                parts: super::turn::messages::map_parts(&message.parts),
                finish: message.finish,
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
            Role::System => Message::System {
                id: message.id,
                content: collect_text(&message.parts),
            },
        })
        .collect())
}

fn collect_text(parts: &[PartProjection]) -> String {
    let mut out = String::new();
    for part in parts {
        if let PartProjection::Text { text, .. } = part {
            out.push_str(text);
        }
    }
    out
}
