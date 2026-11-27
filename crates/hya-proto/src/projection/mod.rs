//! Shared idempotent reducer: fold an event log into a session view. Used by the
//! store (read path) and the client (SSE reconnect); idempotent by `EventSeq` so
//! re-delivered events are no-ops.

mod compaction;
mod extension;
mod helpers;
mod session;
mod snapshot;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{Envelope, Event};

pub use extension::{ProjectionReducer, ProjectionReducerRegistry};
pub use session::{
    MessageError, MessageProjection, PartProjection, SessionArchiveError, SessionProjection,
    session_archive_event,
};
pub use snapshot::PROJECTION_REDUCER_VERSION;

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(feature = "lifecycle-bench")]
impl Clone for Projection {
    fn clone(&self) -> Self {
        crate::bench::projection_clone(self);
        Self {
            session: self.session.clone(),
            extensions: self.extensions.clone(),
            last_seq: self.last_seq,
        }
    }
}

/// Full folded view: one session transcript plus per-bundle extension state.
#[cfg_attr(not(feature = "lifecycle-bench"), derive(Clone))]
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// Session transcript and metadata.
    pub session: SessionProjection,
    /// Per-bundle extension state folded from [`Event::Extension`] events,
    /// keyed by bundle id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
    /// Highest durable `EventSeq` applied; live `seq == 0` does not advance this.
    pub last_seq: u64,
}

impl Projection {
    /// Fold a slice of envelopes in order into a fresh projection without
    /// bundle extension reducers (unknown bundles preserve raw extension entries).
    #[must_use]
    pub fn from_events(envs: &[Envelope]) -> Self {
        Self::from_events_with(envs, &ProjectionReducerRegistry::new())
    }

    /// Fold a slice of envelopes in order into a fresh projection using `reducers`.
    #[must_use]
    pub fn from_events_with(envs: &[Envelope], reducers: &ProjectionReducerRegistry) -> Self {
        let mut p = Self::default();
        for e in envs {
            p.apply_with(e, reducers);
        }
        p
    }

    /// Apply one envelope without bundle extension reducers: `seq == 0` is
    /// live-only (no `last_seq` advance); `seq <= last_seq` is a durable no-op;
    /// otherwise fold and advance `last_seq`.
    pub fn apply(&mut self, env: &Envelope) {
        self.apply_with(env, &ProjectionReducerRegistry::new());
    }

    /// Apply one envelope using `reducers` for [`Event::Extension`].
    pub fn apply_with(&mut self, env: &Envelope, reducers: &ProjectionReducerRegistry) {
        if env.seq.0 == 0 {
            self.apply_event(&env.event, reducers);
            self.stamp_message_time(&env.event, env.ts_millis);
            return;
        }
        if env.seq.0 <= self.last_seq {
            return;
        }
        self.apply_event(&env.event, reducers);
        self.stamp_message_time(&env.event, env.ts_millis);
        self.last_seq = env.seq.0;
    }

    fn apply_event(&mut self, e: &Event, reducers: &ProjectionReducerRegistry) {
        match e {
            Event::SessionCreated {
                session,
                parent,
                agent,
                model,
                workdir,
                project,
                kind,
            } => {
                self.session.id = Some(*session);
                self.session.parent = *parent;
                self.session.agent = Some(agent.clone());
                self.session.model = Some(model.clone());
                self.session.workdir = Some(workdir.clone());
                self.session.project = *project;
                self.session.kind = *kind;
            }
            Event::InitialContextRecorded { text, .. } => {
                if self.session.initial_context.is_none() {
                    self.session.initial_context = Some(text.clone());
                }
            }
            Event::InitialContextInvalidated { .. } => {
                self.session.initial_context = None;
            }
            Event::SessionAgentModelOverrideSet { agent, model, .. } => match model {
                Some(model) => {
                    self.session
                        .agent_model_overrides
                        .insert(agent.as_str().to_string(), model.clone());
                }
                None => {
                    self.session.agent_model_overrides.remove(agent.as_str());
                }
            },
            Event::SessionPermissionModeSet { mode, .. } => {
                self.session.permission_mode = Some(mode.clone());
            }
            Event::SessionTitled { title, .. } => {
                self.session.title = Some(title.clone());
                self.session.ephemeral = false;
            }
            Event::SessionMetadataSet { metadata, .. } => {
                self.session.metadata = Some(metadata.clone());
            }
            Event::SessionArchived { archived, .. } => {
                self.session.archived = Some(archived.clone());
                self.session.ephemeral = false;
            }
            Event::SessionUnarchived { .. } => {
                self.session.archived = None;
            }
            Event::SessionEphemeralSet { ephemeral, .. } => {
                self.session.ephemeral = *ephemeral;
            }
            Event::AgentSwitched { agent, .. } => {
                self.session.agent = Some(agent.clone());
            }
            Event::ModelSwitched { model, .. } => {
                self.session.model = Some(model.clone());
            }
            Event::MessageStarted {
                message,
                role,
                agent,
                model,
                ..
            } => self.apply_message_started(message, role, agent, model),
            Event::StepStarted { message, .. } => self.apply_step_started(message),
            Event::TurnBindingRecorded {
                message,
                generation,
                ..
            } => self.apply_turn_binding_recorded(message, generation),
            Event::UserPromptContextRecorded {
                message,
                files,
                agents,
                ..
            } => self.apply_user_prompt_context_recorded(message, files, agents),
            Event::MessageFinished {
                message,
                finish,
                tokens,
                cause,
                ..
            } => self.apply_message_finished(message, finish, tokens, cause),
            Event::FilesChanged {
                message,
                call,
                files,
                ..
            } => self.apply_files_changed(message, call, files),
            Event::FilesObserved { files, .. } => self.apply_files_observed(files),
            Event::SessionReverted { message, files, .. } => {
                self.apply_revert(*message, files);
            }
            Event::SessionUnreverted { .. } => self.apply_session_unreverted(),
            Event::PartDeleted { message, part, .. } => self.apply_part_deleted(message, part),
            Event::TextStart { message, part, .. } => self.apply_text_start(message, part),
            Event::TextDelta {
                message,
                part,
                delta,
                ..
            } => self.apply_text_delta(message, part, delta),
            Event::TextReplace {
                message,
                part,
                text: replacement,
                ..
            } => self.apply_text_replace(message, part, replacement),
            Event::ReasoningStart {
                message,
                part,
                reason,
                ..
            } => self.apply_reasoning_start(message, part, reason),
            Event::ReasoningDelta {
                message,
                part,
                delta,
                ..
            } => self.apply_reasoning_delta(message, part, delta),
            Event::ReasoningReplace {
                message,
                part,
                text: replacement,
                ..
            } => self.apply_reasoning_replace(message, part, replacement),
            Event::ReasoningEnd {
                message,
                part,
                provider_data,
                ..
            } => self.apply_reasoning_end(message, part, provider_data),
            Event::ToolInputStart {
                message,
                part,
                call,
                name,
                ..
            } => self.apply_tool_input_start(message, part, call, name),
            Event::ToolCallRequested {
                message,
                part,
                call,
                name,
                input,
                provider_data,
                ..
            } => self.apply_tool_call_requested(message, part, call, name, input, provider_data),
            Event::ToolResult {
                message,
                part,
                output,
                time_ms,
                ..
            } => self.apply_tool_result(message, part, output, time_ms),
            Event::ToolError {
                message,
                part,
                message_text,
                value,
                ..
            } => self.apply_tool_error(message, part, message_text, value),
            Event::ToolPartUpdated {
                message,
                part,
                state: next,
                ..
            } => self.apply_tool_part_updated(message, part, next),
            Event::ContextCompacted {
                message: marker,
                to_message,
                step_cut,
                ..
            } => self.apply_context_compacted(marker, to_message, step_cut),
            Event::TextEnd { .. }
            | Event::ToolInputDelta { .. }
            | Event::CommandExecuted { .. }
            | Event::StepFinished { .. }
            | Event::Error {
                failed_message: None,
                ..
            } => {}
            Event::SessionForked {
                source,
                before_message,
                ..
            } => {
                self.session.forked_from = Some(*source);
                self.session.forked_before = *before_message;
            }
            Event::Error {
                failed_message: Some(message),
                code,
                message: text,
                ..
            } => self.apply_error(message, code, text),
            // Billed stays billed: session totals only ever grow.
            Event::UsageRecorded {
                message,
                model,
                purpose,
                tokens,
                ..
            } => self.apply_usage_recorded(message, model, purpose, tokens),
            // Full replacement: the event carries the whole list.
            Event::TodosUpdated { todos, .. } => {
                self.replace_todos(todos.clone());
            }
            Event::Extension {
                session,
                bundle,
                kind,
                payload,
            } => {
                if let Some(reducer) = reducers.get(bundle) {
                    reducer.fold_extension(self, *session, kind, payload);
                } else {
                    self.record_raw_extension(bundle, kind, payload);
                }
            }
        }
    }

    fn record_raw_extension(&mut self, bundle: &str, kind: &str, payload: &serde_json::Value) {
        let entry = self
            .extensions
            .entry(bundle.to_owned())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let item = serde_json::json!({
            "kind": kind,
            "payload": payload,
        });
        match entry {
            serde_json::Value::Array(items) => items.push(item),
            other => {
                let previous = std::mem::replace(other, serde_json::Value::Array(Vec::new()));
                if let serde_json::Value::Array(items) = other {
                    items.push(previous);
                    items.push(item);
                }
            }
        }
    }
}
