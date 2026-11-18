//! The rewrite primitive: remove everything before the current step.

use async_trait::async_trait;
use hya_proto::context::{COMPACT_CONTEXT_MARKER, render_todos};
use hya_proto::{
    CompactionStrategy, Event, FinishReason, MessageId, PartId, Projection, Role, SessionId,
    StepCut, TodoItem,
};
use hya_tool::{RewriteCtx, Rewriter, ToolError};

use super::request::{collect_text, compacted_messages, is_marker};
use crate::error::CoreError;

/// Share of `max_context` (percent) the last round may fill before the
/// harness rewrites.
const TRIGGER_PERCENT: u64 = 80;

/// Who started a rewrite.
#[derive(Clone, Copy, Debug)]
pub(super) enum Initiator {
    /// The model, through the `rewrite` tool, from inside the current step.
    Model,
    /// The harness, before the next step starts.
    Harness,
}

/// The kernel's own rewriter: removes everything before the next step once
/// the last round filled 80% of the window. Bare mode uses it, and so does
/// full mode when no internal bundle registers a rewriter.
pub struct HarnessRewriter;

#[async_trait]
impl Rewriter for HarnessRewriter {
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        let budget = pressure(cx.projection, cx.window.map(|window| window.max_context));
        if budget.is_none_or(|(used, threshold)| used <= threshold) {
            return Ok(false);
        }
        self.commit(&cx, budget).await
    }

    async fn rewrite_now(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        let budget = pressure(cx.projection, cx.window.map(|window| window.max_context));
        self.commit(&cx, budget).await
    }
}

impl HarnessRewriter {
    async fn commit(
        &self,
        cx: &RewriteCtx<'_>,
        budget: Option<(u64, u64)>,
    ) -> Result<bool, ToolError> {
        let rewrite = rewrite_events(
            cx.projection,
            cx.session,
            cx.message,
            Initiator::Harness,
            budget,
        )
        .map_err(|error| ToolError::Other(error.to_string()))?;
        let Some((events, _)) = rewrite else {
            return Ok(false);
        };
        cx.log.append(cx.session, events).await?;
        Ok(true)
    }
}

/// The latest recorded round's `prompt + output` tokens and the trigger
/// threshold, when the projection has usage and the route a context window.
pub(super) fn pressure(projection: &Projection, max_context: Option<u32>) -> Option<(u64, u64)> {
    let last = projection
        .session
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::Assistant)
        .find_map(|message| message.usage.as_ref())?
        .last_round;
    let window = max_context.filter(|w| *w > 0)?;
    Some((
        last.prompt().saturating_add(last.output),
        u64::from(window) * TRIGGER_PERCENT / 100,
    ))
}

/// Remove everything before the current step of `message`: the visible
/// messages before the turn's user message, and the earlier steps of
/// `message`. Keeps the turn's user message and the current step.
///
/// The batch to append: a system marker that repeats every earlier user
/// message verbatim, then `ContextCompacted{strategy: Rewrite}`. Returns it
/// with the folded message count, or `None` when nothing is removable.
pub(super) fn rewrite_events(
    projection: &Projection,
    session: SessionId,
    message: MessageId,
    initiator: Initiator,
    budget: Option<(u64, u64)>,
) -> Result<Option<(Vec<Event>, u32)>, CoreError> {
    let visible = compacted_messages(projection);
    let at = visible
        .iter()
        .position(|row| row.id == message)
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "rewrite: message {message} is not in the live context"
            ))
        })?;
    let current = &visible[at];
    // The turn's head: its user message, or the message itself without one.
    let head = visible[..at]
        .iter()
        .rposition(|row| row.role == Role::User)
        .unwrap_or(at);
    let step_start = match initiator {
        Initiator::Model => current.last_step_start.unwrap_or(0),
        Initiator::Harness => current.parts.len(),
    };
    let earlier_parts = current
        .parts
        .get(current.folded_parts..step_start)
        .unwrap_or(&[]);
    // An earlier marker is the record of a past rewrite, not content.
    let earlier_messages = visible[..head].iter().filter(|row| !is_marker(row)).count();
    if earlier_messages == 0 && earlier_parts.is_empty() {
        return Ok(None);
    }
    let kept_head = visible[head].id;
    let step_cut = earlier_parts.last().map(|part| StepCut {
        through_part: part.id(),
        kept_head,
    });
    let to_message = match step_cut {
        Some(_) => message,
        // `earlier_messages > 0` here, so the head has a message before it.
        None => visible[head - 1].id,
    };
    let from_message = if head > 0 { visible[0].id } else { message };
    let folded_count = u32::try_from(head + usize::from(step_cut.is_some())).unwrap_or(u32::MAX);
    let users: Vec<String> = projection
        .session
        .messages
        .iter()
        .take_while(|row| row.id != kept_head)
        .filter(|row| row.role == Role::User)
        .map(|row| collect_text(&row.parts))
        .collect();
    let (input_tokens_est, threshold) = budget.unwrap_or((0, 0));
    let marker = MessageId::new();
    let todos = projection.session.todos.as_deref().unwrap_or_default();
    let mut events = marker_events(session, marker, marker_text(initiator, &users, todos));
    events.push(Event::ContextCompacted {
        session,
        message: marker,
        strategy: CompactionStrategy::Rewrite,
        from_message,
        to_message,
        folded_count,
        input_tokens_est,
        threshold,
        step_cut,
    });
    Ok(Some((events, folded_count)))
}

fn marker_text(initiator: Initiator, users: &[String], todos: &[TodoItem]) -> String {
    let by = match initiator {
        Initiator::Model => "model",
        Initiator::Harness => "harness",
    };
    let users: Vec<String> = users
        .iter()
        .enumerate()
        .map(|(n, text)| format!("--- user message {} ---\n{text}", n + 1))
        .collect();
    let mut marker = format!(
        "{COMPACT_CONTEXT_MARKER}[hya] Earlier context was removed by the {by}. It stays in the session log and cannot be read back.\nUser messages from the removed part, verbatim:\n\n{}",
        users.join("\n\n")
    );
    if !todos.is_empty() {
        marker.push_str("\n\n");
        marker.push_str(&render_todos(todos));
    }
    marker
}

fn marker_events(session: SessionId, message: MessageId, text: String) -> Vec<Event> {
    let part = PartId::new();
    vec![
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
            delta: text,
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
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ]
}
