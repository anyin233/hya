//! Conversions between the event-sourced domain types (`hya-proto`) and
//! the generated `hya.v1` wire types, plus the curated envelope →
//! `StreamEvent` projection used by replay and both live streams.

use hya_api::v1 as pb;
use hya_proto::projection::{MemberProjection, MessageProjection, PartProjection, Projection};
use hya_proto::{Envelope, Event, Role};
use hya_proto::{FinishCause, FinishReason, MemberRunStatus, ReportOutcome, ToolPartState};

/// Map a domain finish reason to the wire enum.
pub(crate) fn finish_reason(finish: FinishReason) -> i32 {
    match finish {
        FinishReason::Stop => pb::FinishReason::Stop as i32,
        FinishReason::ToolCalls => pb::FinishReason::ToolCalls as i32,
        FinishReason::Length => pb::FinishReason::Length as i32,
        FinishReason::Cancelled => pb::FinishReason::Cancelled as i32,
        FinishReason::Error => pb::FinishReason::Error as i32,
    }
}

/// Map a domain finish cause to the wire enum (`0` when absent).
pub(crate) fn finish_cause(cause: Option<FinishCause>) -> i32 {
    match cause {
        None => pb::FinishCause::Unspecified as i32,
        Some(FinishCause::UserCancel) => pb::FinishCause::UserCancel as i32,
        Some(FinishCause::Shutdown) => pb::FinishCause::Shutdown as i32,
        Some(FinishCause::LeaderFailed) => pb::FinishCause::LeaderFailed as i32,
        Some(FinishCause::Interrupted) => pb::FinishCause::Interrupted as i32,
        Some(FinishCause::ProviderError) => pb::FinishCause::ProviderError as i32,
        Some(FinishCause::Archived) => pb::FinishCause::Archived as i32,
        Some(FinishCause::Other) => pb::FinishCause::Other as i32,
    }
}

/// Map a domain role to the wire enum.
pub(crate) fn wire_role(role: Role) -> i32 {
    match role {
        Role::User => pb::Role::User as i32,
        Role::Assistant => pb::Role::Assistant as i32,
        Role::System => pb::Role::System as i32,
    }
}

/// Map a domain tool-part state to the wire enum.
pub(crate) fn tool_state(state: &ToolPartState) -> i32 {
    match state {
        ToolPartState::Pending { .. } => pb::ToolExecutionState::Pending as i32,
        ToolPartState::Running { .. } => pb::ToolExecutionState::Running as i32,
        ToolPartState::Completed { .. } => pb::ToolExecutionState::Ok as i32,
        ToolPartState::Error { .. } => pb::ToolExecutionState::Error as i32,
    }
}

/// JSON text of a tool payload; empty for `null` (arguments not known yet).
fn json_text(value: &serde_json::Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        value.to_string()
    }
}

/// Stable error code of a tool error: the structured `error.type`, else
/// `fallback`.
fn tool_error_code(value: Option<&serde_json::Value>, fallback: &str) -> String {
    value
        .and_then(|value| value.pointer("/error/type"))
        .and_then(|code| code.as_str())
        .unwrap_or(fallback)
        .to_owned()
}

/// Wire tool part for one domain tool state: arguments in every state, the
/// stored (already capped) output and duration once OK, the error text once
/// failed. The caller fills `call_id` and `tool`.
fn tool_call_part(state: &ToolPartState) -> pb::ToolCallPart {
    let mut part = pb::ToolCallPart {
        state: tool_state(state),
        ..Default::default()
    };
    match state {
        ToolPartState::Pending { input } | ToolPartState::Running { input } => {
            part.input_json = json_text(input);
        }
        ToolPartState::Completed {
            input,
            output,
            time_ms,
        } => {
            part.input_json = json_text(input);
            part.output_json = output.to_string();
            part.duration_ms = *time_ms;
        }
        ToolPartState::Error {
            input,
            message,
            value,
        } => {
            part.input_json = json_text(input);
            part.error_code = tool_error_code(value.as_ref(), "unknown");
            part.error_message = message.clone();
        }
    }
    part
}

/// Wire member status for a domain member run status.
fn member_status(status: MemberRunStatus) -> i32 {
    match status {
        MemberRunStatus::Spawning => pb::MemberStatus::Spawning as i32,
        MemberRunStatus::Running => pb::MemberStatus::Running as i32,
        MemberRunStatus::Done => pb::MemberStatus::Done as i32,
        MemberRunStatus::Failed => pb::MemberStatus::Failed as i32,
        MemberRunStatus::Cancelled => pb::MemberStatus::Cancelled as i32,
    }
}

/// Map a folded member row to the wire member view.
pub(crate) fn member_info(member: &MemberProjection) -> pb::MemberInfo {
    pb::MemberInfo {
        member: member.member.to_string(),
        child: member
            .child
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        agent: member.subagent_type.to_string(),
        description: member.description.clone(),
        status: member_status(member.status),
        summary: member.summary.clone(),
        call_id: member
            .tool_call
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        depth: member.depth,
    }
}

/// Map a projected session to the wire session summary.
///
/// `info` supplies wall-clock timestamps from the store's session listing.
pub(crate) fn session_info(
    projection: &Projection,
    started_millis: i64,
    updated_millis: i64,
) -> pb::SessionInfo {
    let session = &projection.session;
    pb::SessionInfo {
        id: session
            .id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        parent: session
            .parent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        title: session.title.clone().unwrap_or_default(),
        agent: session
            .agent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        model: session
            .model
            .as_ref()
            .map(|model| model.to_string())
            .and_then(|reference| super::catalog::model_ref(&reference)),
        workdir: session.workdir.clone().unwrap_or_default(),
        background: false,
        busy: false,
        time_created: timestamp(started_millis),
        time_updated: timestamp(updated_millis),
        last_seq: projection.last_seq,
        // Effective (root-inherited) mode; filled in by the caller, which
        // can walk the lineage.
        permission_mode: String::new(),
        members: session.members.iter().map(member_info).collect(),
        usage: (!session.usage.is_empty()).then(|| usage_totals(&session.usage.total())),
        forked_from: session.forked_from.map(|source| pb::ForkSource {
            session: source.to_string(),
            message_id: session
                .forked_before
                .map(|message| message.to_string())
                .unwrap_or_default(),
        }),
        revert: session.revert.as_ref().map(|revert| pb::SessionRevert {
            message_id: revert.message.to_string(),
            text: revert
                .hidden
                .first()
                .map(|message| message_text(&message.parts))
                .unwrap_or_default(),
            hidden_messages: u32::try_from(revert.hidden.len()).unwrap_or(u32::MAX),
            files: revert.files.iter().map(reverted_file).collect(),
        }),
        archived: session.is_archived(),
        archived_at: session.archived_at_millis().and_then(timestamp),
    }
}

/// Concatenated text parts of a message (a user prompt's text).
pub(crate) fn message_text(parts: &[hya_proto::PartProjection]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// One file a revert or unrevert wrote, as the wire reports it.
pub(crate) fn reverted_file(file: &hya_proto::FileRestore) -> pb::RevertedFile {
    let (action, reason) = match (&file.error, &file.restored) {
        (Some(error), _) => ("failed", error.clone()),
        (None, hya_proto::FileState::Omitted { reason, .. }) => ("skipped", reason.clone()),
        (None, restored) if *restored == file.saved => ("unchanged", String::new()),
        (None, hya_proto::FileState::Absent) => ("deleted", String::new()),
        (None, hya_proto::FileState::Stored { .. }) => ("restored", String::new()),
    };
    pb::RevertedFile {
        path: file.path.clone(),
        action: action.to_owned(),
        reason,
    }
}

/// Unix milliseconds → wire timestamp.
pub(crate) fn timestamp(millis: i64) -> Option<pbjson_types::Timestamp> {
    Some(pbjson_types::Timestamp {
        seconds: millis.div_euclid(1000),
        nanos: (millis.rem_euclid(1000) * 1_000_000) as i32,
    })
}

/// Convert a JSON object into the wire `Struct` form.
pub(crate) fn to_struct(value: serde_json::Value) -> pbjson_types::Struct {
    let map = match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    pbjson_types::Struct {
        fields: map
            .into_iter()
            .map(|(key, value)| (key, to_pb_value(value)))
            .collect(),
    }
}

fn to_pb_value(value: serde_json::Value) -> pbjson_types::Value {
    use pbjson_types::value::Kind;
    use serde_json::Value as Json;
    let kind = match value {
        Json::Null => Kind::NullValue(0),
        Json::Bool(flag) => Kind::BoolValue(flag),
        Json::Number(number) => Kind::NumberValue(number.as_f64().unwrap_or(0.0)),
        Json::String(text) => Kind::StringValue(text),
        Json::Array(items) => Kind::ListValue(pbjson_types::ListValue {
            values: items.into_iter().map(to_pb_value).collect(),
        }),
        Json::Object(map) => Kind::StructValue(to_struct(Json::Object(map))),
    };
    pbjson_types::Value { kind: Some(kind) }
}

/// Convert a wire `Struct` back into a JSON object.
pub(crate) fn from_struct(value: &pbjson_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(
        value
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), from_pb_value(value)))
            .collect(),
    )
}

fn from_pb_value(value: &pbjson_types::Value) -> serde_json::Value {
    use pbjson_types::value::Kind;
    use serde_json::Value as Json;
    match value.kind.as_ref() {
        Some(Kind::NullValue(_)) | None => Json::Null,
        Some(Kind::BoolValue(flag)) => Json::Bool(*flag),
        Some(Kind::NumberValue(number)) => serde_json::Number::from_f64(*number)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Some(Kind::StringValue(text)) => Json::String(text.clone()),
        Some(Kind::ListValue(list)) => Json::Array(list.values.iter().map(from_pb_value).collect()),
        Some(Kind::StructValue(map)) => from_struct(map),
    }
}

/// Map a projected message to the wire message view.
pub(crate) fn message(message: &MessageProjection) -> pb::MessageInfo {
    pb::MessageInfo {
        id: message.id.to_string(),
        session: String::new(),
        role: wire_role(message.role),
        agent: message
            .agent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        model: message
            .served_model()
            .map(ToString::to_string)
            .unwrap_or_default(),
        finish: message.finish.map(finish_reason).unwrap_or(0),
        parts: message
            .parts
            .iter()
            .filter_map(part)
            .chain(attachment_parts(&message.files))
            .collect(),
        time_created: message.time_created.and_then(timestamp),
        time_updated: message.time_updated.and_then(timestamp),
        finish_cause: finish_cause(message.cause),
        error: message.error.as_ref().map(|error| pb::MessageError {
            code: error.code.clone(),
            message: error.message.clone(),
        }),
        // Attributed rounds when recorded; the legacy finish total otherwise.
        usage: message
            .usage
            .as_ref()
            .map(|usage| usage.tokens)
            .or(message.tokens)
            .as_ref()
            .map(token_usage),
        round_usage: message
            .usage
            .as_ref()
            .map(|usage| token_usage(&usage.last_round)),
    }
}

/// Map one domain usage (a call, or a sum of calls) to the wire.
pub(crate) fn token_usage(tokens: &hya_proto::TokenUsage) -> pb::TokenUsage {
    pb::TokenUsage {
        input: tokens.input,
        output: tokens.output,
        reasoning: tokens.reasoning,
        cache_read: tokens.cache_read,
        cache_write: tokens.cache_write,
        reasoning_unknown: tokens.reasoning_unknown,
    }
}

/// Map folded session totals to the wire usage shape.
fn usage_totals(totals: &hya_proto::UsageTotals) -> pb::TokenUsage {
    pb::TokenUsage {
        input: totals.input,
        output: totals.output,
        reasoning: totals.reasoning,
        cache_read: totals.cache_read,
        cache_write: totals.cache_write,
        reasoning_unknown: totals.reasoning_unknown_output > 0,
    }
}

/// The prompt images recorded on a message as wire parts. Transcript reads
/// never carry the bytes (`data` stays empty); they live in the session blob
/// table and only go to the model.
fn attachment_parts(files: &[serde_json::Value]) -> Vec<pb::PartInfo> {
    hya_core::attachments::recorded_attachments(files)
        .into_iter()
        .map(|attachment| pb::PartInfo {
            id: attachment.part,
            kind: Some(hya_api::v1::part_info::Kind::Attachment(
                pb::AttachmentPart {
                    name: attachment.name,
                    mime: attachment.mime,
                    data: Vec::new(),
                    path: attachment.path.unwrap_or_default(),
                    size: attachment.size,
                },
            )),
        })
        .collect()
}

/// Map a projected part; media-free projections always map.
fn part(part: &PartProjection) -> Option<pb::PartInfo> {
    use hya_api::v1::part_info::Kind as K;
    let (id, kind) = match part {
        PartProjection::Text { id, text } => {
            (id.to_string(), K::Text(pb::TextPart { text: text.clone() }))
        }
        PartProjection::Reasoning {
            id, text, reason, ..
        } => (
            id.to_string(),
            K::Reasoning(pb::ReasoningPart {
                text: text.clone(),
                variant: reason.clone().unwrap_or_default(),
            }),
        ),
        PartProjection::Tool {
            id,
            call,
            name,
            state,
        } => {
            let mut tool = tool_call_part(state);
            tool.call_id = call.to_string();
            tool.tool = name.to_string();
            (id.to_string(), K::ToolCall(tool))
        }
    };
    Some(pb::PartInfo {
        id,
        kind: Some(kind),
    })
}

/// `sessionUpdated` carrying only the archived flag.
fn archived_update(archived: bool) -> hya_api::v1::stream_event::Payload {
    hya_api::v1::stream_event::Payload::SessionUpdated(pb::SessionUpdated {
        title: None,
        model: None,
        agent: None,
        background: None,
        permission_mode: None,
        archived: Some(archived),
    })
}

/// Project one envelope onto the curated wire event stream.
///
/// Returns `None` for internal-only events that the v1 surface does not
/// expose. Both transports serialize the result identically. Live-only
/// envelopes (`seq == 0`, the assistant text of an in-flight round) map the
/// same way and keep `seq: 0`; their ids match the durable events the round
/// later records.
pub(crate) fn stream_event(envelope: &Envelope) -> Option<pb::StreamEvent> {
    use hya_api::v1::stream_event::Payload as P;
    let session = envelope
        .event
        .session()
        .map(|id| id.to_string())
        .unwrap_or_default();
    let payload = match &envelope.event {
        Event::SessionCreated {
            agent,
            model,
            workdir,
            parent,
            ..
        } => P::SessionStarted(pb::SessionStarted {
            agent: agent.to_string(),
            model: model.to_string(),
            workdir: workdir.clone(),
            parent: parent.as_ref().map(ToString::to_string).unwrap_or_default(),
        }),
        Event::SessionTitled { title, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: Some(title.clone()),
            model: None,
            agent: None,
            background: None,
            permission_mode: None,
            archived: None,
        }),
        Event::AgentSwitched { agent, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: None,
            model: None,
            agent: Some(agent.to_string()),
            background: None,
            permission_mode: None,
            archived: None,
        }),
        Event::ModelSwitched { model, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: None,
            model: Some(model.to_string()),
            agent: None,
            background: None,
            permission_mode: None,
            archived: None,
        }),
        Event::SessionPermissionModeSet { mode, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: None,
            model: None,
            agent: None,
            background: None,
            permission_mode: Some(mode.clone()),
            archived: None,
        }),
        // A legacy zero stamp cleared the archive.
        Event::SessionArchived { archived, .. } => {
            archived_update(archived.as_f64().is_some_and(|stamp| stamp != 0.0))
        }
        Event::SessionUnarchived { .. } => archived_update(false),
        Event::MessageStarted {
            message,
            role,
            agent,
            model,
            ..
        } => P::MessageStarted(pb::MessageStarted {
            message: message.to_string(),
            role: wire_role(*role),
            agent: agent.as_ref().map(ToString::to_string).unwrap_or_default(),
            model: model.as_ref().map(ToString::to_string).unwrap_or_default(),
        }),
        Event::MessageFinished {
            message,
            finish,
            cause,
            ..
        } => P::MessageFinished(pb::MessageFinished {
            message: message.to_string(),
            finish: finish_reason(*finish),
            usage: None,
            cause: finish_cause(*cause),
        }),
        Event::TextStart { message, part, .. } => {
            P::PartStarted(part_started(message, part, "text"))
        }
        Event::TextDelta {
            message,
            part,
            delta,
            ..
        } => P::PartAppended(pb::PartAppended {
            message: message.to_string(),
            part: part.to_string(),
            text_delta: delta.clone(),
        }),
        Event::TextReplace {
            message,
            part,
            text,
            ..
        }
        | Event::ReasoningReplace {
            message,
            part,
            text,
            ..
        } => P::PartReplaced(pb::PartReplaced {
            message: message.to_string(),
            part: part.to_string(),
            text: text.clone(),
        }),
        Event::TextEnd { message, part, .. } => P::PartCompleted(pb::PartCompleted {
            message: message.to_string(),
            part: part.to_string(),
        }),
        Event::ReasoningStart { message, part, .. } => {
            P::PartStarted(part_started(message, part, "reasoning"))
        }
        Event::ReasoningDelta {
            message,
            part,
            delta,
            ..
        } => P::PartAppended(pb::PartAppended {
            message: message.to_string(),
            part: part.to_string(),
            text_delta: delta.clone(),
        }),
        Event::ReasoningEnd { message, part, .. } => P::PartCompleted(pb::PartCompleted {
            message: message.to_string(),
            part: part.to_string(),
        }),
        Event::ToolInputStart {
            message,
            part,
            call,
            name,
            ..
        } => P::PartStarted(pb::PartStarted {
            tool: name.to_string(),
            call_id: call.to_string(),
            ..part_started(message, part, "tool_call")
        }),
        Event::ToolInputDelta {
            message,
            part,
            delta,
            ..
        } => P::PartAppended(pb::PartAppended {
            message: message.to_string(),
            part: part.to_string(),
            text_delta: delta.clone(),
        }),
        Event::ToolCallRequested {
            message,
            part,
            call,
            name,
            input,
            ..
        } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: call.to_string(),
            state: pb::ToolExecutionState::Running as i32,
            input_json: input.to_string(),
            tool: name.to_string(),
            ..Default::default()
        }),
        Event::ToolPartUpdated {
            message,
            part,
            state,
            ..
        } => {
            let full = tool_call_part(state);
            P::ToolStateChanged(pb::ToolStateChanged {
                message: message.to_string(),
                part: part.to_string(),
                call_id: String::new(),
                state: full.state,
                error_code: full.error_code,
                error_message: full.error_message,
                input_json: full.input_json,
                output_json: full.output_json,
                duration_ms: full.duration_ms,
                tool: String::new(),
            })
        }
        Event::ToolResult {
            message,
            part,
            call,
            output,
            time_ms,
            ..
        } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: call.to_string(),
            state: pb::ToolExecutionState::Ok as i32,
            output_json: output.to_string(),
            duration_ms: *time_ms,
            ..Default::default()
        }),
        Event::ToolError {
            message,
            part,
            call,
            message_text,
            value,
            ..
        } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: call.to_string(),
            state: pb::ToolExecutionState::Error as i32,
            error_code: tool_error_code(value.as_ref(), "unknown"),
            error_message: message_text.clone(),
            ..Default::default()
        }),
        Event::MemberSpawned {
            member,
            child,
            subagent_type,
            description,
            depth,
            tool_call,
            ..
        } => P::MemberUpdated(pb::MemberInfo {
            member: member.to_string(),
            child: child.as_ref().map(ToString::to_string).unwrap_or_default(),
            agent: subagent_type.to_string(),
            description: description.clone(),
            status: pb::MemberStatus::Spawning as i32,
            summary: String::new(),
            call_id: tool_call
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            depth: *depth,
        }),
        Event::MemberStatusChanged { member, status, .. } => P::MemberUpdated(pb::MemberInfo {
            member: member.to_string(),
            status: member_status(*status),
            ..Default::default()
        }),
        Event::MemberFinished {
            member,
            status,
            summary,
            child,
            ..
        } => P::MemberUpdated(pb::MemberInfo {
            member: member.to_string(),
            child: child.as_ref().map(ToString::to_string).unwrap_or_default(),
            status: member_status(*status),
            summary: summary.clone(),
            ..Default::default()
        }),
        // A resident member's terminal report (ADR-0015) closes its row.
        Event::SubagentReported {
            member,
            child,
            outcome,
            report,
            ..
        } => P::MemberUpdated(pb::MemberInfo {
            member: member.to_string(),
            child: child.to_string(),
            status: member_status(match outcome {
                ReportOutcome::Done => MemberRunStatus::Done,
                ReportOutcome::Failed => MemberRunStatus::Failed,
            }),
            summary: report.clone(),
            ..Default::default()
        }),
        Event::Error {
            code,
            message: text,
            failed_message,
            ..
        } => P::ErrorReported(pb::ErrorReported {
            message: failed_message
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            code: code.clone(),
            error_message: text.clone(),
        }),
        Event::ContextCompacted {
            strategy,
            message,
            folded_count,
            threshold,
            ..
        } => P::CompactionApplied(pb::CompactionApplied {
            until_seq: envelope.seq.0,
            strategy: strategy.as_str().to_owned(),
            message: message.to_string(),
            folded_count: *folded_count,
            // Automatic strategies record the threshold that tripped; a
            // manual `CompactSession` records none.
            manual: *threshold == 0,
        }),
        Event::UsageRecorded {
            message,
            model,
            tokens,
            ..
        } => P::TokensRecorded(pb::TokensRecorded {
            message: message
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            usage: Some(token_usage(tokens)),
            model: model.to_string(),
        }),
        Event::TodosUpdated { todos, .. } => P::TodoUpdated(pb::TodoUpdated {
            items: super::message::todo_list(todos).items,
        }),
        Event::SessionReverted { message, files, .. } => P::SessionReverted(pb::SessionReverted {
            message_id: message.to_string(),
            undone: false,
            files: files.iter().map(reverted_file).collect(),
        }),
        Event::SessionUnreverted { files, .. } => P::SessionReverted(pb::SessionReverted {
            message_id: String::new(),
            undone: true,
            files: files.iter().map(reverted_file).collect(),
        }),
        Event::UserPromptContextRecorded { message, files, .. } => {
            let parts = attachment_parts(files);
            if parts.is_empty() {
                return None;
            }
            P::PartsAdded(pb::PartsAdded {
                message: message.to_string(),
                parts,
            })
        }
        Event::WorkflowSelected { .. }
        | Event::WorkflowRunStarted { .. }
        | Event::WorkflowStageStarted { .. }
        | Event::WorkflowStageFinished { .. }
        | Event::WorkflowRunFinished { .. } => {
            P::WorkflowUpdated(pb::WorkflowUpdated { state: None })
        }
        _ => return None,
    };
    Some(pb::StreamEvent {
        seq: envelope.seq.0,
        session,
        time_recorded: timestamp(envelope.ts_millis),
        payload: Some(payload),
    })
}

fn part_started(
    message: &hya_proto::MessageId,
    part: &hya_proto::PartId,
    kind: &str,
) -> pb::PartStarted {
    pb::PartStarted {
        message: message.to_string(),
        part: part.to_string(),
        kind: kind.to_owned(),
        tool: String::new(),
        call_id: String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_proto::{EventSeq, MessageId, SessionId};

    fn finished(cause: Option<FinishCause>) -> Envelope {
        Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: Event::MessageFinished {
                session: SessionId::new(),
                message: MessageId::new(),
                role: Role::Assistant,
                finish: FinishReason::Cancelled,
                tokens: None,
                cause,
            },
        }
    }

    #[test]
    fn message_finished_carries_its_cause_on_the_wire() {
        let event = stream_event(&finished(Some(FinishCause::Shutdown))).unwrap();
        match event.payload {
            Some(pb::stream_event::Payload::MessageFinished(finished)) => {
                assert_eq!(finished.finish, pb::FinishReason::Cancelled as i32);
                assert_eq!(finished.cause, pb::FinishCause::Shutdown as i32);
            }
            other => panic!("expected MessageFinished, got {other:?}"),
        }
        // Old logs and model-ended messages: unspecified (0).
        let event = stream_event(&finished(None)).unwrap();
        match event.payload {
            Some(pb::stream_event::Payload::MessageFinished(finished)) => {
                assert_eq!(finished.cause, pb::FinishCause::Unspecified as i32);
            }
            other => panic!("expected MessageFinished, got {other:?}"),
        }
    }
}
