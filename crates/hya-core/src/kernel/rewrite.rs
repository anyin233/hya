//! The rewrite primitive: remove everything before the current step.

use std::sync::Arc;

use hya_context::rewrite::{Pressure, RewriteCtx, Rewriter, ToolError};
use hya_context::tokens::TokenAccounting;
use hya_proto::context::{COMPACT_CONTEXT_MARKER, marker_criteria};
use hya_proto::{
    CompactionStrategy, Event, MessageId, Projection, Role, SessionId, StepCut, TodoItem,
};

use super::request::{collect_text, compacted_messages, is_marker};
use super::round::system_message_events;
use crate::error::CoreError;

/// Share of `max_context` (percent) the request may fill before the context
/// is reduced.
const TRIGGER_PERCENT: u64 = 80;

/// Share of `max_context` (percent) at which the model is asked, once per
/// turn, to bring its todo list up to date before a rewrite.
const CRITERIA_CHECKPOINT_PERCENT: u64 = 60;

/// Prefix of the goal-checkpoint notice; marks it in the transcript.
pub(super) const CRITERIA_CHECKPOINT: &str = "[hya] Goal checkpoint:";

pub(super) fn is_criteria_checkpoint(message: &hya_proto::MessageProjection) -> bool {
    message.role == Role::System && collect_text(&message.parts).starts_with(CRITERIA_CHECKPOINT)
}

/// Who started a rewrite.
#[derive(Clone, Copy, Debug)]
pub(super) enum Initiator {
    /// The model, through the `rewrite` tool, from inside the current step.
    Model,
    /// The harness, before the next step starts.
    Harness,
}

/// Reduce the Context before a round, in bare and full mode: each extension
/// in order with the request's [`Pressure`], then, while the request is still
/// over the threshold, the kernel's own Rewrite. When nothing changed, the
/// goal checkpoint. `true` when the log changed.
///
/// Over the threshold, `compaction.before` runs first, and `compaction.after`
/// once the kernel's Rewrite committed.
///
/// An extension that changed the log makes the recorded usage stale, so the
/// request is estimated again from the re-read projection.
pub async fn before_round(
    cx: RewriteCtx<'_>,
    extensions: &[Arc<dyn Rewriter>],
) -> Result<bool, ToolError> {
    let mut pressure = measure(&cx, cx.projection, false).await?;
    if let (Some(hooks), Some(pressure)) = (cx.hooks, pressure.filter(|p| p.over())) {
        hooks
            .before(
                cx.session,
                usize::try_from(pressure.used).unwrap_or(usize::MAX),
            )
            .await;
    }
    let mut reread: Option<Arc<Projection>> = None;
    for extension in extensions {
        let projection = reread.as_deref().unwrap_or(cx.projection);
        let changed = extension
            .before_round(RewriteCtx {
                projection,
                pressure,
                ..cx
            })
            .await?;
        if changed {
            let projection = cx.log.projection(cx.session).await?;
            pressure = measure(&cx, &projection, true).await?;
            reread = Some(projection);
        }
    }
    let projection = reread.as_deref().unwrap_or(cx.projection);
    if let Some(pressure) = pressure.filter(|pressure| pressure.over())
        && let Some(marker_tokens) = commit(&cx, projection, Some(pressure)).await?
    {
        if let Some(hooks) = cx.hooks {
            hooks.after(cx.session, marker_tokens).await;
        }
        return Ok(true);
    }
    if reread.is_some() {
        return Ok(true);
    }
    criteria_checkpoint(&cx, pressure).await
}

/// Reduce the Context now (`/summarize`): the first extension whose
/// `rewrite_now` changes the log ends it; otherwise the kernel's own Rewrite.
/// `true` when the log changed.
///
/// A reduction on request has no threshold: the pressure carries `0`, which
/// is how the log marks a manual reduction.
pub async fn rewrite_now(
    cx: RewriteCtx<'_>,
    extensions: &[Arc<dyn Rewriter>],
) -> Result<bool, ToolError> {
    let pressure = measure(&cx, cx.projection, false)
        .await?
        .map(|pressure| Pressure {
            threshold: 0,
            ..pressure
        });
    let cx = RewriteCtx { pressure, ..cx };
    for extension in extensions {
        if extension.rewrite_now(cx).await? {
            return Ok(true);
        }
    }
    Ok(commit(&cx, cx.projection, pressure).await?.is_some())
}

/// The size of the request `projection` yields against the threshold: the
/// largest of the last recorded round's `prompt + output`, the usage-anchored
/// count of the request, and its local estimate (what the host's pre-send
/// check measures). Once a reduction made the recorded usage `stale`, the
/// local estimate alone. `None` without a window.
async fn measure(
    cx: &RewriteCtx<'_>,
    projection: &Projection,
    stale: bool,
) -> Result<Option<Pressure>, ToolError> {
    let Some(window) = cx.window.filter(|window| window.max_context > 0) else {
        return Ok(None);
    };
    let messages = cx.transcript.messages(projection).await?;
    let accounting = TokenAccounting::default().with_tool_images(window.sends_tool_images);
    let mut tokens = accounting.estimate_with_reasoning_policy(&messages, window.reasoning_replay);
    if !stale {
        tokens = tokens.max(
            accounting
                .tokens_in_use_with_reasoning_policy(
                    &messages,
                    window.usage_reporting,
                    window.reasoning_replay,
                )
                .tokens,
        );
    }
    let mut used = tokens.saturating_add(cx.overhead_tokens) as u64;
    if !stale && let Some(last) = last_round(projection) {
        used = used.max(last);
    }
    Ok(Some(Pressure {
        used,
        threshold: threshold(window.max_context),
    }))
}

/// The kernel's own Rewrite of `projection` before the next step: the
/// marker's estimated tokens, or `None` when nothing was removable.
async fn commit(
    cx: &RewriteCtx<'_>,
    projection: &Projection,
    pressure: Option<Pressure>,
) -> Result<Option<usize>, ToolError> {
    let rewrite = rewrite_events(
        projection,
        cx.session,
        cx.message,
        Initiator::Harness,
        pressure,
    )
    .map_err(|error| ToolError::Other(error.to_string()))?;
    let Some((events, _)) = rewrite else {
        return Ok(None);
    };
    let marker_tokens = events
        .iter()
        .find_map(|event| match event {
            Event::TextDelta { delta, .. } => Some(delta.len() / 4),
            _ => None,
        })
        .unwrap_or_default();
    cx.log.append(cx.session, events).await?;
    Ok(Some(marker_tokens))
}

/// Ask the model, at most once per turn, to record the task's completion
/// criteria and their status in its todo list, when the request would fill
/// [`CRITERIA_CHECKPOINT_PERCENT`] of the window by `pressure`. The notice is a
/// system message in the log, so the request stays a function of the View.
/// Returns whether it was appended.
async fn criteria_checkpoint(
    cx: &RewriteCtx<'_>,
    pressure: Option<Pressure>,
) -> Result<bool, ToolError> {
    let (Some(pressure), Some(window)) = (pressure, cx.window) else {
        return Ok(false);
    };
    if pressure.used * 100 < u64::from(window.max_context) * CRITERIA_CHECKPOINT_PERCENT {
        return Ok(false);
    }
    let projection = cx.projection;
    let visible = compacted_messages(projection);
    if visible
        .first()
        .is_some_and(|row| is_marker(row) && row.id > cx.message)
        || projection
            .session
            .messages
            .iter()
            .rev()
            .take_while(|row| row.id != cx.message)
            .any(is_criteria_checkpoint)
    {
        return Ok(false);
    }
    let text = format!(
        "{CRITERIA_CHECKPOINT} earlier context will soon be removed; only the user messages and your todo list are kept verbatim. Record in the todo list every result you have derived and still need (values, findings, decisions), then continue the task. Anything not in the list will have to be worked out again."
    );
    cx.log
        .append(
            cx.session,
            system_message_events(cx.session, MessageId::new(), text),
        )
        .await?;
    Ok(true)
}

/// The rewrite threshold of a `max_context` window.
fn threshold(max_context: u32) -> u64 {
    u64::from(max_context) * TRIGGER_PERCENT / 100
}

/// The latest recorded round's `prompt + output` tokens.
fn last_round(projection: &Projection) -> Option<u64> {
    let last = projection
        .session
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::Assistant)
        .find_map(|message| message.usage.as_ref())?
        .last_round;
    Some(last.prompt().saturating_add(last.output))
}

/// The last recorded round against the threshold, which a model-initiated
/// rewrite records; `None` without usage or a window.
pub(super) fn recorded_pressure(
    projection: &Projection,
    max_context: Option<u32>,
) -> Option<Pressure> {
    let window = max_context.filter(|window| *window > 0)?;
    Some(Pressure {
        used: last_round(projection)?,
        threshold: threshold(window),
    })
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
    pressure: Option<Pressure>,
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
    let (input_tokens_est, threshold) =
        pressure.map_or((0, 0), |pressure| (pressure.used, pressure.threshold));
    let marker = MessageId::new();
    let todos = projection.session.todos.as_deref().unwrap_or_default();
    let mut events = system_message_events(session, marker, marker_text(initiator, &users, todos));
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
    marker.push_str("\n\n");
    marker.push_str(&marker_criteria(todos));
    marker
}
