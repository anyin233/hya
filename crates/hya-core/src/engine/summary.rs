use hya_proto::{
    AgentName, CompactionStrategy, Event, Message, MessageId, ModelRef, PartProjection, Projection,
    Role, SessionId, UsagePurpose,
};

use super::SessionEngine;
use super::{
    FixedSystemAgent, fixed_system_agent, projection_workdir, summarize_options_from_definition,
};
use crate::error::CoreError;

use hya_provider::COMPACT_CONTEXT_MARKER;

impl SessionEngine {
    /// Produce a summary message for the session via the summarizer.
    pub async fn summarize_session(&self, session: SessionId) -> Result<MessageId, CoreError> {
        let _lease = self
            .begin_turn(session, &tokio_util::sync::CancellationToken::new())
            .await?
            .ok_or(CoreError::Cancelled)?;
        let projection = self.store.read_projection(session).await?;
        // Empty projection has no session id; fail before workdir/catalog lookup.
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_string()));
        }
        let workdir = projection_workdir(&projection).ok_or_else(|| {
            CoreError::Invalid("session workdir required for summarize_session".to_string())
        })?;
        // Bind once from the persisted session workdir; exact-lookup only.
        let binding = self.bind_session_runtime(session, &workdir).await?;
        let definition = fixed_system_agent(&binding, FixedSystemAgent::Summary)?;
        let options = summarize_options_from_definition(
            &definition,
            &self.model_categories,
            binding.agent_model_preference(definition.stable_id),
            &|model| self.provider_router().resolve(model).is_some(),
        );
        let messages = summary_messages(&projection)?;
        let usage = crate::compaction::UsageCollector::default();
        let options = crate::compaction::SummarizeOptions {
            previous_summary: crate::compaction::previous_summary(&messages).or_else(|| {
                projection
                    .session
                    .handoff
                    .as_ref()
                    .map(|handoff| handoff.doc.clone())
            }),
            max_output_tokens: Some(self.compaction.summary_max_tokens),
            usage: Some(usage.clone()),
            ..options
        };
        let summarizer = self
            .summarizer
            .as_ref()
            .ok_or_else(|| CoreError::Invalid("summarizer not configured".to_string()))?;
        let summary = summarizer.summarize(&messages, options).await;
        self.record_side_call_usage(None, session, UsagePurpose::Compaction, &usage)
            .await;
        let summary = summary?;
        // Behind the marker, not merely appended. Without it `compacted_messages`
        // never finds a cut point, so the summarized history stays in the
        // transcript and `/compact` *grows* the context it was asked to shrink.
        let folded = messages.first().zip(messages.last()).map(|(first, last)| {
            (
                first.id(),
                last.id(),
                u32::try_from(messages.len()).unwrap_or(u32::MAX),
            )
        });
        let marker = self
            .commit_compaction(
                None,
                session,
                format!("{COMPACT_CONTEXT_MARKER}\n{summary}"),
                CompactionStrategy::LocalSummarizer,
                folded,
                u64::try_from(self.token_accounting.estimate(&messages)).unwrap_or(u64::MAX),
                0,
            )
            .await?;
        Ok(marker)
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

impl SessionEngine {
    /// Persist the marker and its fold range before publishing either.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn commit_compaction(
        &self,
        actor_claim: Option<&hya_store::ActorClaim>,
        session: SessionId,
        content: String,
        strategy: CompactionStrategy,
        folded: Option<(MessageId, MessageId, u32)>,
        input_tokens_est: u64,
        threshold: u64,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = hya_proto::PartId::new();
        let mut events = vec![
            Event::MessageStarted {
                session,
                message,
                role: Role::System,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: content,
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::System,
                finish: hya_proto::FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        if let Some((from_message, to_message, folded_count)) = folded {
            events.push(Event::ContextCompacted {
                session,
                message,
                strategy,
                from_message,
                to_message,
                folded_count,
                input_tokens_est,
                threshold,
            });
        }
        self.emit_batch_for_actor(actor_claim, session, events)
            .await?;
        Ok(message)
    }
}
