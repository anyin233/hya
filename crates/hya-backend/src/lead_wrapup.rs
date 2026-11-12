//! The lead of a headless root run (`exec`/`run`) must not end the run while
//! it still has work in flight. Nobody is there to answer a turn that stops
//! early: when the lead's turn ends on its own with live subagents (stopped
//! and lost when the run ends), unfinished todos, or a last message that
//! announces a next step it did not take, the run gives it another turn with
//! a harness notice naming what is left, until a turn ends with none of them.
//! In a budgeted run the deadline bounds this: past the grace no turn starts,
//! and in the final window one ignored notice ends the run.

use hya_core::{AgentSpec, CoreError, SessionEngine};
use hya_proto::{
    FinishReason, MessageId, PartProjection, Projection, Role, RosterEntry, RosterStatus,
    SessionId, TodoItem, TodoStatus, scope,
};
use hya_tool::deadline::{RunDeadline, Stage};
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

/// Run the lead's turn, then one more turn per notice while it ends with
/// [`Pending`] work (bounded by [`MAX_IGNORED_NOTICES`], and by `deadline`
/// or else [`MAX_NOTICES_WITHOUT_BUDGET`]). Returns the last turn's finish;
/// a cancelled or failed turn ends the run at once.
pub(crate) async fn run_lead(
    engine: &SessionEngine,
    session: SessionId,
    agent: &AgentSpec,
    cancel: CancellationToken,
    deadline: Option<&RunDeadline>,
) -> Result<FinishReason, CoreError> {
    let (mut ignored, mut notices) = (0u32, 0u32);
    let mut last_notice: Option<MessageId> = None;
    loop {
        let finish = engine.run_turn(session, agent, cancel.clone()).await?;
        if !matches!(finish, FinishReason::Stop | FinishReason::Length) {
            return Ok(finish);
        }
        let stage = deadline.map_or(Stage::Normal, RunDeadline::stage);
        if stage == Stage::Over {
            return Ok(finish);
        }
        let projection = engine.read_projection(session).await?;
        let pending = Pending::of(&projection, finish);
        if pending.is_empty() {
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
        // A late stage leads the notice, so no in-turn notice repeats it.
        let time = deadline.filter(|_| stage >= Stage::WrapUp).map(|deadline| {
            deadline.mark_notified(session, stage);
            deadline.notice(stage, false)
        });
        let notice = pending.notice(time.as_deref());
        last_notice = Some(engine.inject_system_message(session, notice).await?);
    }
}

/// Why a lead turn that ended on its own must not end the run yet.
struct Pending<'a> {
    subagents: Vec<&'a RosterEntry>,
    todos: Vec<&'a TodoItem>,
    narration: Option<Narration>,
}

/// The last step answered in text that leaves its work unfinished.
#[derive(Debug, PartialEq, Eq)]
enum Narration {
    /// The output hit the length limit.
    Truncated,
    /// The final paragraph announces a step it did not take (quoted).
    Announces(String),
}

impl<'a> Pending<'a> {
    fn of(projection: &'a Projection, finish: FinishReason) -> Self {
        Self {
            subagents: live_subagents(projection),
            todos: unfinished_todos(projection),
            narration: narration(projection, finish),
        }
    }

    fn is_empty(&self) -> bool {
        self.subagents.is_empty() && self.todos.is_empty() && self.narration.is_none()
    }

    /// One notice listing every reason, most costly to leave first, after
    /// the deadline notice `time` of a late stage.
    fn notice(&self, time: Option<&str>) -> String {
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
            notice.push_str(if time.is_some() {
                "  → `archive` the subagents whose work you cannot integrate in the time left; `wait` briefly for the rest.\n"
            } else {
                "  → call `wait` to collect their reports and integrate the results, or `archive` the ones you no longer need.\n"
            });
        }
        if !self.todos.is_empty() {
            notice.push_str("Your todo list has unfinished items:\n");
            for todo in &self.todos {
                notice.push_str(&format!(
                    "- #{} [{}] {}\n",
                    todo.id,
                    todo.status.as_str(),
                    todo.content
                ));
            }
            notice.push_str(
                "  → Continue with them. Mark an item `completed` when it is done, or `blocked` if it cannot be done in this run.\n",
            );
        }
        match &self.narration {
            Some(Narration::Announces(quote)) => notice.push_str(&format!(
                "Your last message announced a next step without taking it: \"{quote}\"\n  → Take that step now with a tool call.\n"
            )),
            Some(Narration::Truncated) => notice.push_str(
                "Your last message was cut off at the output limit.\n  → Continue with a tool call.\n",
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
fn live_subagents(projection: &Projection) -> Vec<&RosterEntry> {
    projection
        .team
        .roster
        .values()
        .filter(|entry| scope::parent_path(&entry.handle) == Some(scope::ROOT_HANDLE))
        .collect()
}

/// The lead's own todos still to do. `Blocked` is the way out for an item
/// that cannot be done: in a headless run nothing outside will unblock it.
fn unfinished_todos(projection: &Projection) -> Vec<&TodoItem> {
    projection
        .session
        .todos
        .iter()
        .flatten()
        .filter(|todo| matches!(todo.status, TodoStatus::Pending | TodoStatus::InProgress))
        .collect()
}

fn narration(projection: &Projection, finish: FinishReason) -> Option<Narration> {
    if finish == FinishReason::Length {
        return Some(Narration::Truncated);
    }
    let text = final_step_text(projection)?;
    announces_next_step(&text).then(|| {
        let paragraph = last_paragraph(text.trim_end());
        let mut quote: String = paragraph.chars().take(QUOTE_CHARS).collect();
        if paragraph.chars().count() > QUOTE_CHARS {
            quote.push('…');
        }
        Narration::Announces(quote)
    })
}

/// Text the turn's last step produced: the Text parts after the last Tool
/// part of the session's last assistant message (one message spans all the
/// turn's steps).
fn final_step_text(projection: &Projection) -> Option<String> {
    let message = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)?;
    let from = message
        .parts
        .iter()
        .rposition(|part| matches!(part, PartProjection::Tool { .. }))
        .map_or(0, |at| at + 1);
    let text = message.parts[from..]
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(text)
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
    if text.ends_with(':') || text.ends_with('：') {
        return true;
    }
    let paragraph = last_paragraph(text);
    if paragraph.chars().count() > MAX_ANNOUNCEMENT_CHARS {
        return false;
    }
    let Some(sentence) = last_sentence(paragraph) else {
        return false;
    };
    clauses(sentence).any(|clause| {
        let clause = clause.trim_start().to_lowercase();
        LEADS.iter().any(|lead| clause.starts_with(lead))
            && !NOT_LEADS.iter().any(|not| clause.starts_with(not))
    })
}

fn last_paragraph(text: &str) -> &str {
    text.rsplit("\n\n").next().unwrap_or(text).trim()
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
    sentence
        .split("**")
        .flat_map(|part| part.split(['，', ',', '；', ';', '…', '—']))
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
    use hya_proto::{MessageProjection, PartId, ToolCallId, ToolName, ToolPartState};

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

    #[test]
    fn u1_pending_and_in_progress_todos_are_unfinished() {
        let mut projection = Projection::default();
        projection.session.todos = Some(vec![
            todo("1", TodoStatus::Pending),
            todo("2", TodoStatus::Pending),
            todo("3", TodoStatus::InProgress),
            todo("4", TodoStatus::Completed),
        ]);
        let ids: Vec<_> = unfinished_todos(&projection)
            .iter()
            .map(|todo| todo.id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    #[test]
    fn u2_completed_and_blocked_todos_are_finished() {
        let mut projection = Projection::default();
        projection.session.todos = Some(vec![
            todo("1", TodoStatus::Completed),
            todo("2", TodoStatus::Blocked),
        ]);
        assert!(unfinished_todos(&projection).is_empty());
        assert!(unfinished_todos(&Projection::default()).is_empty());
    }

    #[test]
    fn u3_a_trailing_colon_announces() {
        for text in [
            // python-statemachine (hya-v11)
            "Line-length issues are pre-existing. Now let me look at the docs structure to add a page:",
            // circuit-fibsqrt hya-fh11-a1
            "Max fan-out is 102 (cap 10000) ✓. Now verify the file format, output-signal placement, and convergence timing:",
            // cargo-flight-dispatch local tb4
            "Reviewer verdict: **correct**, no P0/P1/P2. Let me read the full report and address its single P3:",
            // mvcc-lsm-compaction local tb4 (unfinished todos too)
            "The fix works. Now let me validate it thoroughly with a randomized differential check against a no-compaction oracle (in /tmp, exploratory) to make sure the compaction rules never lose anything a reader can observe:",
            "下一步：",
        ] {
            assert!(announces_next_step(text), "{text}");
        }
    }

    #[test]
    fn u4_a_short_last_paragraph_leading_with_an_action_announces() {
        for text in [
            // caffe-cifar-10 hya-fh11
            "LMDB 已构建，计算出的均值（125.31、122.95、113.87）与官方 CIFAR-10 均值完全一致——数据正确。现在我来检查 solver 生命周期，以便准确制定配置。",
            // circuit-fibsqrt hya-fh11
            "Found it: the two bits of each input pair are swapped (`rem_new[0]` should be `n[30]`, `rem_new[1]` should be `n[31]`). N=208 worked by coincidence. Let me fix.",
            // cargo-flight-dispatch hya-fh11-a3
            "Now let me write an independent verifier that recomputes everything from the data with different formula formulations (vector-based ground speed, law-of-cosines distances) and checks the output.",
            "All edits are in, **now let me** run the checks.",
        ] {
            assert!(announces_next_step(text), "{text}");
        }
    }

    #[test]
    fn u5_a_reply_ending_in_a_fenced_block_is_complete() {
        for text in [
            // chess-best-move hya-flash-02 (reward 1)
            "The best moves are written to /app/move.txt, one per line:\n\n```\ne2e4\ng1f3\n```",
            // feal-linear-cryptanalysis hya-fh11-a1 (reward 1)
            "Recovered all four round keys; the plaintexts are in /app/plaintexts.txt:\n\n```\n0x1234\n```\n",
        ] {
            assert!(!announces_next_step(text), "{text}");
        }
    }

    #[test]
    fn u6_summaries_and_handbacks_do_not_announce() {
        for text in [
            "Changed files:\n- a.rs\n- b.rs",
            "All tests pass.",
            "Let me know if you need anything else.",
            "如需调整请告诉我。",
            "Done. The v1.2 build is installed and the service is running.",
            "",
        ] {
            assert!(!announces_next_step(text), "{text:?}");
        }
    }

    #[test]
    fn u6_a_long_last_paragraph_does_not_announce() {
        let long = format!("{} Let me run the tests.", "word ".repeat(100));
        assert!(!announces_next_step(&long));
    }

    #[test]
    fn u7_only_text_after_the_last_tool_call_counts() {
        let projection = with_messages(vec![
            message(Role::User, vec![text("do it")]),
            message(
                Role::Assistant,
                vec![
                    text("Now let me check the file:"),
                    tool("bash"),
                    text("Done."),
                    text("Changed files: a.txt."),
                ],
            ),
        ]);
        assert_eq!(
            final_step_text(&projection).as_deref(),
            Some("Done.\nChanged files: a.txt.")
        );
        assert_eq!(narration(&projection, FinishReason::Stop), None);
        let announced = with_messages(vec![message(
            Role::Assistant,
            vec![tool("bash"), text("Now let me verify the output:")],
        )]);
        assert_eq!(
            narration(&announced, FinishReason::Stop),
            Some(Narration::Announces(
                "Now let me verify the output:".to_string()
            ))
        );
    }

    #[test]
    fn u8_a_length_finish_is_truncated() {
        let projection = with_messages(vec![message(Role::Assistant, vec![text("All good.")])]);
        assert_eq!(
            narration(&projection, FinishReason::Length),
            Some(Narration::Truncated)
        );
    }

    #[test]
    fn u9_progress_is_any_tool_call_after_the_notice() {
        let notice = message(Role::System, vec![text("HEADLESS RUN")]);
        let id = notice.id;
        let acted = with_messages(vec![
            message(Role::Assistant, vec![text("Let me check:")]),
            notice.clone(),
            message(Role::Assistant, vec![tool("bash"), text("ok")]),
        ]);
        assert!(called_any_tool(&acted, id));
        let ignored = with_messages(vec![
            message(Role::Assistant, vec![tool("bash")]),
            notice,
            message(Role::Assistant, vec![text("ok")]),
        ]);
        assert!(!called_any_tool(&ignored, id));
    }

    #[test]
    fn notice_lists_every_reason_in_order() {
        let mut projection = with_messages(vec![message(
            Role::Assistant,
            vec![text("Now let me run it:")],
        )]);
        projection.session.todos = Some(vec![
            todo("3", TodoStatus::Pending),
            todo("4", TodoStatus::Completed),
        ]);
        let notice = Pending::of(&projection, FinishReason::Stop).notice(None);
        assert!(notice.starts_with("HEADLESS RUN"));
        let todos = notice.find("#3 [pending] item 3").expect("todo listed");
        let narration = notice.find("\"Now let me run it:\"").expect("quoted");
        assert!(todos < narration);
        assert!(!notice.contains("#4"));
    }

    /// U10: replay over the final texts of passing recorded trials
    /// (`testdata/final_texts.jsonl`). The rule hits only these, each read
    /// by hand: a real announcement the run happened to survive.
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
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            passing += 1;
            let trial = row["trial"].as_str().expect("trial");
            let text = row["text"].as_str().expect("text");
            if announces_next_step(text) {
                let leaf = trial.rsplit('/').next().unwrap_or(trial);
                if REVIEWED.contains(&leaf) {
                    hit.push(leaf.to_string());
                } else {
                    unexpected.push(format!("{trial}: {text}"));
                }
            }
        }
        assert!(passing > 100, "corpus has {passing} passing trials");
        assert!(unexpected.is_empty(), "{}", unexpected.join("\n---\n"));
        assert_eq!(hit.len(), REVIEWED.len());
    }
}
