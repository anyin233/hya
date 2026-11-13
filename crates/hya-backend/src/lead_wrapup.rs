//! The lead of a headless root run (`exec`) must not end the run while
//! it still has work in flight. Nobody is there to answer a turn that stops
//! early: when the lead's turn ends on its own with live subagents (stopped
//! and lost when the run ends), unfinished todos, or a last message that
//! announces a next step it did not take, the run gives it another turn with
//! a harness notice naming what is left, until a turn ends with none of them.
//! In a budgeted run the deadline bounds this: past the grace no turn starts,
//! and in the final window one ignored notice ends the run.

use hya_core::{AgentSpec, CoreError, ResidentSupervisor, SessionEngine};
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
    let deadline = hya_tool::deadline::current();
    loop {
        let finish = engine
            .run_turn_with_external_dirs_and_guidance(
                session,
                agent,
                cancel.clone(),
                &[],
                None,
                None,
                initial_prompt.take(),
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
        last_notice = Some(engine.inject_system_message(session, notice).await?);
    }
}

/// Why a lead turn that ended on its own must not end the run yet.
struct Pending<'a> {
    subagents: Vec<&'a RosterEntry>,
    todos: Vec<&'a TodoItem>,
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
}

impl<'a> Pending<'a> {
    fn of(projection: &'a Projection, finish: FinishReason) -> Self {
        Self {
            subagents: live_subagents(projection),
            todos: unfinished_todos(projection),
            unread_mail: unread_root_mail(projection),
            narration: narration(projection, finish),
        }
    }

    fn is_empty(&self) -> bool {
        self.subagents.is_empty()
            && self.todos.is_empty()
            && self.unread_mail == 0
            && self.narration.is_none()
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
            Some(Narration::MalformedToolMarkup(marker)) => notice.push_str(&format!(
                "Your last message emitted raw tool-call markup ({marker}) in plain text instead of a structured tool call, so no tool was executed.\n  → Re-issue the intended tool call now using the tool-calling interface (do not write XML/DSML tags in text).\n"
            )),
            Some(Narration::SpuriousSummary) => notice.push_str(
                "Your last message emitted an internal <summary> block instead of executing the next step or delivering a user-facing completion report.\n  → Continue any remaining work with a tool call, or deliver your normal final answer.\n",
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
        .filter(|entry| !matches!(entry.status, RosterStatus::Done | RosterStatus::Failed))
        .filter(|entry| {
            // Finished Workflow actors stay parked for reuse; only busy ones
            // represent work the lead still needs to collect.
            entry.status != RosterStatus::Idle
                || !projection
                    .session
                    .workflow
                    .as_ref()
                    .and_then(|workflow| workflow.run.as_ref())
                    .filter(|run| run.status.is_terminal())
                    .is_some_and(|run| {
                        run.stages
                            .iter()
                            .flat_map(|stage| &stage.members)
                            .any(|linked| {
                                projection.session.members.iter().any(|member| {
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
    let cursor = projection
        .team
        .roster
        .get(scope::ROOT_HANDLE)
        .map_or(0, |entry| entry.resident_cursor);
    projection
        .team
        .unread_incoming_mail(scope::ROOT_HANDLE, cursor)
        .count() as u64
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
        let clause = clause.trim_start().to_lowercase();
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

    fn roster(handle: &str, status: RosterStatus) -> RosterEntry {
        serde_json::from_value(serde_json::json!({
            "handle": handle, "session": SessionId::new(), "status": status,
        }))
        .expect("roster fixture")
    }

    #[test]
    fn l1_unread_root_mail_holds_exec_open_until_consumed() {
        let mut projection = Projection::default();
        let root = roster(scope::ROOT_HANDLE, RosterStatus::Idle);
        projection.team.roster.insert(root.handle.clone(), root);
        projection.team.mail.push(hya_proto::MailMessage {
            from: "main/worker".into(),
            to: hya_proto::MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
            kind: hya_proto::MailKind::Message,
            body: "result".into(),
        });
        projection
            .team
            .inboxes
            .insert(scope::ROOT_HANDLE.into(), vec![0]);
        let pending = Pending::of(&projection, FinishReason::Stop);
        assert!(!pending.is_empty());
        assert!(pending.notice(None, false).contains("1 unread message"));
        projection
            .team
            .roster
            .get_mut(scope::ROOT_HANDLE)
            .unwrap()
            .resident_cursor = 1;
        assert!(Pending::of(&projection, FinishReason::Stop).is_empty());
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
            hya_core::AgentCatalog::new(Arc::new(
                hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
            ))
            .unwrap(),
        );
        let runtime = Arc::new(hya_core::RuntimeRegistry::new(
            hya_tool::ToolRegistry::builtins(),
            catalog,
        ));
        let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            hya_store::SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime,
            permission,
            hya_core::EventBus::default(),
        ));
        let agent = AgentSpec {
            name: hya_proto::AgentName::new("hya-task"),
            model: hya_proto::ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let root = engine
            .create(hya_core::CreateSession {
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
        assert!(!projection.team.archived.is_empty());
        assert_eq!(
            projection
                .session
                .messages
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .count(),
            1,
            "own directive must not cause headless continuation notices"
        );
        supervisor
            .drain(
                hya_proto::FinishCause::Shutdown,
                std::time::Duration::from_secs(2),
            )
            .await;
    }

    #[test]
    fn l6_late_stage_archive_guidance_does_not_need_a_new_header() {
        let mut projection = Projection::default();
        let worker = roster("main/worker", RosterStatus::Idle);
        projection.team.roster.insert(worker.handle.clone(), worker);
        let notice = Pending::of(&projection, FinishReason::Stop).notice(None, true);
        assert!(notice.contains("`archive` the subagents whose work you cannot integrate"));
        assert!(!notice.contains("HEADLESS RUN — FINAL"));
    }

    #[test]
    fn l4_terminal_roster_entries_do_not_hold_exec_open() {
        let mut projection = Projection::default();
        for status in [RosterStatus::Done, RosterStatus::Failed] {
            let entry = roster("main/worker", status);
            projection.team.roster.insert(entry.handle.clone(), entry);
            assert!(live_subagents(&projection).is_empty(), "{status:?}");
        }
    }

    #[test]
    fn l4_finished_workflow_actors_are_parked_not_pending() {
        let mut projection = Projection::default();
        let worker = roster("main/stage", RosterStatus::Idle);
        let member = hya_proto::MemberId::new();
        projection.session.members.push(
            serde_json::from_value(serde_json::json!({
                "member": member, "child": worker.session, "subagent_type": "worker",
                "description": "stage", "depth": 1, "status": "done"
            }))
            .expect("member fixture"),
        );
        projection.team.roster.insert(worker.handle.clone(), worker);
        let run = serde_json::json!({
            "id": hya_proto::WorkflowRunId::new(),
            "workflow": {"source": "test", "name": "test", "revision": "00".repeat(32)},
            "request_hash": "test", "owner": hya_proto::OwnerRunId::new(),
            "status": "completed", "stages": [{
                "id": "stage", "agent": "worker", "mode": "once", "level": 0,
                "status": "completed", "members": [{"member": member, "role": "worker", "iteration": 0}]
            }]
        });
        projection.session.workflow = Some(
            serde_json::from_value(serde_json::json!({"run": run})).expect("workflow fixture"),
        );
        assert!(live_subagents(&projection).is_empty());
        projection
            .session
            .workflow
            .as_mut()
            .unwrap()
            .run
            .as_mut()
            .unwrap()
            .status = hya_proto::WorkflowRunStatus::Running;
        assert_eq!(
            live_subagents(&projection).len(),
            1,
            "an active workflow still owns work"
        );
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
    fn u7_only_a_tool_free_last_step_counts() {
        let row = message(
            Role::Assistant,
            vec![text("Now let me check the file:"), tool("bash")],
        );
        let id = row.id;
        let mut projection = with_messages(vec![message(Role::User, vec![text("do it")]), row]);
        apply_step(&mut projection, id, 1);
        projection.session.messages[1]
            .parts
            .extend([text("Done."), text("Changed files: a.txt.")]);
        assert_eq!(
            final_step_text(&projection).as_deref(),
            Some("Done.\nChanged files: a.txt.")
        );
        assert_eq!(narration(&projection, FinishReason::Stop), None);

        let row = message(Role::Assistant, vec![tool("bash")]);
        let id = row.id;
        let mut announced = with_messages(vec![row]);
        apply_step(&mut announced, id, 1);
        announced.session.messages[0]
            .parts
            .push(text("Now let me verify the output:"));
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
        let notice = Pending::of(&projection, FinishReason::Stop).notice(None, false);
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
        assert_eq!(passing, 135, "corpus size");
        assert!(unexpected.is_empty(), "{}", unexpected.join("\n---\n"));
        assert_eq!(hit.len(), REVIEWED.len());
    }
    #[test]
    fn u11_dsml_and_xml_tool_leaks_are_caught() {
        for leak in [
            "｜DSML｜ calls>",
            "</｜DSML｜ invoke>",
            "<｜dsml｜ invoke>",
            "<result><name>bash</name><parameter name=\"command\">pwd</parameter></result>",
            "<invoke>pwd</invoke>",
            "<tool_call>pwd</tool_call>",
            "<function_calls>pwd</function_calls>",
            "calls>",
        ] {
            let projection = with_messages(vec![message(Role::Assistant, vec![text(leak)])]);
            assert!(
                matches!(
                    narration(&projection, FinishReason::Stop),
                    Some(Narration::MalformedToolMarkup(_))
                ),
                "{leak}"
            );
        }
        for summary in [
            "Example:\n\n```xml\n<result><parameter name=\"x\">1</parameter></result>\n```\n\nDone. The XML format is documented.",
            "Example:\n```xml\n<invoke>pwd</invoke>\n```",
        ] {
            let projection = with_messages(vec![message(Role::Assistant, vec![text(summary)])]);
            assert_eq!(
                narration(&projection, FinishReason::Stop),
                None,
                "{summary}"
            );
        }
        let executed = with_messages(vec![message(
            Role::Assistant,
            vec![text("｜DSML｜ calls>"), tool("bash")],
        )]);
        assert_eq!(narration(&executed, FinishReason::Stop), None);
        assert!(announces_next_step("Now let me check:\n\n---\n\n______"));
        assert!(!announces_next_step("Done.\n\n---"));
    }

    #[test]
    fn u12_spurious_compaction_summary_is_caught() {
        let projection = with_messages(vec![message(
            Role::Assistant,
            vec![text(
                "<analysis>Remaining work</analysis>\n<summary>Continue checking.</summary>  ",
            )],
        )]);
        assert_eq!(
            narration(&projection, FinishReason::Stop),
            Some(Narration::SpuriousSummary)
        );
        let normal = with_messages(vec![message(
            Role::Assistant,
            vec![text("<summary>User-facing result.</summary>")],
        )]);
        assert_eq!(narration(&normal, FinishReason::Stop), None);
    }
    #[test]
    fn l2_long_parameter_values_still_detect_a_result_suffix() {
        for ending in ["</result>", "</parameter></result>"] {
            let leak = format!(
                "<result><name>write</name><parameter name=\"content\">{}{}",
                "多行内容\n".repeat(200),
                ending
            );
            assert!(malformed_tool_markup(&leak).is_some(), "{ending}");
        }
    }

    #[test]
    fn l3_dsml_examples_in_closed_backtick_fences_are_not_leaks() {
        assert_eq!(
            malformed_tool_markup(
                "Example:\n```xml\n｜DSML｜ calls>\n</｜DSML｜ invoke>\n```\nDone."
            ),
            None
        );
    }

    #[test]
    fn l3_dsml_examples_in_closed_tilde_fences_are_not_leaks() {
        assert_eq!(
            malformed_tool_markup("Example:\n~~~xml\n｜DSML｜ calls>\n~~~\nDone."),
            None
        );
    }

    #[test]
    fn l3_xml_examples_in_closed_tilde_fences_are_not_leaks() {
        assert_eq!(
            malformed_tool_markup(
                "Example:\n~~~xml\n<parameter name=\"x\">value</result>\n~~~\nDone."
            ),
            None
        );
    }

    #[test]
    fn l3_inline_dsml_examples_are_not_leaks() {
        for text in [
            "Used `｜DSML｜ calls>` as the example.",
            "Used ``</｜DSML｜ invoke>`` as the example.",
        ] {
            assert_eq!(malformed_tool_markup(text), None, "{text}");
        }
    }

    #[test]
    fn l3_inline_xml_examples_are_not_leaks() {
        for text in [
            "The tags are `<parameter name=\"x\">` and `</result>`.",
            "Example: ``<parameter name=\"x\">`value`</result>``",
            "Example `<parameter name=\"x\">value` ends with </result>",
        ] {
            assert_eq!(malformed_tool_markup(text), None, "{text}");
        }
    }

    #[test]
    fn l3_ordinary_calls_word_endings_are_not_leaks() {
        for text in ["Vec<Calls>", "syscalls>", "callbacks>", "Unicode字calls>"] {
            assert_eq!(malformed_tool_markup(text), None, "{text}");
        }
    }

    #[test]
    fn l3_parameter_result_pair_must_end_the_output() {
        assert_eq!(
            malformed_tool_markup(
                "<parameter name=\"x\">value</result>\n\nDone. The format is documented."
            ),
            None
        );
    }

    #[test]
    fn l3_a_single_line_backtick_span_does_not_open_a_fence() {
        assert_eq!(
            malformed_tool_markup(
                "```foo```\n\n```\n<parameter name=\"x\">example</result>\n```\n\nDone."
            ),
            None
        );
    }

    #[test]
    fn l3_notice_names_dsml_without_repeating_the_glyph_sequence() {
        let projection = with_messages(vec![message(
            Role::Assistant,
            vec![text("｜DSML｜ calls>")],
        )]);
        let notice = Pending::of(&projection, FinishReason::Stop).notice(None, false);
        assert!(notice.contains("(DSML)"));
        assert!(!notice.contains("｜DSML｜"));
        assert_eq!(
            malformed_tool_markup("Ran it via a tool call instead of DSML markup."),
            None
        );
    }

    #[test]
    fn l3_unclosed_code_and_tool_call_tag_suffixes_still_detect_leaks() {
        for leak in [
            "```xml\n｜DSML｜ calls>",
            "~~~xml\n</invoke>",
            "`<parameter name=\"command\">pwd</result>",
            "<tool_calls>",
            "</tool_calls>",
            " calls>",
            "<calls>",
            "\n<result><parameter name=\"command\">pwd</result>  ",
            "```safe```\n</tool_call>",
        ] {
            assert!(malformed_tool_markup(leak).is_some(), "{leak}");
        }
    }

    fn apply_step(projection: &mut Projection, message: MessageId, step: u32) {
        projection.apply(&hya_proto::Envelope {
            seq: hya_proto::EventSeq(0),
            ts_millis: 0,
            event: hya_proto::Event::StepStarted {
                session: SessionId::new(),
                message,
                step,
            },
        });
    }

    #[test]
    fn l5_a_reasoning_only_last_step_does_not_reuse_previous_text() {
        let row = message(Role::Assistant, vec![text("Now verify the file:")]);
        let id = row.id;
        let mut projection = with_messages(vec![row]);
        apply_step(&mut projection, id, 1);
        projection.session.messages[0]
            .parts
            .push(PartProjection::Reasoning {
                id: PartId::new(),
                text: "thinking".into(),
                reason: None,
                provider_data: None,
            });
        assert_eq!(final_step_text(&projection).as_deref(), Some(""));
        assert_eq!(narration(&projection, FinishReason::Stop), None);
    }

    #[test]
    fn l5_a_step_with_tool_then_text_is_not_checked_as_narration() {
        let projection = with_messages(vec![message(
            Role::Assistant,
            vec![tool("bash"), text("</invoke>")],
        )]);
        assert_eq!(final_step_text(&projection), None);
        assert_eq!(narration(&projection, FinishReason::Stop), None);
    }
}
