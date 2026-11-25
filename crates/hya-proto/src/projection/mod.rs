//! Shared idempotent reducer: fold an event log into a session view. Used by the
//! store (read path) and the client (SSE reconnect); idempotent by `EventSeq` so
//! re-delivered events are no-ops.

mod compaction;
mod helpers;
mod session;
mod snapshot;
mod team;
#[cfg(test)]
mod tests;
mod workflow;

use serde::{Deserialize, Serialize};

use self::team::team_is_empty;
use crate::event::{Envelope, Event};

pub use compaction::ContextStatusProjection;
pub use session::{
    HandoffProjection, MessageError, MessageProjection, PartProjection, SessionArchiveError,
    SessionProjection, session_archive_event,
};
pub use snapshot::PROJECTION_REDUCER_VERSION;
pub use team::{
    ArchivedEntry, ChannelProjection, MailMessage, MemberProjection, ResidentWorkProjection,
    RosterEntry, TeamProjection,
};

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(feature = "lifecycle-bench")]
impl Clone for Projection {
    fn clone(&self) -> Self {
        crate::bench::projection_clone(self);
        Self {
            session: self.session.clone(),
            team: self.team.clone(),
            last_seq: self.last_seq,
        }
    }
}

/// Full folded view: one session transcript plus optional team mailbox/roster state.
#[cfg_attr(not(feature = "lifecycle-bench"), derive(Clone))]
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// Session transcript and metadata.
    pub session: SessionProjection,
    /// Team-scoped mailbox/channel/roster state. Empty on sessions that are not a
    /// team root / carry no mail events.
    #[serde(default, skip_serializing_if = "team_is_empty")]
    pub team: TeamProjection,
    /// Highest durable `EventSeq` applied; live `seq == 0` does not advance this.
    pub last_seq: u64,
}

impl Projection {
    /// Fold a slice of envelopes in order into a fresh projection.
    #[must_use]
    pub fn from_events(envs: &[Envelope]) -> Self {
        let mut p = Self::default();
        for e in envs {
            p.apply(e);
        }
        p
    }

    /// Apply one envelope: `seq == 0` is live-only (no `last_seq` advance);
    /// `seq <= last_seq` is a durable no-op; otherwise fold and advance `last_seq`.
    pub fn apply(&mut self, env: &Envelope) {
        if env.seq.0 == 0 {
            self.apply_event(&env.event);
            self.stamp_message_time(&env.event, env.ts_millis);
            return;
        }
        if env.seq.0 <= self.last_seq {
            return;
        }
        self.apply_event(&env.event);
        self.stamp_message_time(&env.event, env.ts_millis);
        self.last_seq = env.seq.0;
    }

    fn apply_event(&mut self, e: &Event) {
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
            Event::WorkflowSelected { workflow, .. } => self.apply_workflow_selected(workflow),
            Event::WorkflowRunStarted {
                run,
                workflow,
                request_hash,
                owner,
                stages,
                ..
            } => self.apply_workflow_run_started(run, workflow, request_hash, owner, stages),
            Event::WorkflowStageStarted { run, stage, .. } => {
                self.apply_workflow_stage_started(run, stage)
            }
            Event::WorkflowStageMemberLinked {
                run,
                stage,
                member,
                role,
                iteration,
                ..
            } => self.apply_workflow_stage_member_linked(run, stage, member, role, iteration),
            Event::WorkflowStageRouteOutcome {
                session,
                run,
                stage: stage_id,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
            } => self.apply_workflow_stage_route_outcome(
                session,
                run,
                stage_id,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
            ),
            Event::WorkflowStageFinished {
                run, stage, status, ..
            } => self.apply_workflow_stage_finished(run, stage, status),
            Event::WorkflowRunFinished {
                run, status, error, ..
            } => self.apply_workflow_run_finished(run, status, error),
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
            Event::MemberSpawned {
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
                ..
            } => self.apply_member_spawned(
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                tool_call,
            ),
            Event::MemberStatusChanged { member, status, .. } => {
                self.apply_member_status_changed(member, status)
            }
            Event::MemberFinished {
                member,
                status,
                summary,
                child,
                ..
            } => self.apply_member_finished(member, status, summary, child),
            Event::AgentRegistered {
                session: _,
                agent_session,
                handle,
                parent,
                agent_type,
                mode,
            } => self.apply_agent_registered(agent_session, handle, parent, agent_type, mode),
            Event::AgentActivityChanged {
                handle,
                status,
                current_task,
                ..
            } => self.apply_agent_activity_changed(handle, status, current_task),
            Event::ResidentWorkStarted {
                actor_session,
                handle,
                epoch,
                inbox_through,
                ..
            } => self.apply_resident_work_started(actor_session, handle, epoch, inbox_through),
            Event::AgentHeartbeat {
                handle,
                heartbeat_ms,
                ..
            } => self.apply_agent_heartbeat(handle, heartbeat_ms),
            Event::ChannelJoined { .. } => {}
            Event::ChannelCreated {
                channel,
                kind,
                unit,
                members,
                ..
            } => self.apply_channel_created(channel, kind, unit, members),
            Event::SubagentReported {
                member,
                outcome,
                report,
                ..
            } => self.apply_subagent_reported(member, outcome, report),
            Event::HandoffCommitted {
                generation,
                doc,
                degraded,
                ..
            } => self.apply_handoff_committed(generation, doc, degraded),
            Event::AgentArchived {
                handle,
                child,
                reason,
                ..
            } => self.apply_agent_archived(handle, child, reason),
            Event::MailConsumed {
                handle, through, ..
            }
            | Event::ResidentWorkFinished {
                handle,
                inbox_through: through,
                ..
            } => self.apply_mail_consumed(handle, through, e),
            Event::AgentRestarted { handle, .. } => self.apply_agent_restarted(handle),
            Event::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => self.apply_mail_sent(from, to, kind, body),
            Event::ContextCompacted {
                message: marker,
                to_message,
                step_cut,
                ..
            } => self.apply_context_compacted(marker, to_message, step_cut),
            Event::TextEnd { .. }
            | Event::SessionStatus { .. }
            | Event::ToolInputDelta { .. }
            | Event::CommandExecuted { .. }
            | Event::StepFinished { .. }
            | Event::Error {
                failed_message: None,
                ..
            } => {}
            Event::ContextEvicted { parts, .. } => self.apply_context_evicted(parts),
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
            // Latest occupancy wins: clients read one current figure, not a history.
            Event::ContextStatus {
                tokens,
                source,
                mode,
                threshold,
                ..
            } => self.apply_context_status(tokens, source, mode, threshold),
        }
    }
}
