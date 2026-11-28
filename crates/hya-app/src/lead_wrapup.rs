//! The lead of a headless root run (`exec`) must not end the run while
//! it still has work in flight. Nobody is there to answer a turn that stops
//! early: when the lead's turn ends on its own with live subagents (stopped
//! and lost when the run ends), unread mail, or a last message that
//! announces a next step it did not take, the run gives it another turn with
//! a harness notice naming what is left, until a turn ends with none of them.
//! In a budgeted run the deadline bounds this: past the grace no turn starts,
//! and in the final window one ignored notice ends the run.

use anyhow::Context as _;
use hya_engine::{AgentSpec, CoreError, ResidentSupervisor, SessionEngine};
use hya_proto::{
    FinishReason, MessageId, PartProjection, Projection, Role, RosterStatus, SessionId, scope,
};
use hya_tool::deadline::{RunDeadline, Stage};
use hya_tool::extension::{RosterEntry, members_projection, team_projection, workflow_projection};
use tokio_util::sync::CancellationToken;

/// Notice turns in a row that may end with the same pending work and no tool
/// call before the run ends anyway: a model can ignore the notice.
const MAX_IGNORED_NOTICES: u32 = 2;
/// Notices a run without a time budget may inject: a lead that keeps doing a
/// little and stopping again is otherwise unbounded. A budget bounds it.
const MAX_NOTICES_WITHOUT_BUDGET: u32 = 12;
/// Longest final paragraph the narration rule still reads as an announcement.
const MAX_ANNOUNCEMENT_CHARS: usize = 400;
/// Characters of the announcing paragraph quoted back in the notice.
const QUOTE_CHARS: usize = 200;

/// Admit the prompt and run the headless lead through its continuation notices.
///
/// # Errors
/// Returns an error if prompt admission, a deadline notice, or the turn fails.
pub async fn run_headless_lead(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    session: SessionId,
    agent: &AgentSpec,
    cancel: CancellationToken,
    prompt: String,
) -> anyhow::Result<FinishReason> {
    let prompt = engine
        .admit_user_prompt(session, prompt)
        .await
        .context("admit prompt")?;
    if let Some(deadline) = hya_tool::deadline::current() {
        engine
            .inject_system_message(session, deadline.opening())
            .await
            .context("announce the time budget")?;
    }
    run_lead(engine, supervisor, session, agent, cancel, prompt)
        .await
        .context("run turn")
}

/// Run the lead's turn, then one more turn per notice while it ends with
/// [`Pending`] work (bounded by [`MAX_IGNORED_NOTICES`], and by `deadline`
/// or else [`MAX_NOTICES_WITHOUT_BUDGET`]). Returns the last turn's finish;
/// a cancelled or failed turn ends the run at once.
async fn run_lead(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    session: SessionId,
    agent: &AgentSpec,
    cancel: CancellationToken,
    initial_prompt: MessageId,
) -> Result<FinishReason, CoreError> {
    let _driver = supervisor.root_driver_activity(session).await?;
    let (mut ignored, mut notices) = (0u32, 0u32);
    let mut last_notice: Option<MessageId> = None;
    let mut initial_prompt = Some(initial_prompt);
    let mut completion_checked = false;
    let mut ran_known_tool = false;
    let deadline = hya_tool::deadline::current();
    loop {
        let finish = engine
            .run_headless_lead_turn(
                session,
                agent,
                cancel.clone(),
                initial_prompt.take(),
                &mut ran_known_tool,
            )
            .await?;
        if !matches!(finish, FinishReason::Stop | FinishReason::Length) {
            return Ok(finish);
        }
        let stage = deadline.map_or(Stage::Normal, RunDeadline::stage);
        if stage == Stage::Over {
            return Ok(finish);
        }
        let projection = engine.read_projection_shared(session).await?;
        let pending = Pending::of(&projection, finish);
        if pending.is_empty() {
            if !completion_checked
                && deadline.is_some_and(|d| d.remaining() > 2 * d.final_window())
                && ran_known_tool
                && !projection.session.messages.iter().any(|row| {
                    row.role == Role::System
                        && row.parts.iter().any(|part| {
                            matches!(part, PartProjection::Text { text, .. }
                        if hya_engine::kernel::is_completion_check(text))
                        })
                })
                && !cancel.is_cancelled()
            {
                completion_checked = true;
                engine
                    .inject_system_message(
                        session,
                        hya_engine::kernel::COMPLETION_CHECK.to_string(),
                    )
                    .await?;
                ignored = 0;
                last_notice = None;
                continue;
            }
            return Ok(finish);
        }
        if let Some(notice) = last_notice {
            if called_any_tool(&projection, notice) {
                ignored = 0;
            } else {
                ignored += 1;
            }
            let allowed = if stage == Stage::Final {
                1
            } else {
                MAX_IGNORED_NOTICES
            };
            if ignored >= allowed {
                return Ok(finish);
            }
        }
        if deadline.is_none() && notices >= MAX_NOTICES_WITHOUT_BUDGET {
            return Ok(finish);
        }
        notices += 1;
        let time = deadline.and_then(|deadline| {
            deadline
                .take_notice(session)
                .map(|stage| deadline.notice(stage, false))
        });
        let notice = pending.notice(time.as_deref(), stage >= Stage::WrapUp);
        drop(projection);
        last_notice = Some(engine.inject_system_message(session, notice).await?);
    }
}

/// Why a lead turn that ended on its own must not end the run yet.
struct Pending {
    subagents: Vec<RosterEntry>,
    unread_mail: u64,
    narration: Option<Narration>,
}

/// The last step answered in text that leaves its work unfinished.
#[derive(Debug, PartialEq, Eq)]
enum Narration {
    /// The output hit the length limit.
    Truncated,
    /// The final paragraph announces a step it did not take (quoted).
    Announces(String),
    /// A tool call escaped into text, so no tool executed it.
    MalformedToolMarkup(String),
    /// Internal compaction prose escaped as the final answer.
    SpuriousSummary,
    /// No text and no tool call: nothing was delivered.
    Empty,
}

impl Pending {
    fn of(projection: &Projection, finish: FinishReason) -> Self {
        Self {
            subagents: live_subagents(projection),
            unread_mail: unread_root_mail(projection),
            narration: narration(projection, finish),
        }
    }

    fn is_empty(&self) -> bool {
        self.subagents.is_empty() && self.unread_mail == 0 && self.narration.is_none()
    }

    /// One notice listing every reason, most costly to leave first, after
    /// the deadline notice `time` of a late stage.
    fn notice(&self, time: Option<&str>, late_stage: bool) -> String {
        let mut notice = String::new();
        if let Some(time) = time {
            notice.push_str(time);
            notice.push_str("\n\n");
        }
        notice.push_str("HEADLESS RUN — your turn ended, but the run is not finished:\n");
        if !self.subagents.is_empty() {
            notice.push_str(
                "Subagents you started are still live (stopped and their work lost when the run ends):\n",
            );
            for entry in &self.subagents {
                notice.push_str(&subagent_line(entry));
                notice.push('\n');
            }
            notice.push_str(if late_stage {
                "  → `archive` the subagents whose work you cannot integrate in the time left; `wait` briefly for the rest.\n"
            } else {
                "  → call `wait` to collect their reports and integrate the results, or `archive` the ones you no longer need.\n"
            });
        }
        if self.unread_mail != 0 {
            notice.push_str(&format!("Your inbox has {} unread message(s).\n  → Read and integrate the new mail before completing the run.\n", self.unread_mail));
        }
        match &self.narration {
            Some(Narration::Announces(quote)) => notice.push_str(&format!(
                "Your last message announced a next step without taking it: \"{quote}\"\n  → Take that step now with a tool call.\n"
            )),
            Some(Narration::Truncated) => notice.push_str(
                "Your last message was cut off at the output limit.\n  → Continue with a tool call.\n",
            ),
            Some(Narration::MalformedToolMarkup(marker)) => notice.push_str(&format!(
                "Your last message emitted raw tool-call markup ({marker}) in plain text instead of a structured tool call, so no tool was executed.\n  → Re-issue the intended tool call now using the tool-calling interface (do not write XML/DSML tags in text).\n"
            )),
            Some(Narration::SpuriousSummary) => notice.push_str(
                "Your last message emitted an internal <summary> block instead of executing the next step or delivering a user-facing completion report.\n  → Continue any remaining work with a tool call, or deliver your normal final answer.\n",
            ),
            Some(Narration::Empty) => notice.push_str(
                "Your last response was empty: no text and no tool call.\n  → Continue the remaining work with a tool call, or deliver your final answer.\n",
            ),
            None => {}
        }
        notice.push_str(
            "The process exits when a turn ends with none of the above. If the task is complete, say so in one final message and end your turn.",
        );
        notice
    }
}

/// The lead's direct subagents still on the team roster.
fn live_subagents(projection: &Projection) -> Vec<RosterEntry> {
    let team = team_projection(projection);
    let workflow = workflow_projection(projection);
    let members = members_projection(projection);
    team.roster
        .into_values()
        .filter(|entry| scope::parent_path(&entry.handle) == Some(scope::ROOT_HANDLE))
        .filter(|entry| !matches!(entry.status, RosterStatus::Done | RosterStatus::Failed))
        .filter(|entry| {
            // Finished Workflow actors stay parked for reuse; only busy ones
            // represent work the lead still needs to collect.
            entry.status != RosterStatus::Idle
                || !workflow
                    .as_ref()
                    .and_then(|workflow| workflow.run.as_ref())
                    .filter(|run| run.status.is_terminal())
                    .is_some_and(|run| {
                        run.stages
                            .iter()
                            .flat_map(|stage| &stage.members)
                            .any(|linked| {
                                members.iter().any(|member| {
                                    member.member == linked.member
                                        && member.child == Some(entry.session)
                                })
                            })
                    })
        })
        .collect()
}

/// Mail that the supervisor would otherwise deliver in its next root turn.
fn unread_root_mail(projection: &Projection) -> u64 {
    let team = team_projection(projection);
    let cursor = team
        .roster
        .get(scope::ROOT_HANDLE)
        .map_or(0, |entry| entry.resident_cursor);
    team.unread_incoming_mail(scope::ROOT_HANDLE, cursor)
        .count() as u64
}

fn narration(projection: &Projection, finish: FinishReason) -> Option<Narration> {
    if finish == FinishReason::Length {
        return Some(Narration::Truncated);
    }
    let text = final_step_text(projection)?;
    if text.trim().is_empty() {
        return Some(Narration::Empty);
    }
    if let Some(marker) = malformed_tool_markup(&text) {
        return Some(Narration::MalformedToolMarkup(marker));
    }
    if text.trim_end().ends_with("</summary>")
        && text.contains("<analysis>")
        && text.contains("<summary>")
    {
        return Some(Narration::SpuriousSummary);
    }
    announces_next_step(&text).then(|| {
        let paragraph = last_paragraph(text.trim_end());
        let mut quote: String = paragraph.chars().take(QUOTE_CHARS).collect();
        if paragraph.chars().count() > QUOTE_CHARS {
            quote.push('…');
        }
        Narration::Announces(quote)
    })
}

/// Only a tool-free last step is eligible for text continuation checks.
/// One assistant message spans all steps of a turn.
fn final_step_text(projection: &Projection) -> Option<String> {
    let message = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)?;
    let parts = &message.parts[message.last_step_start.unwrap_or(0)..];
    if parts
        .iter()
        .any(|part| matches!(part, PartProjection::Tool { .. }))
    {
        return None;
    }
    let text = parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(text)
}

/// Ignore code examples; a result suffix can belong to a long parameter
/// whose opening lies well before the tail window.
fn malformed_tool_markup(text: &str) -> Option<String> {
    let outside = outside_closed_code(text).to_lowercase();
    if outside.contains("｜dsml｜") {
        return Some("DSML".into());
    }
    let tail: String = outside
        .trim_end()
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    for marker in ["</invoke>", "</tool_call>", "</function_calls>"] {
        if tail.ends_with(marker) {
            return Some(marker.into());
        }
    }
    if let Some(prefix) = tail.strip_suffix("calls>") {
        // An angle bracket after a word is a generic type, e.g. Vec<Calls>.
        let prefix = prefix.strip_suffix('<').unwrap_or(prefix);
        if prefix
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
        {
            return Some("calls>".into());
        }
    }
    (outside.contains("<parameter ") && tail.ends_with("</result>"))
        .then(|| "<parameter …></result>".into())
}

/// Blank only closed Markdown fences and matched inline backtick spans.
/// Preserve byte offsets while blanking, including non-ASCII examples.
fn outside_closed_code(text: &str) -> String {
    let mut outside = text.to_string();
    let mut open = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let delimiter = trimmed.chars().next().unwrap_or(' ');
        if matches!(delimiter, '`' | '~') {
            let width = trimmed.chars().take_while(|c| *c == delimiter).count();
            if width >= 3 {
                match open {
                    None if delimiter != '`' || !trimmed[width..].contains('`') => {
                        open = Some((offset, delimiter, width));
                    }
                    Some((start, fence, opening_width))
                        if delimiter == fence
                            && width >= opening_width
                            && trimmed[width..].trim().is_empty() =>
                    {
                        let end = offset + line.len();
                        outside.replace_range(start..end, &" ".repeat(end - start));
                        open = None;
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    // An unclosed fence is unfinished output, not an inline code span.
    let inline_end = open.map_or(text.len(), |(start, _, _)| start);
    let mut at = 0;
    while at < inline_end {
        if outside.as_bytes()[at] != b'`' {
            at += 1;
            continue;
        }
        let width = outside.as_bytes()[at..inline_end]
            .iter()
            .take_while(|c| **c == b'`')
            .count();
        let mut end = at + width;
        let mut close = None;
        while end < inline_end {
            if outside.as_bytes()[end] == b'`' {
                let closing_width = outside.as_bytes()[end..inline_end]
                    .iter()
                    .take_while(|c| **c == b'`')
                    .count();
                if closing_width == width {
                    close = Some(end + width);
                    break;
                }
                end += closing_width;
            } else {
                end += 1;
            }
        }
        if let Some(end) = close {
            outside.replace_range(at..end, &" ".repeat(end - at));
            at = end;
        } else {
            at += width;
        }
    }
    outside
}

/// Clause openings that announce the speaker's own next action.
const LEADS: &[&str] = &[
    "now let me",
    "now i'll",
    "now i will",
    "now verify",
    "now check",
    "now run",
    "now write",
    "now test",
    "next, let me",
    "next let me",
    "next i'll",
    "let me ",
    "i'll now",
    "i will now",
    "i'll start by ",
    "i will start by ",
    "i'll begin by ",
    "i will begin by ",
    "i'll first ",
    "i will first ",
    "first, i'll ",
    "first i'll ",
    "first, i will ",
    "first i will ",
    "first, let me ",
    "first let me ",
    "next i will ",
    "next, i'll ",
    "next, i will ",
    "let's start by ",
    "let's begin by ",
    "先让我",
    "我先来",
    "我先看",
    "我先查",
    "我先检查",
    "我先读",
    "我先运行",
    "现在我来",
    "现在我",
    "现在来",
    "接下来我",
    "接下来",
    "下面我",
    "我来",
    "我现在",
    "然后我",
];
/// Openings that match a lead but hand the turn back to the user.
const NOT_LEADS: &[&str] = &["let me know", "如需", "如果需要"];

/// Whether a final text promises work it does not contain. Reads the raw
/// text: a reply ending in a fenced block after "…one per line:" is complete.
/// Tuned for precision on recorded runs (no hit on a passing trial).
fn announces_next_step(text: &str) -> bool {
    let text = text.trim_end();
    let paragraph = last_paragraph(text);
    if paragraph.ends_with(':') || paragraph.ends_with('：') {
        return true;
    }
    if paragraph.chars().count() > MAX_ANNOUNCEMENT_CHARS {
        return false;
    }
    let Some(sentence) = last_sentence(paragraph) else {
        return false;
    };
    clauses(sentence).any(|clause| {
        let clause = clause.trim_start().to_lowercase().replace('\u{2019}', "'");
        LEADS.iter().any(|lead| clause.starts_with(lead))
            && !NOT_LEADS.iter().any(|not| clause.starts_with(not))
    })
}

fn last_paragraph(text: &str) -> &str {
    text.rsplit("\n\n")
        .map(str::trim)
        .find(|paragraph| {
            !paragraph.is_empty()
                && !paragraph
                    .chars()
                    .all(|c| c.is_whitespace() || matches!(c, '-' | '_' | '*' | '─' | '━'))
        })
        .unwrap_or("")
}

/// The last non-empty sentence: ASCII `.!?` end one before whitespace,
/// CJK `。！？` end one anywhere.
fn last_sentence(paragraph: &str) -> Option<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut chars = paragraph.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let end = match c {
            '。' | '！' | '？' => true,
            '.' | '!' | '?' => chars.peek().is_none_or(|(_, next)| next.is_whitespace()),
            _ => false,
        };
        if end {
            let next = at + c.len_utf8();
            sentences.push(&paragraph[start..next]);
            start = next;
        }
    }
    sentences.push(&paragraph[start..]);
    sentences.into_iter().rev().map(str::trim).find(|sentence| {
        !sentence
            .trim_end_matches(['.', '!', '?', '。', '！', '？'])
            .is_empty()
    })
}

fn clauses(sentence: &str) -> impl Iterator<Item = &str> {
    sentence.split("**").flat_map(|part| {
        std::iter::once(part).chain(
            part.match_indices(['，', ',', '；', ';', '…', '—'])
                .map(|(at, delimiter)| &part[at + delimiter.len()..]),
        )
    })
}

/// Whether any message after the `since` notice calls a tool: the one signal
/// of acting on a notice shared by every pending reason.
fn called_any_tool(projection: &Projection, since: MessageId) -> bool {
    let messages = &projection.session.messages;
    messages
        .iter()
        .position(|message| message.id == since)
        .is_some_and(|at| {
            messages[at + 1..].iter().any(|message| {
                message
                    .parts
                    .iter()
                    .any(|part| matches!(part, PartProjection::Tool { .. }))
            })
        })
}

fn subagent_line(entry: &RosterEntry) -> String {
    let status = match entry.status {
        RosterStatus::Idle => "idle",
        RosterStatus::Busy => "working",
        RosterStatus::Done => "done",
        RosterStatus::Failed => "failed",
    };
    match &entry.current_task {
        Some(task) => format!("- {} ({status}: {task})", entry.handle),
        None => format!("- {} ({status})", entry.handle),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use hya_proto::{
        MessageProjection, PartId, TodoItem, TodoStatus, ToolCallId, ToolName, ToolPartState,
    };

    fn text(text: &str) -> PartProjection {
        PartProjection::Text {
            id: PartId::new(),
            text: text.to_string(),
        }
    }

    fn tool(name: &str) -> PartProjection {
        PartProjection::Tool {
            id: PartId::new(),
            call: ToolCallId::new(),
            name: ToolName::new(name),
            state: ToolPartState::Running {
                input: serde_json::json!({}),
            },
            eviction_notice: None,
            provider_data: None,
        }
    }

    fn message(role: Role, parts: Vec<PartProjection>) -> MessageProjection {
        serde_json::from_value::<MessageProjection>(serde_json::json!({
            "id": MessageId::new(),
            "role": role,
            "finish": null,
            "parts": [],
        }))
        .map(|mut message| {
            message.parts = parts;
            message
        })
        .expect("message fixture")
    }

    fn with_messages(messages: Vec<MessageProjection>) -> Projection {
        let mut projection = Projection::default();
        projection.session.messages = messages;
        projection
    }

    fn todo(id: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: format!("item {id}"),
            status,
        }
    }

    fn roster(handle: &str, status: RosterStatus) -> RosterEntry {
        serde_json::from_value(serde_json::json!({
            "handle": handle, "session": SessionId::new(), "status": status,
        }))
        .expect("roster fixture")
    }

    #[test]
    fn narration_announcements_steps_truncation_empty_and_spurious_summary() {
        for announcing in [
            "I'll start by examining the config file.",
            "I’ll start by reading config.yaml.",
            "I will begin by running tests.",
            "First, I'll check the logs.",
            "我先查看配置文件。",
            "Line-length issues are pre-existing. Now let me look at the docs:",
            "下一步：",
            "LMDB 已构建，数据正确。现在我来检查 solver 生命周期。",
            "Found it: N=208 worked by coincidence. Let me fix.",
            "All edits are in, **now let me** run the checks.",
            "Now let me check:\n\n---\n\n______",
        ] {
            assert!(announces_next_step(announcing), "{announcing}");
        }
        let long = format!("{} Let me run the tests.", "word ".repeat(100));
        for complete in [
            "我先后运行了单元测试和集成测试，全部通过。",
            "I started by examining the config file and wrote the output.",
            "First, I checked the logs; all tests pass.",
            "Let me know if you need anything else.",
            "如需调整请告诉我。",
            "Changed files:\n- a.rs\n- b.rs",
            "Done.\n\n---",
            "",
            "The best moves are in /app/move.txt:\n\n```\ne2e4\n```",
            long.as_str(),
        ] {
            assert!(!announces_next_step(complete), "{complete:?}");
        }

        // Only a tool-free final step counts; reasoning-only final step does not reuse prior text.
        let mut proj = with_messages(vec![
            message(Role::User, vec![text("do it")]),
            message(
                Role::Assistant,
                vec![text("Now let me check:"), tool("bash")],
            ),
        ]);
        let id = proj.session.messages[1].id;
        apply_step(&mut proj, id, 1);
        proj.session.messages[1]
            .parts
            .extend([text("Done."), text("Changed files: a.txt.")]);
        assert_eq!(
            final_step_text(&proj).as_deref(),
            Some("Done.\nChanged files: a.txt.")
        );
        assert_eq!(narration(&proj, FinishReason::Stop), None);

        let mut reasoning_only =
            with_messages(vec![message(Role::Assistant, vec![text("Now verify:")])]);
        let rid = reasoning_only.session.messages[0].id;
        apply_step(&mut reasoning_only, rid, 1);
        reasoning_only.session.messages[0]
            .parts
            .push(PartProjection::Reasoning {
                id: PartId::new(),
                text: "thinking".into(),
                reason: None,
                provider_data: None,
            });
        assert_eq!(
            narration(&reasoning_only, FinishReason::Stop),
            Some(Narration::Empty)
        );
        assert_eq!(
            narration(
                &with_messages(vec![message(
                    Role::Assistant,
                    vec![tool("bash"), text("</invoke>")]
                )]),
                FinishReason::Stop
            ),
            None
        );

        // Truncated, empty, spurious summary, and progress detection.
        assert_eq!(
            narration(
                &with_messages(vec![message(Role::Assistant, vec![text("All good.")])]),
                FinishReason::Length
            ),
            Some(Narration::Truncated)
        );
        for parts in [vec![], vec![text("")], vec![text(" \n")]] {
            assert_eq!(
                narration(
                    &with_messages(vec![message(Role::Assistant, parts)]),
                    FinishReason::Stop
                ),
                Some(Narration::Empty)
            );
        }
        assert_eq!(
            narration(
                &with_messages(vec![message(
                    Role::Assistant,
                    vec![text(
                        "<analysis>Remaining work</analysis>\n<summary>Continue.</summary>  "
                    )],
                )]),
                FinishReason::Stop,
            ),
            Some(Narration::SpuriousSummary)
        );
        assert_eq!(
            narration(
                &with_messages(vec![message(
                    Role::Assistant,
                    vec![text("<summary>Result.</summary>")]
                )]),
                FinishReason::Stop
            ),
            None
        );

        let notice = message(Role::System, vec![text("HEADLESS RUN")]);
        let nid = notice.id;
        assert!(called_any_tool(
            &with_messages(vec![
                notice.clone(),
                message(Role::Assistant, vec![tool("bash"), text("ok")])
            ]),
            nid
        ));
        assert!(!called_any_tool(
            &with_messages(vec![
                message(Role::Assistant, vec![tool("bash")]),
                notice,
                message(Role::Assistant, vec![text("ok")])
            ]),
            nid
        ));
    }

    #[test]
    fn dsml_and_xml_tool_markup_leak_matrix() {
        let long_xml = format!(
            "<result><name>write</name><parameter name=\"content\">{}</parameter></result>",
            "多行内容\n".repeat(200)
        );
        for leak in [
            "｜DSML｜ calls>",
            "</｜DSML｜ invoke>",
            "<｜dsml｜ invoke>",
            "<result><name>bash</name><parameter name=\"command\">pwd</parameter></result>",
            "<invoke>pwd</invoke>",
            "<tool_call>pwd</tool_call>",
            "<function_calls>pwd</function_calls>",
            "calls>",
            "```xml\n｜DSML｜ calls>",
            "~~~xml\n</invoke>",
            "`<parameter name=\"command\">pwd</result>",
            "<tool_calls>",
            "</tool_calls>",
            " calls>",
            "<calls>",
            "```safe```\n</tool_call>",
            long_xml.as_str(),
        ] {
            assert!(
                matches!(
                    narration(
                        &with_messages(vec![message(Role::Assistant, vec![text(leak)])]),
                        FinishReason::Stop
                    ),
                    Some(Narration::MalformedToolMarkup(_))
                ),
                "{leak}"
            );
        }
        for safe in [
            "Example:\n```xml\n｜DSML｜ calls>\n</｜DSML｜ invoke>\n```\nDone.",
            "Example:\n~~~xml\n｜DSML｜ calls>\n~~~\nDone.",
            "Example:\n~~~xml\n<parameter name=\"x\">value</result>\n~~~\nDone.",
            "Used `｜DSML｜ calls>` and ``</｜DSML｜ invoke>`` as examples.",
            "The tags are `<parameter name=\"x\">` and `</result>`.",
            "Example `<parameter name=\"x\">value` ends with </result>",
            "Vec<Calls> syscalls> callbacks> Unicode字calls>",
            "<parameter name=\"x\">value</result>\n\nDone. The format is documented.",
            "```foo```\n\n```\n<parameter name=\"x\">example</result>\n```\n\nDone.",
            "Ran it via a tool call instead of DSML markup.",
        ] {
            assert_eq!(malformed_tool_markup(safe), None, "{safe}");
        }
        assert_eq!(
            narration(
                &with_messages(vec![message(
                    Role::Assistant,
                    vec![text("｜DSML｜ calls>"), tool("bash")]
                )]),
                FinishReason::Stop
            ),
            None
        );
        let notice = Pending::of(
            &with_messages(vec![message(
                Role::Assistant,
                vec![text("｜DSML｜ calls>")],
            )]),
            FinishReason::Stop,
        )
        .notice(None, false);
        assert!(notice.contains("(DSML)") && !notice.contains("｜DSML｜"));
    }

    #[test]
    fn pending_holds_for_unread_mail_live_roster_and_active_workflow_only() {
        // 1. Unread root mail holds exec open until cursor advances.
        let mut ma = hya_tool::extension::MultiAgentExtensionState::default();
        let mut ch = hya_tool::extension::ChannelsExtensionState::default();
        let root = roster(scope::ROOT_HANDLE, RosterStatus::Idle);
        ma.roster.insert(root.handle.clone(), root);
        ch.mail.push(hya_tool::extension::MailMessage {
            from: "main/worker".into(),
            to: hya_proto::MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
            kind: hya_proto::MailKind::Message,
            body: "result".into(),
        });
        ch.inboxes.insert(scope::ROOT_HANDLE.into(), vec![0]);
        let mut proj = Projection::default();
        proj.extensions.insert(
            hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            serde_json::to_value(&ma).unwrap(),
        );
        proj.extensions.insert(
            hya_tool::extension::BUNDLE_CHANNELS.into(),
            serde_json::to_value(&ch).unwrap(),
        );
        let pending = Pending::of(&proj, FinishReason::Stop);
        assert!(!pending.is_empty());
        assert!(pending.notice(None, false).contains("1 unread message"));
        ma.roster
            .get_mut(scope::ROOT_HANDLE)
            .unwrap()
            .resident_cursor = 1;
        proj.extensions.insert(
            hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            serde_json::to_value(&ma).unwrap(),
        );
        assert!(Pending::of(&proj, FinishReason::Stop).is_empty());

        // 2. Late-stage archive guidance & terminal roster entries & workflow actor parking.
        let mut p2 = Projection::default();
        let mut ma2 = hya_tool::extension::MultiAgentExtensionState::default();
        let worker = roster("main/worker", RosterStatus::Idle);
        ma2.roster.insert(worker.handle.clone(), worker);
        p2.extensions.insert(
            hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            serde_json::to_value(&ma2).unwrap(),
        );
        let late = Pending::of(&p2, FinishReason::Stop).notice(None, true);
        assert!(late.contains("`archive` the subagents whose work you cannot integrate"));
        assert!(!late.contains("HEADLESS RUN — FINAL"));
        for status in [RosterStatus::Done, RosterStatus::Failed] {
            let entry = roster("main/worker", status);
            ma2.roster.insert(entry.handle.clone(), entry);
            p2.extensions.insert(
                hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
                serde_json::to_value(&ma2).unwrap(),
            );
            assert!(live_subagents(&p2).is_empty(), "{status:?}");
        }

        let mut p3 = Projection::default();
        let mut ma3 = hya_tool::extension::MultiAgentExtensionState::default();
        let stage_worker = roster("main/stage", RosterStatus::Idle);
        let member = hya_proto::MemberId::new();
        ma3.members.push(
            serde_json::from_value(serde_json::json!({
                "member": member, "child": stage_worker.session, "subagent_type": "worker",
                "description": "stage", "depth": 1, "status": "done"
            }))
            .unwrap(),
        );
        ma3.roster.insert(stage_worker.handle.clone(), stage_worker);
        p3.extensions.insert(
            hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            serde_json::to_value(&ma3).unwrap(),
        );
        let run = serde_json::json!({
            "id": hya_proto::WorkflowRunId::new(),
            "workflow": {"source": "test", "name": "test", "revision": "00".repeat(32)},
            "request_hash": "test", "owner": hya_proto::OwnerRunId::new(),
            "status": "completed", "stages": [{
                "id": "stage", "agent": "worker", "mode": "once", "level": 0,
                "status": "completed", "members": [{"member": member, "role": "worker", "iteration": 0}]
            }]
        });
        let mut wf3: hya_tool::WorkflowProjection =
            serde_json::from_value(serde_json::json!({"run": run})).unwrap();
        p3.extensions.insert(
            hya_tool::extension::BUNDLE_WORKFLOW.into(),
            serde_json::to_value(&wf3).unwrap(),
        );
        assert!(live_subagents(&p3).is_empty());
        wf3.run.as_mut().unwrap().status = hya_tool::WorkflowRunStatus::Running;
        p3.extensions.insert(
            hya_tool::extension::BUNDLE_WORKFLOW.into(),
            serde_json::to_value(&wf3).unwrap(),
        );
        assert_eq!(live_subagents(&p3).len(), 1);

        // Open todos do not hold runs open.
        let mut p4 = with_messages(vec![message(
            Role::Assistant,
            vec![text("Now let me run it:")],
        )]);
        p4.session.todos = Some(vec![todo("3", TodoStatus::Pending)]);
        let notice = Pending::of(&p4, FinishReason::Stop).notice(None, false);
        assert!(
            notice.starts_with("HEADLESS RUN")
                && notice.contains("\"Now let me run it:\"")
                && !notice.contains("todo")
        );
    }

    /// U10: replay over the final texts of passing recorded trials (`testdata/final_texts.jsonl`).
    #[test]
    fn u10_recorded_passing_trials_hit_only_real_announcements() {
        const REVIEWED: &[&str] = &[
            "hf-model-inference__8VgFzpc",
            "filter-js-from-html__DCN2Crn",
            "circuit-fibsqrt__NWKfq2H",
            "fix-git__FQXnjv8",
            "kv-store-grpc__eNbzmBn",
            "kv-store-grpc__46b88ds",
            "merge-diff-arc-agi-task__KLnZG3a",
            "model-extraction-relu-logits__WoxXbPL",
        ];
        let corpus = include_str!("../testdata/final_texts.jsonl");
        let mut passing = 0;
        let mut unexpected = Vec::new();
        let mut hit = Vec::new();
        for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            passing += 1;
            let trial = row["trial"].as_str().unwrap();
            let text = row["text"].as_str().unwrap();
            let projection = with_messages(vec![message(Role::Assistant, vec![self::text(text)])]);
            assert!(
                !matches!(
                    narration(&projection, FinishReason::Stop),
                    Some(Narration::MalformedToolMarkup(_) | Narration::SpuriousSummary)
                ),
                "{trial}"
            );
            if announces_next_step(text) {
                let leaf = trial.rsplit('/').next().unwrap_or(trial);
                if REVIEWED.contains(&leaf) {
                    hit.push(leaf.to_string());
                } else {
                    unexpected.push(format!("{trial}: {text}"));
                }
            }
        }
        assert_eq!(passing, 135);
        assert!(unexpected.is_empty(), "{}", unexpected.join("\n---\n"));
        assert_eq!(hit.len(), REVIEWED.len());
    }

    #[tokio::test]
    async fn outgoing_directive_does_not_extend_headless_run_after_archive() {
        use hya_provider::{DevProvider, Provider, ProviderRouter};
        use std::sync::{Arc, OnceLock, Weak};
        struct SpawnAndArchive {
            context: OnceLock<(
                Weak<SessionEngine>,
                Weak<ResidentSupervisor>,
                SessionId,
                AgentSpec,
            )>,
            spawned: std::sync::atomic::AtomicBool,
        }
        #[async_trait::async_trait]
        impl Provider for SpawnAndArchive {
            fn id(&self) -> &str {
                "hya"
            }
            fn capabilities(
                &self,
                model: &hya_proto::ModelRef,
            ) -> Option<hya_provider::Capabilities> {
                DevProvider::new().capabilities(model)
            }
            async fn stream(
                &self,
                req: hya_provider::CompletionRequest,
                session: SessionId,
                message: MessageId,
            ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
                let (engine, supervisor, root, agent) = self.context.get().unwrap();
                if session == *root && !self.spawned.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let engine = engine.upgrade().unwrap();
                    let supervisor = supervisor.upgrade().unwrap();
                    let binding = engine.bind_runtime(&agent.workdir).unwrap();
                    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
                    let (_, handle) = supervisor
                        .spawn_resident(
                            *root,
                            agent.clone(),
                            (binding, Arc::from([]), resources, None),
                            "task directive".into(),
                            None,
                            None,
                        )
                        .await
                        .unwrap();
                    supervisor
                        .archive_member(*root, &handle, "no longer needed")
                        .await
                        .unwrap();
                }
                DevProvider::new().stream(req, session, message).await
            }
        }
        let provider = Arc::new(SpawnAndArchive {
            context: OnceLock::new(),
            spawned: false.into(),
        });
        let catalog = Arc::new(
            hya_engine::AgentCatalog::new(Arc::new(
                hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
            ))
            .unwrap(),
        );
        let runtime = Arc::new(hya_engine::RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            catalog,
        ));
        let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            hya_store::SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime,
            permission,
            hya_engine::EventBus::default(),
        ));
        let agent = AgentSpec {
            name: hya_proto::AgentName::new("hya-task"),
            model: hya_proto::ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let root = engine
            .create(hya_engine::CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let supervisor = ResidentSupervisor::start(engine.clone());
        assert!(
            provider
                .context
                .set((
                    Arc::downgrade(&engine),
                    Arc::downgrade(&supervisor),
                    root,
                    agent.clone()
                ))
                .is_ok()
        );
        let prompt = engine
            .admit_user_prompt(root, "finish the task".into())
            .await
            .unwrap();
        run_lead(
            &engine,
            &supervisor,
            root,
            &agent,
            CancellationToken::new(),
            prompt,
        )
        .await
        .unwrap();
        let projection = engine.read_projection(root).await.unwrap();
        assert!(!team_projection(&projection).archived.is_empty());
        assert_eq!(
            projection
                .session
                .messages
                .iter()
                .filter(|m| m.role == Role::Assistant)
                .count(),
            1
        );
        supervisor
            .drain(
                hya_proto::FinishCause::Shutdown,
                std::time::Duration::from_secs(2),
            )
            .await;
    }

    fn apply_step(projection: &mut Projection, message: MessageId, step: u32) {
        projection.apply(&hya_proto::Envelope {
            seq: hya_proto::EventSeq(0),
            ts_millis: 0,
            event: hya_proto::Event::StepStarted {
                session: SessionId::new(),
                message,
                step,
                view: None,
            },
        });
    }
}
