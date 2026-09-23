use hya_proto::{
    Message, MessageId, ModelRef, Part, PartId, PartProjection, Role, SessionId, UsagePurpose,
};
use hya_provider::CompletionRequest;

use super::SessionEngine;
use super::{
    FixedSystemAgent, fixed_system_agent, projection_workdir, summarize_options_from_definition,
};
use crate::error::CoreError;
use crate::title;

impl SessionEngine {
    /// Generate and set an automatic title for a new session.
    pub async fn auto_title_session(
        &self,
        session: SessionId,
        fallback_model: &ModelRef,
    ) -> Result<bool, CoreError> {
        let projection = self.store.read_projection(session).await?;
        if projection.session.id.is_none() || projection.session.parent.is_some() {
            return Ok(false);
        }
        if let Some(current) = projection.session.title.as_deref()
            && !title::is_default_or_fallback_title(current)
        {
            return Ok(false);
        }
        let Some(user_text) = only_user_text(&projection.session.messages) else {
            return Ok(false);
        };
        let workdir = projection_workdir(&projection).ok_or_else(|| {
            CoreError::Invalid("session workdir required for title generation".to_string())
        })?;
        // Bind once from the persisted session workdir; exact-lookup only.
        let binding = self.bind_session_runtime(session, &workdir).await?;
        let definition = fixed_system_agent(&binding, FixedSystemAgent::Title)?;
        let options = summarize_options_from_definition(
            &definition,
            &self.model_categories,
            binding.agent_model_preference(definition.stable_id),
            &|model| self.provider_router().resolve(model).is_some(),
        );
        let model = options
            .model
            .clone()
            .unwrap_or_else(|| fallback_model.clone());
        let usage = crate::compaction::UsageCollector::default();
        let generated = self
            .generate_title(
                &model,
                options.system,
                options.reasoning,
                &user_text,
                &usage,
            )
            .await;
        // Title generation is billed to the session it names.
        self.record_side_call_usage(None, session, UsagePurpose::Title, &usage)
            .await;
        let generated = generated?;
        let Some(title) = title::clean_title_output(&generated) else {
            return Ok(false);
        };
        self.set_title(session, title).await?;
        Ok(true)
    }

    async fn generate_title(
        &self,
        model: &ModelRef,
        system: Option<String>,
        reasoning: Option<hya_provider::ReasoningEffort>,
        user_text: &str,
        usage: &crate::compaction::UsageCollector,
    ) -> Result<String, CoreError> {
        let request = CompletionRequest {
            model: model.clone(),
            system,
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: user_text.to_string(),
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(128),
            reasoning,
            headers: Default::default(),
        };
        let stream = self
            .provider_router()
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let (output, reported) = crate::compaction::collect_text_and_usage(stream).await;
        if let Some(tokens) = reported.filter(|tokens| !tokens.is_zero()) {
            usage.record(model.clone(), tokens);
        }
        Ok(output?)
    }
}

fn only_user_text(messages: &[hya_proto::MessageProjection]) -> Option<String> {
    let mut text = None;
    for message in messages {
        match message.role {
            Role::User => {
                if text.is_some() {
                    return None;
                }
                text = Some(parts_text(&message.parts));
            }
            Role::Assistant | Role::System => {}
        }
    }
    text
}

fn parts_text(parts: &[PartProjection]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            PartProjection::Text { text, .. } => out.push_str(text),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => {}
        }
    }
    out
}
