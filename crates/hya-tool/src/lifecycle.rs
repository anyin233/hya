//! Subagent lifecycle plane (ADR-0015): the `report`, `archive`, and `wait`
//! tools ride a narrow request channel to the resident supervisor, mirroring
//! the mailbox plane's dependency inversion (`hya-tool` never sees
//! `CoreError`).
//!
//! `wait` has two implementations sharing this contract: the extended-tools
//! one wakes when subagents report or are archived (or stop without a
//! report); the channel-tools one (which overrides it whenever the channel
//! family is loaded, see `overrides` in the family exposure policy) also wakes
//! on new mail for the caller and marks the mail it returns as read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hya_proto::{ReportOutcome, SessionId, ToolSchema};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::mailbox::ChannelPolicySnapshot;
use crate::tool::ToolError;

/// One lifecycle request from a tool call to the supervisor.
#[derive(Debug)]
pub enum LifecycleRequest {
    /// Block until subagents finish (or mail arrives, or the timeout).
    /// Dropping the reply receiver aborts the wait.
    Wait {
        /// Waiting session.
        session: SessionId,
        /// Parsed request.
        spec: WaitSpec,
        /// Rejection text or the outcome.
        reply: oneshot::Sender<Result<WaitOutcome, String>>,
    },
    /// Accept a terminal report; archives once the actor is at rest.
    Report {
        /// Reporting session.
        session: SessionId,
        /// Terminal outcome.
        outcome: ReportOutcome,
        /// Result text for the parent.
        report: String,
        /// Channel policy captured by the admitted turn.
        channel_policy: Option<ChannelPolicySnapshot>,
        /// Gate rejection text or acceptance.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Stop and archive one of the caller's subagents (the `archive` tool).
    Archive {
        /// Archiving (ancestor) session.
        session: SessionId,
        /// The subagent: canonical handle, leaf, or session id.
        target: String,
        /// Short reason recorded with the archive.
        reason: String,
        /// Rejection text or what was archived.
        reply: oneshot::Sender<Result<ArchiveReceipt, String>>,
    },
}

/// Default `wait` timeout when the call names none.
pub const WAIT_DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Upper bound on one `wait`; longer requests are clamped.
pub const WAIT_MAX_TIMEOUT_SECS: u64 = 1800;

/// Whether a `wait` returns on the first finished target or on all of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    /// Return as soon as one target finished its current work.
    Any,
    /// Return once every target finished its current work.
    All,
}

/// A parsed `wait` call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitSpec {
    /// Handles, leaves, or session ids; empty = every live direct subagent.
    pub targets: Vec<String>,
    /// Any vs all.
    pub mode: WaitMode,
    /// Bounded wait; `0` only reports the current state.
    pub timeout: Duration,
    /// Also return when mail for the caller arrives (the channel-tools
    /// override).
    pub wake_on_mail: bool,
}

impl WaitSpec {
    /// Parse `wait` input: `targets` (string or string array; the strings
    /// `"any"`/`"all"` as the whole value select the mode over every live
    /// subagent), `mode` (`any`/`all`, default `all`), `timeout_secs`
    /// (default [`WAIT_DEFAULT_TIMEOUT_SECS`], clamped to
    /// [`WAIT_MAX_TIMEOUT_SECS`]).
    ///
    /// # Errors
    /// [`ToolError::Input`] naming the offending field and its valid shape.
    pub fn parse(input: &Value, wake_on_mail: bool) -> Result<Self, ToolError> {
        let mut mode = None;
        let mut targets = Vec::new();
        match input.get("targets").or_else(|| input.get("target")) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => match text.trim() {
                "" => {}
                "any" => mode = Some(WaitMode::Any),
                "all" => mode = Some(WaitMode::All),
                one => targets.push(one.to_string()),
            },
            Some(Value::Array(items)) => {
                for item in items {
                    let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty())
                    else {
                        return Err(ToolError::Input(
                            "wait `targets` must be subagent handles or session ids as strings, e.g. [\"main/hya-worker-exusiai\"]".to_string(),
                        ));
                    };
                    if !targets.iter().any(|known| known == text) {
                        targets.push(text.to_string());
                    }
                }
            }
            Some(_) => {
                return Err(ToolError::Input(
                    "wait `targets` must be a list of subagent handles or session ids (omit it to wait on all your live subagents)".to_string(),
                ));
            }
        }
        match input.get("mode") {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) if text.trim() == "any" => mode = Some(WaitMode::Any),
            Some(Value::String(text)) if text.trim() == "all" => mode = Some(WaitMode::All),
            Some(other) => {
                return Err(ToolError::Input(format!(
                    "wait `mode` must be \"any\" or \"all\", got {other}"
                )));
            }
        }
        let timeout = match input.get("timeout_secs").or_else(|| input.get("timeout")) {
            None | Some(Value::Null) => WAIT_DEFAULT_TIMEOUT_SECS,
            Some(value) => {
                let seconds = value
                    .as_f64()
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                    .ok_or_else(|| {
                        ToolError::Input(format!(
                            "wait `timeout_secs` must be a number of seconds between 0 and {WAIT_MAX_TIMEOUT_SECS}, got {value}"
                        ))
                    })?;
                // Clamped, then truncated to whole seconds.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let whole = seconds.min(WAIT_MAX_TIMEOUT_SECS as f64) as u64;
                whole
            }
        };
        Ok(Self {
            targets,
            mode: mode.unwrap_or(WaitMode::All),
            timeout: Duration::from_secs(timeout),
            wake_on_mail,
        })
    }
}

/// Model-facing schema of `wait`; `wake_on_mail` selects the channel-tools
/// description.
#[must_use]
pub fn wait_tool_schema(wake_on_mail: bool) -> ToolSchema {
    let description = if wake_on_mail {
        "Block until your subagents finish — a subagent finishes only when it calls `report` or is archived, never merely by going idle — OR new mail arrives for you (a message from a subagent or your parent, or a harness notice such as LEADER FAILED), whichever comes first, up to a timeout. Returns why it woke (`members`, `mail`, `stalled`, `timeout`, or `nothing_to_wait_for`), the targets that finished during this call with their reports, targets that had already finished before this call (`already_finished`), the ones still running, and the new mail (each message is returned once and then marked read). `stalled` means a subagent ended its turn without reporting and has nothing queued: it will not continue until you `send` it mail (or `archive` it). Use it instead of polling `list_channel` or sleeping. Cancelling your turn aborts the wait."
    } else {
        "Block until your subagents finish — a subagent finishes only when it calls `report` or is archived, never merely by going idle — up to a timeout. Returns why it woke (`members`, `stalled`, `timeout`, or `nothing_to_wait_for`), the targets that finished during this call with their reports, targets that had already finished before this call (`already_finished`), and the ones still running. `stalled` means a subagent ended its turn without reporting and has nothing queued: it will not continue until you mail it (or `archive` it). Use it instead of polling or sleeping. Cancelling your turn aborts the wait."
    };
    ToolSchema {
        name: hya_proto::ToolName::new("wait"),
        description: description.to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "targets": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Subagents to wait for: handles as returned by `task` (e.g. `main/hya-worker-exusiai`, or the leaf `hya-worker-exusiai`) or session ids. Omit to wait for all of your live direct subagents."
                },
                "mode": {
                    "type": "string",
                    "enum": ["all", "any"],
                    "description": "`all` (default): return when every target finished (reported or archived); `any`: return when the first one finished during this call."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": WAIT_MAX_TIMEOUT_SECS,
                    "description": "Give up after this many seconds (default 600, max 1800). 0 returns the current state at once without blocking."
                }
            },
            "required": []
        }),
        output_schema: None,
    }
}

/// Why a `wait` returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitWake {
    /// The target condition (`any`/`all`) was met by targets that reported or
    /// were archived during this call.
    Members,
    /// Mail for the caller arrived (channel-tools `wait` only).
    Mail,
    /// A target ended its turn without reporting and has nothing queued: it
    /// will not continue on its own. Reported once per stopped turn.
    Stalled,
    /// The timeout elapsed first (or `timeout_secs: 0` asked for a snapshot).
    Timeout,
    /// Nothing to wait for: no live subagents, or every target had already
    /// finished before this call (listed in `already_finished`).
    NothingToWaitFor,
}

/// Where one waited-on subagent stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMemberState {
    /// Delivered its terminal report (and was archived).
    Reported,
    /// Archived without a report (parent `archive`, drain, teardown).
    Archived,
    /// Live, its last turn ended without a report, and nothing is queued for
    /// it (no running turn, mail, directive, or live subagent): it will not
    /// continue until it is mailed. Never counts as finished.
    Idle,
    /// Running a turn, owing one, finishing an accepted report, or waiting on
    /// its own live subagents.
    Working,
}

/// One target's state in a [`WaitOutcome`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WaitMember {
    /// Canonical handle.
    pub handle: String,
    /// Session id.
    pub session: SessionId,
    /// Current state.
    pub state: WaitMemberState,
    /// `done` / `failed` for a report, `cancelled` for an archive, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The full report text (reported) or the archive note. Never an idle or
    /// working member's in-progress text. The tool result shows a bounded
    /// preview of it (see [`WaitOutcome::to_tool_result`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// The DM channel id between the caller and this member, when the
    /// caller is its parent: the report mail lives there, so the full text
    /// stays readable with `read channel://<id>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// Most serialized characters one `wait` tool result may take — the whole
/// `{title, output, metadata}` envelope as the generic output cap measures it
/// — kept well under [`crate::MAX_TOOL_OUTPUT_CHARS`] so the cap never
/// replaces the result with its tail.
pub const WAIT_RESULT_BUDGET: usize = 4500;

/// One new mail message returned by a channel-aware `wait`. Each message is
/// returned once: the wait advances the caller's durable inbox cursor
/// (`MailConsumed`), so neither a later `wait` nor the `[NEW MAIL]` steer
/// notice repeats it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WaitMail {
    /// Sender handle (`harness` for control notices).
    pub from: String,
    /// Channel id it arrived on, when channel-addressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The message body, bounded (600 chars); the full history stays readable
    /// with `read channel://<id>`.
    pub preview: String,
}

/// Structured result of one `wait`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WaitOutcome {
    /// Why the wait returned.
    pub woke_by: WaitWake,
    /// Targets that reported or were archived during this call.
    pub finished: Vec<WaitMember>,
    /// Targets that had already reported or been archived before this call
    /// (and were not woken again since): listed for reference, never a wake.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub already_finished: Vec<WaitMember>,
    /// Targets still working, or stopped without a report (`idle`).
    pub running: Vec<WaitMember>,
    /// New mail for the caller (channel-aware wait only), returned once.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mail: Vec<WaitMail>,
    /// Wall-clock milliseconds spent waiting.
    pub waited_ms: u64,
}

/// One report or mail body shown in the preview section.
struct PreviewBody<'a> {
    /// The member whose report this is (`None` for mail).
    member: Option<&'a str>,
    heading: String,
    text: &'a str,
    /// `read channel://<id>` target holding the full text, if readable.
    channel: Option<&'a str>,
}

impl WaitOutcome {
    /// Tool result JSON: `{title, output, metadata}`.
    ///
    /// Self-budgeted to [`WAIT_RESULT_BUDGET`] serialized characters so the
    /// generic output cap never cuts it. `output` leads with a compact header
    /// — why the wait woke, one line per target (handle, state, outcome),
    /// the still-running handles, and the new-mail count — then bounded
    /// previews of every report and mail body, sharing what is left of the
    /// budget; a cut preview ends with where the full text is readable
    /// (`read channel://<DM id>`) when the caller can read it. `metadata`
    /// carries the structured outcome without the bodies (`report_chars` and
    /// `report_truncated` instead of the text).
    #[must_use]
    pub fn to_tool_result(&self) -> Value {
        let bodies = self.preview_bodies();
        let lengths: Vec<usize> = bodies
            .iter()
            .map(|body| body.text.chars().count())
            .collect();
        let size = |result: &Value| result.to_string().chars().count();
        // Everything but the body text (header, headings, pointers, metadata).
        let bare = size(&self.render(&bodies, &vec![0; bodies.len()]));
        let mut budget = WAIT_RESULT_BUDGET
            .saturating_sub(bare)
            .min(lengths.iter().sum());
        loop {
            let result = self.render(&bodies, &water_fill(&lengths, budget));
            // JSON escaping can grow the text past its char count: shrink
            // until the whole envelope fits.
            if budget == 0 || size(&result) <= WAIT_RESULT_BUDGET {
                return result;
            }
            budget = budget * 9 / 10;
        }
    }

    fn preview_bodies(&self) -> Vec<PreviewBody<'_>> {
        let mut bodies = Vec::new();
        for member in self.finished.iter().chain(&self.already_finished) {
            if let Some(report) = &member.report {
                let heading = match (&member.outcome, member.state) {
                    (Some(outcome), WaitMemberState::Reported) => {
                        format!("Report from {} ({outcome}):", member.handle)
                    }
                    (_, WaitMemberState::Reported) => format!("Report from {}:", member.handle),
                    _ => format!("Archive note for {}:", member.handle),
                };
                bodies.push(PreviewBody {
                    member: Some(member.handle.as_str()),
                    heading,
                    text: report.as_str(),
                    channel: member.channel.as_deref(),
                });
            }
        }
        for mail in &self.mail {
            let channel = mail
                .channel
                .as_ref()
                .map_or(String::new(), |id| format!(" @{id}"));
            bodies.push(PreviewBody {
                member: None,
                heading: format!("Mail from {}{channel}:", mail.from),
                text: mail.preview.as_str(),
                channel: mail.channel.as_deref(),
            });
        }
        bodies
    }

    fn render(&self, bodies: &[PreviewBody<'_>], shown: &[usize]) -> Value {
        let mut lines = Vec::new();
        let headline = match self.woke_by {
            WaitWake::Members => "Subagents finished.",
            WaitWake::Mail => "Mail arrived for you.",
            WaitWake::Stalled => {
                "A subagent stopped without reporting: its turn ended and nothing is queued for it, so it will not continue until you `send` it mail (or `archive` it)."
            }
            WaitWake::Timeout => "Timed out; some subagents are still running.",
            WaitWake::NothingToWaitFor if self.already_finished.is_empty() => {
                "Nothing to wait for: you have no live subagents."
            }
            WaitWake::NothingToWaitFor => {
                "Nothing to wait for: every target already finished before this call (reports below); calling `wait` again will not change that."
            }
        };
        lines.push(headline.to_string());
        let member_line = |member: &WaitMember, tag: &str| {
            let mut line = format!("- {} [{tag}]", member.handle);
            if let Some(outcome) = &member.outcome {
                line.push_str(&format!(" {outcome}"));
            }
            line
        };
        for member in &self.finished {
            lines.push(member_line(member, state_label(member.state)));
        }
        for member in &self.already_finished {
            let tag = format!("already {}", state_label(member.state));
            lines.push(member_line(member, &tag));
        }
        for member in &self.running {
            if member.state == WaitMemberState::Idle {
                lines.push(format!(
                    "- {} [idle: ended its turn without `report`; nothing queued]",
                    member.handle
                ));
            }
        }
        let working: Vec<&str> = self
            .running
            .iter()
            .filter(|member| member.state == WaitMemberState::Working)
            .map(|member| member.handle.as_str())
            .collect();
        if !working.is_empty() {
            lines.push(format!("Still running: {}", working.join(", ")));
        }
        if !self.mail.is_empty() {
            lines.push(format!(
                "{} new mail message(s), now marked read (history: read channel://<id>?last=N).",
                self.mail.len()
            ));
        }
        for (body, &limit) in bodies.iter().zip(shown) {
            lines.push(String::new());
            lines.push(body.heading.clone());
            let total = body.text.chars().count();
            if limit >= total {
                lines.push(body.text.to_string());
                continue;
            }
            let cut: String = body.text.chars().take(limit).collect();
            let omitted = total - limit;
            let pointer = body.channel.map_or(String::new(), |id| {
                format!("; full text: read channel://{id}")
            });
            lines.push(format!("{cut}\n[… {omitted} more chars{pointer}]"));
        }

        let member_meta = |member: &WaitMember| {
            let mut meta = json!({
                "handle": member.handle,
                "session": member.session,
                "state": member.state,
            });
            if let Some(outcome) = &member.outcome {
                meta["outcome"] = json!(outcome);
            }
            if let Some(channel) = &member.channel {
                meta["channel"] = json!(channel);
            }
            if let Some(report) = &member.report {
                let chars = report.chars().count();
                let shown = bodies
                    .iter()
                    .zip(shown)
                    .find(|(body, _)| body.member == Some(member.handle.as_str()))
                    .map_or(chars, |(_, shown)| *shown);
                meta["report_chars"] = json!(chars);
                meta["report_truncated"] = json!(shown < chars);
            }
            meta
        };
        let mut metadata = json!({
            "woke_by": self.woke_by,
            "finished": self.finished.iter().map(member_meta).collect::<Vec<_>>(),
            "running": self.running.iter().map(member_meta).collect::<Vec<_>>(),
            "waited_ms": self.waited_ms,
        });
        if !self.already_finished.is_empty() {
            metadata["already_finished"] = json!(
                self.already_finished
                    .iter()
                    .map(member_meta)
                    .collect::<Vec<_>>()
            );
        }
        if !self.mail.is_empty() {
            metadata["mail"] = json!(
                self.mail
                    .iter()
                    .map(|mail| {
                        let mut meta = json!({
                            "from": mail.from,
                            "chars": mail.preview.chars().count(),
                        });
                        if let Some(channel) = &mail.channel {
                            meta["channel"] = json!(channel);
                        }
                        meta
                    })
                    .collect::<Vec<_>>()
            );
        }
        json!({
            "title": format!("wait: {}", match self.woke_by {
                WaitWake::Members => "members",
                WaitWake::Mail => "mail",
                WaitWake::Stalled => "stalled",
                WaitWake::Timeout => "timeout",
                WaitWake::NothingToWaitFor => "nothing to wait for",
            }),
            "output": lines.join("\n"),
            "metadata": metadata,
        })
    }
}

/// Split `budget` characters across bodies of `lengths`: short bodies are
/// shown whole, and what they leave is shared equally by the longer ones.
fn water_fill(lengths: &[usize], budget: usize) -> Vec<usize> {
    let mut shown = vec![0; lengths.len()];
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&index| lengths[index]);
    let mut remaining = budget;
    for (position, &index) in order.iter().enumerate() {
        let share = remaining / (order.len() - position);
        shown[index] = lengths[index].min(share);
        remaining -= shown[index];
    }
    shown
}

fn state_label(state: WaitMemberState) -> &'static str {
    match state {
        WaitMemberState::Reported => "reported",
        WaitMemberState::Archived => "archived",
        WaitMemberState::Idle => "idle",
        WaitMemberState::Working => "working",
    }
}

/// What one `archive` call stopped and archived.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ArchiveReceipt {
    /// Canonical handle of the archived subagent.
    pub handle: String,
    /// Its session id (unchanged when it is woken again).
    pub session: SessionId,
    /// Whether an in-flight turn was cancelled (`cause: archived`).
    pub cancelled_turn: bool,
    /// Live descendants archived first (deepest first), if any.
    pub descendants: Vec<String>,
}

/// Rejection text for a second `report` inside the turn whose report was
/// already accepted.
pub const REPORT_ALREADY_ACCEPTED: &str = "your report was already accepted in this episode and your turn ends after this tool round; do not call `report` again. If your parent mails you later you are woken for a new episode and may report once more.";

/// Turn-scoped marker the engine hands to every `report` call of one turn.
///
/// It is set when a `report` is accepted; the engine then ends the turn right
/// after the current tool round (no further model call), and a second
/// `report` in the same turn is rejected with [`REPORT_ALREADY_ACCEPTED`]
/// without reaching the supervisor.
#[derive(Clone, Debug, Default)]
pub struct ReportLatch(Arc<AtomicBool>);

impl ReportLatch {
    /// A fresh, unset latch (one per turn).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a `report` was accepted in this turn.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Session-scoped facade tools use to request lifecycle transitions.
#[derive(Clone)]
pub struct LifecyclePlane {
    tx: Option<mpsc::UnboundedSender<LifecycleRequest>>,
    session: Option<SessionId>,
    channel_policy: Option<ChannelPolicySnapshot>,
    report_latch: Option<ReportLatch>,
}

impl Default for LifecyclePlane {
    fn default() -> Self {
        Self::disconnected()
    }
}

impl LifecyclePlane {
    /// Build a connected plane plus the receiver the service loop drains.
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<LifecycleRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: Some(tx),
                session: None,
                channel_policy: None,
                report_latch: None,
            },
            rx,
        )
    }

    /// A plane whose sends fail fast (tests, disconnected engines).
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            tx: None,
            session: None,
            channel_policy: None,
            report_latch: None,
        }
    }

    /// Scope requests to the acting session.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    /// Attach the channel policy captured by the same admitted turn.
    #[must_use]
    pub fn with_channel_policy(mut self, policy: ChannelPolicySnapshot) -> Self {
        self.channel_policy = Some(policy);
        self
    }

    /// Attach the turn's report latch: an accepted `report` sets it (the
    /// engine ends the turn after the round) and a second `report` in the
    /// same turn is rejected.
    #[must_use]
    pub fn with_report_latch(mut self, latch: ReportLatch) -> Self {
        self.report_latch = Some(latch);
        self
    }

    fn session(&self) -> Result<SessionId, ToolError> {
        self.session
            .ok_or_else(|| ToolError::Other("lifecycle tool requires a session".to_string()))
    }

    fn tx(&self) -> Result<&mpsc::UnboundedSender<LifecycleRequest>, ToolError> {
        self.tx.as_ref().ok_or_else(|| {
            ToolError::Other("lifecycle actions are unavailable outside a running team".to_string())
        })
    }

    /// Submit a terminal report (ADR-0015). Once accepted, the turn's
    /// [`ReportLatch`] is set so the engine ends the turn after this round.
    ///
    /// # Errors
    /// Gate rejections, and a second report in a turn whose report was
    /// already accepted, surface as [`ToolError::Input`].
    pub async fn report(&self, outcome: ReportOutcome, report: String) -> Result<(), ToolError> {
        if self.report_latch.as_ref().is_some_and(ReportLatch::is_set) {
            return Err(ToolError::Input(REPORT_ALREADY_ACCEPTED.to_string()));
        }
        let session = self.session()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx()?
            .send(LifecycleRequest::Report {
                session,
                outcome,
                report,
                channel_policy: self.channel_policy,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::Other("lifecycle service unavailable".to_string()))?;
        flatten(reply_rx).await?;
        if let Some(latch) = &self.report_latch {
            latch.set();
        }
        Ok(())
    }

    /// Stop and archive one of the caller's subagents.
    ///
    /// # Errors
    /// Rejections (unknown target, already archived, the team lead, not a
    /// descendant of the caller) surface as [`ToolError::Input`].
    pub async fn archive(
        &self,
        target: String,
        reason: String,
    ) -> Result<ArchiveReceipt, ToolError> {
        let session = self.session()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx()?
            .send(LifecycleRequest::Archive {
                session,
                target,
                reason,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::Other("lifecycle service unavailable".to_string()))?;
        flatten(reply_rx).await
    }
}

impl LifecyclePlane {
    /// Wait for subagents per `spec`; `cancel` (the tool call's token) aborts
    /// promptly with [`ToolError::Cancelled`] and drops the request.
    ///
    /// # Errors
    /// Unknown or foreign targets surface as [`ToolError::Input`].
    pub async fn wait(
        &self,
        spec: WaitSpec,
        cancel: &CancellationToken,
    ) -> Result<WaitOutcome, ToolError> {
        let session = self.session()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx()?
            .send(LifecycleRequest::Wait {
                session,
                spec,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::Other("lifecycle service unavailable".to_string()))?;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ToolError::Cancelled),
            result = flatten(reply_rx) => result,
        }
    }
}

async fn flatten<T>(rx: oneshot::Receiver<Result<T, String>>) -> Result<T, ToolError> {
    match rx.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(ToolError::Input(message)),
        Err(_) => Err(ToolError::Other("lifecycle service dropped".to_string())),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn report_round_trips_through_the_plane() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let task = tokio::spawn(async move {
            plane
                .report(ReportOutcome::Done, "shipped".to_string())
                .await
        });
        match rx.recv().await.expect("request") {
            LifecycleRequest::Report {
                outcome,
                report,
                reply,
                ..
            } => {
                assert_eq!(outcome, ReportOutcome::Done);
                assert_eq!(report, "shipped");
                reply.send(Ok(())).expect("reply");
            }
            other => panic!("expected report, got {other:?}"),
        }
        task.await.unwrap().expect("accepted");
    }

    #[tokio::test]
    async fn gate_rejections_surface_as_input_errors() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let task =
            tokio::spawn(async move { plane.report(ReportOutcome::Done, "x".to_string()).await });
        if let LifecycleRequest::Report { reply, .. } = rx.recv().await.expect("request") {
            reply
                .send(Err("report rejected: unread mail".to_string()))
                .unwrap();
        }
        let error = task.await.unwrap().unwrap_err();
        assert!(matches!(error, ToolError::Input(message) if message.contains("unread")));
    }

    #[tokio::test]
    async fn an_accepted_report_sets_the_latch_and_a_second_is_rejected_locally() {
        let (plane, mut rx) = LifecyclePlane::new();
        let latch = ReportLatch::new();
        let plane = plane
            .for_session(SessionId::new())
            .with_report_latch(latch.clone());
        let rejected_plane = plane.clone();
        let task =
            tokio::spawn(
                async move { plane.report(ReportOutcome::Done, "first".to_string()).await },
            );
        if let LifecycleRequest::Report { reply, .. } = rx.recv().await.expect("request") {
            reply.send(Ok(())).unwrap();
        }
        task.await.unwrap().expect("accepted");
        assert!(latch.is_set());
        let error = rejected_plane
            .report(ReportOutcome::Done, "second".to_string())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Input(message) if message == REPORT_ALREADY_ACCEPTED),
            "{error:?}"
        );
        assert!(
            rx.try_recv().is_err(),
            "the second report never reaches the supervisor"
        );
    }

    #[tokio::test]
    async fn a_rejected_report_leaves_the_latch_unset() {
        let (plane, mut rx) = LifecyclePlane::new();
        let latch = ReportLatch::new();
        let plane = plane
            .for_session(SessionId::new())
            .with_report_latch(latch.clone());
        let task =
            tokio::spawn(async move { plane.report(ReportOutcome::Done, "x".to_string()).await });
        if let LifecycleRequest::Report { reply, .. } = rx.recv().await.expect("request") {
            reply.send(Err("unread mail".to_string())).unwrap();
        }
        assert!(task.await.unwrap().is_err());
        assert!(!latch.is_set(), "a gate rejection must not end the turn");
    }

    #[tokio::test]
    async fn disconnected_plane_fails_fast() {
        let error = LifecyclePlane::disconnected()
            .report(ReportOutcome::Done, "x".to_string())
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Other(_)));
    }

    #[tokio::test]
    async fn archive_round_trips_the_receipt() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let child = SessionId::new();
        let task = tokio::spawn(async move {
            plane
                .archive("main/x-1".to_string(), "stuck".to_string())
                .await
        });
        match rx.recv().await.expect("request") {
            LifecycleRequest::Archive {
                target,
                reason,
                reply,
                ..
            } => {
                assert_eq!(target, "main/x-1");
                assert_eq!(reason, "stuck");
                reply
                    .send(Ok(ArchiveReceipt {
                        handle: target,
                        session: child,
                        cancelled_turn: true,
                        descendants: Vec::new(),
                    }))
                    .expect("reply");
            }
            other => panic!("expected archive, got {other:?}"),
        }
        let receipt = task.await.unwrap().expect("archived");
        assert_eq!(receipt.session, child);
        assert!(receipt.cancelled_turn);

        let unscoped = LifecyclePlane::new().0;
        assert!(matches!(
            unscoped.archive("x".to_string(), String::new()).await,
            Err(ToolError::Other(_))
        ));
    }

    #[test]
    fn wait_spec_defaults_clamps_and_names_bad_fields() {
        let spec = WaitSpec::parse(&json!({}), false).unwrap();
        assert!(spec.targets.is_empty());
        assert_eq!(spec.mode, WaitMode::All);
        assert_eq!(spec.timeout, Duration::from_secs(WAIT_DEFAULT_TIMEOUT_SECS));
        assert!(!spec.wake_on_mail);

        let spec = WaitSpec::parse(
            &json!({"targets": ["main/a-1", "main/a-1", "b-2"], "mode": "any", "timeout_secs": 99_999}),
            true,
        )
        .unwrap();
        assert_eq!(spec.targets, ["main/a-1", "b-2"]);
        assert_eq!(spec.mode, WaitMode::Any);
        assert_eq!(spec.timeout, Duration::from_secs(WAIT_MAX_TIMEOUT_SECS));
        assert!(spec.wake_on_mail);

        // "any"/"all" as the whole targets value selects the mode.
        let spec = WaitSpec::parse(&json!({"targets": "any"}), false).unwrap();
        assert!(spec.targets.is_empty());
        assert_eq!(spec.mode, WaitMode::Any);
        assert_eq!(
            WaitSpec::parse(&json!({"timeout_secs": 0}), false)
                .unwrap()
                .timeout,
            Duration::ZERO
        );

        for (input, field) in [
            (json!({"mode": "some"}), "mode"),
            (json!({"timeout_secs": -1}), "timeout_secs"),
            (json!({"targets": [1]}), "targets"),
            (json!({"targets": {"a": 1}}), "targets"),
        ] {
            let error = WaitSpec::parse(&input, false).unwrap_err();
            assert!(
                matches!(&error, ToolError::Input(message) if message.contains(field)),
                "{input}: {error:?}"
            );
        }
    }

    fn member(handle: &str, state: WaitMemberState, report: Option<&str>) -> WaitMember {
        WaitMember {
            handle: handle.to_string(),
            session: SessionId::new(),
            state,
            outcome: (state == WaitMemberState::Reported).then(|| "done".to_string()),
            report: report.map(str::to_string),
            channel: None,
        }
    }

    /// Run 6 (seq 173427): four reports overflowed the generic cap, which
    /// kept only the tail. The rendered result budgets itself: the header
    /// (why it woke, every target, the mail count) always leads, every report
    /// gets a bounded preview ending in a readable pointer, and the whole
    /// envelope fits under the cap so its metadata survives intact.
    #[test]
    fn a_wait_with_four_long_reports_keeps_its_header_and_metadata_under_the_cap() {
        let report = |n: usize| format!("REPORT_{n}_START {} REPORT_{n}_END", "x".repeat(3000));
        let finished: Vec<WaitMember> = (1..=4)
            .map(|n| WaitMember {
                channel: Some(format!("DM-0000000{n}")),
                ..member(
                    &format!("main/dev-worker-{n}"),
                    WaitMemberState::Reported,
                    Some(&report(n)),
                )
            })
            .collect();
        let outcome = WaitOutcome {
            woke_by: WaitWake::Members,
            finished,
            already_finished: vec![member(
                "main/scout-a",
                WaitMemberState::Reported,
                Some("SHORT_REPORT"),
            )],
            running: vec![member("main/reviewer-b", WaitMemberState::Working, None)],
            mail: vec![WaitMail {
                from: "main/reviewer-b".to_string(),
                channel: Some("DM-0000000b".to_string()),
                preview: "m".repeat(600),
            }],
            waited_ms: 7,
        };
        let result = outcome.to_tool_result();
        let serialized = result.to_string().chars().count();
        assert!(
            serialized <= WAIT_RESULT_BUDGET && WAIT_RESULT_BUDGET < crate::MAX_TOOL_OUTPUT_CHARS,
            "{serialized} chars"
        );
        assert_eq!(
            crate::cap_tool_output(result.clone()),
            result,
            "the generic cap leaves the wait result untouched"
        );

        let output = result["output"].as_str().unwrap();
        assert!(output.starts_with("Subagents finished.\n"), "{output}");
        let header_end = output.find("REPORT_1_START").unwrap();
        let header = &output[..header_end];
        for n in 1..=4 {
            assert!(
                header.contains(&format!("- main/dev-worker-{n} [reported] done")),
                "{header}"
            );
            assert!(output.contains(&format!("REPORT_{n}_START")), "{output}");
            assert!(
                output.contains(&format!("read channel://DM-0000000{n}")),
                "every truncated preview names where the full report is: {output}"
            );
        }
        assert!(header.contains("- main/scout-a [already reported] done"));
        assert!(
            header.contains("Still running: main/reviewer-b"),
            "{header}"
        );
        assert!(header.contains("1 new mail message"), "{header}");
        assert!(
            output.contains("SHORT_REPORT"),
            "a short report is shown whole"
        );
        assert!(
            !output.contains("REPORT_1_END"),
            "long reports are previews"
        );

        let metadata = &result["metadata"];
        assert_eq!(metadata["woke_by"], "members");
        assert_eq!(metadata["waited_ms"], 7);
        assert_eq!(metadata["finished"].as_array().unwrap().len(), 4);
        let first = &metadata["finished"][0];
        assert_eq!(first["handle"], "main/dev-worker-1");
        assert_eq!(first["state"], "reported");
        assert_eq!(first["outcome"], "done");
        assert_eq!(first["channel"], "DM-00000001");
        assert_eq!(first["report_chars"], report(1).chars().count());
        assert_eq!(first["report_truncated"], true);
        assert!(
            first.get("report").is_none(),
            "report bodies live in output"
        );
        assert_eq!(metadata["already_finished"][0]["report_truncated"], false);
        assert_eq!(metadata["running"][0]["state"], "working");
        assert_eq!(metadata["mail"][0]["from"], "main/reviewer-b");
        assert_eq!(metadata["mail"][0]["channel"], "DM-0000000b");
    }

    /// Without a DM channel the preview does not invent a pointer.
    #[test]
    fn a_truncated_report_without_a_readable_channel_names_no_pointer() {
        let outcome = WaitOutcome {
            woke_by: WaitWake::Members,
            finished: vec![member(
                "main/deep-a",
                WaitMemberState::Reported,
                Some(&"y".repeat(9000)),
            )],
            already_finished: Vec::new(),
            running: Vec::new(),
            mail: Vec::new(),
            waited_ms: 0,
        };
        let result = outcome.to_tool_result();
        assert!(result.to_string().chars().count() <= WAIT_RESULT_BUDGET);
        let output = result["output"].as_str().unwrap();
        assert!(!output.contains("channel://"), "{output}");
        assert!(output.contains("more chars"), "{output}");
    }

    #[test]
    fn wait_result_flags_already_finished_stalled_and_read_mail() {
        let already = WaitOutcome {
            woke_by: WaitWake::NothingToWaitFor,
            finished: Vec::new(),
            already_finished: vec![member("main/a-1", WaitMemberState::Reported, Some("A"))],
            running: Vec::new(),
            mail: Vec::new(),
            waited_ms: 0,
        }
        .to_tool_result();
        let output = already["output"].as_str().unwrap();
        assert!(output.contains("every target already finished"), "{output}");
        assert!(
            output.contains("- main/a-1 [already reported] done\n"),
            "{output}"
        );
        assert!(
            output.contains("Report from main/a-1 (done):\nA"),
            "{output}"
        );
        assert_eq!(already["metadata"]["woke_by"], "nothing_to_wait_for");
        assert_eq!(
            already["metadata"]["already_finished"][0]["state"],
            "reported"
        );

        let stalled = WaitOutcome {
            woke_by: WaitWake::Stalled,
            finished: Vec::new(),
            already_finished: Vec::new(),
            running: vec![
                member("main/b-1", WaitMemberState::Idle, None),
                member("main/c-1", WaitMemberState::Working, None),
            ],
            mail: vec![WaitMail {
                from: "main/c-1".to_string(),
                channel: Some("DM-x".to_string()),
                preview: "status".to_string(),
            }],
            waited_ms: 5,
        }
        .to_tool_result();
        let output = stalled["output"].as_str().unwrap();
        assert_eq!(stalled["title"], "wait: stalled");
        assert!(output.contains("stopped without reporting"), "{output}");
        assert!(
            output.contains("- main/b-1 [idle: ended its turn without `report`"),
            "{output}"
        );
        assert!(output.contains("Still running: main/c-1"), "{output}");
        assert!(!output.contains("Still running: main/b-1"), "{output}");
        assert!(output.contains("marked read"), "{output}");
        assert!(stalled["metadata"].get("already_finished").is_none());
    }

    #[test]
    fn wait_schema_description_depends_on_the_mail_wake() {
        assert!(wait_tool_schema(true).description.contains("mail arrives"));
        assert!(!wait_tool_schema(false).description.contains("mail arrives"));
        assert_eq!(wait_tool_schema(false).name.as_str(), "wait");
    }

    #[tokio::test]
    async fn cancelling_the_tool_call_aborts_the_wait_and_drops_the_request() {
        let (plane, mut rx) = LifecyclePlane::new();
        let plane = plane.for_session(SessionId::new());
        let cancel = CancellationToken::new();
        let waiting = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                plane
                    .wait(WaitSpec::parse(&json!({}), false).unwrap(), &cancel)
                    .await
            })
        };
        let Some(LifecycleRequest::Wait { reply, .. }) = rx.recv().await else {
            panic!("expected a wait request");
        };
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("cancel aborts promptly")
            .unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
        assert!(reply.is_closed(), "the service sees the wait was abandoned");
    }
}
