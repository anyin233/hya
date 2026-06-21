//! Interactive terminal UI — the default `yaca` entry point.
//!
//! Owns terminal setup/teardown and the async event loop; rendering lives in the
//! pure `yaca-tui` crate. A spawned task runs each turn and streams its events
//! back through the engine `EventBus`, which this loop folds into the view.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt as _;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use yaca_core::completion::render_transcript;
use yaca_core::{AgentSpec, CreateSession, SessionEngine};
use yaca_proto::{ModelRef, SessionId, now_millis};
use yaca_provider::ReasoningEffort;
use yaca_tool::{
    Action as ToolAction, AskRequest, Decision, QuestionAnswer, QuestionKind, QuestionRequest,
};
use yaca_tui::{AppState, PermissionPrompt, QuestionPrompt};

use self::controller::{Controller, SessionSummary, TuiEffect};
use self::history::{HistoryStore, SessionMeta};

mod agents;
mod commands;
mod controller;
#[cfg(test)]
mod harness;
mod history;
mod prompt;
mod reference;

/// Restores the terminal on unwind or early return; the panic hook below covers
/// the message-printing path so a panic stays readable in cooked mode.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}

fn action_label(action: ToolAction) -> String {
    match action {
        ToolAction::Read => "read",
        ToolAction::Edit => "edit",
        ToolAction::Glob => "glob",
        ToolAction::Grep => "grep",
        ToolAction::Bash => "bash",
        ToolAction::Task => "task",
        ToolAction::Mcp => "mcp",
        ToolAction::ExternalDirectory => "external-dir",
    }
    .to_string()
}

fn session_summaries(history: &HistoryStore) -> Vec<SessionSummary> {
    history
        .list_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|meta| SessionSummary {
            id: meta.id,
            title: if meta.title == "Untitled session" && !meta.last_user_message.is_empty() {
                meta.last_user_message
            } else {
                meta.title
            },
            detail: format!("{} · {}", meta.model, meta.workdir),
        })
        .collect()
}

fn meta_for(history: &HistoryStore, id: &str) -> Option<SessionMeta> {
    history
        .list_sessions()
        .ok()?
        .into_iter()
        .find(|meta| meta.id == id)
}

fn reference_items(workdir: &Path) -> Vec<yaca_tui::DialogItem> {
    const MAX_ITEMS: usize = 250;
    let mut items = Vec::new();
    let mut stack = vec![(workdir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 3 || items.len() >= MAX_ITEMS {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths = entries.filter_map(Result::ok).collect::<Vec<_>>();
        paths.sort_by_key(|entry| entry.file_name());
        for entry in paths {
            if items.len() >= MAX_ITEMS {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), ".git" | "target" | ".worktrees") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(workdir) else {
                continue;
            };
            let label = format!("@{}", display_path(relative));
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let detail = if kind.is_dir() { "dir" } else { "file" };
            items.push(yaca_tui::DialogItem {
                label,
                detail: detail.to_string(),
            });
            if kind.is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
    items
}

fn all_reference_items(
    workdir: &Path,
    profiles: &[agents::AgentProfile],
) -> Vec<yaca_tui::DialogItem> {
    let mut items = agents::reference_items(profiles);
    items.extend(reference_items(workdir));
    items
}

fn custom_commands(workdir: &Path) -> Vec<commands::CustomCommand> {
    commands::load_markdown_commands(workdir).unwrap_or_default()
}

fn export_root() -> PathBuf {
    if let Ok(dir) = std::env::var("YACA_EXPORT_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".yaca/exports");
    }
    std::env::temp_dir().join("yaca/exports")
}

fn write_transcript_export(
    dir: &Path,
    session: SessionId,
    projection: &yaca_proto::Projection,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let short = session.to_string().chars().take(12).collect::<String>();
    let path = dir.join(format!("session-{short}-{}.md", now_millis()));
    let transcript = render_transcript(projection);
    std::fs::write(
        &path,
        format!("# yaca session {session}\n\n```text\n{transcript}\n```\n"),
    )
    .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

fn export_transcript(
    session: SessionId,
    projection: &yaca_proto::Projection,
) -> anyhow::Result<PathBuf> {
    write_transcript_export(&export_root(), session, projection)
}

pub struct InitResult {
    pub path: PathBuf,
    pub created: bool,
}

fn init_project_instructions(workdir: &Path) -> anyhow::Result<InitResult> {
    std::fs::create_dir_all(workdir).with_context(|| format!("create {}", workdir.display()))?;
    let path = workdir.join("AGENTS.md");
    if path.exists() {
        return Ok(InitResult {
            path,
            created: false,
        });
    }
    std::fs::write(
        &path,
        "# yaca project instructions\n\n- Keep changes focused and verified.\n- Run the relevant Rust checks before reporting completion.\n- Prefer existing project patterns over new abstractions.\n",
    )
    .with_context(|| format!("write {}", path.display()))?;
    Ok(InitResult {
        path,
        created: true,
    })
}

fn compact_summary(projection: &yaca_proto::Projection) -> String {
    const MAX_SUMMARY_BYTES: usize = 12 * 1024;
    let transcript = render_transcript(projection);
    let mut start = transcript.len().saturating_sub(MAX_SUMMARY_BYTES);
    while start < transcript.len() && !transcript.is_char_boundary(start) {
        start += 1;
    }
    let kept = &transcript[start..];
    format!(
        "The previous conversation was compacted. Preserve these facts and continue from the latest user intent.\n\n{kept}"
    )
}

fn route_agent_mention(
    profiles: &[agents::AgentProfile],
    agent: &mut AgentSpec,
    base_system_prompt: &str,
    prompt: String,
) -> String {
    let Some(routed) = agents::strip_leading_agent_mention(&prompt) else {
        return prompt;
    };
    if let Some(profile) = agents::profile_by_name(profiles, &routed.agent) {
        agents::apply_profile(agent, base_system_prompt, profile);
        routed.prompt
    } else {
        prompt
    }
}

fn display_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn decision_from(prompt: &PermissionPrompt) -> Decision {
    match prompt.selected {
        0 => Decision::AllowOnce,
        1 => Decision::AllowAlways,
        _ => Decision::Reject {
            feedback: (!prompt.reply.trim().is_empty()).then(|| prompt.reply.clone()),
        },
    }
}

fn handle_permission_key(key: KeyEvent, app: &mut AppState) -> Option<Decision> {
    let prompt = app.permission.as_mut()?;
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Some(Decision::Reject { feedback: None });
    }
    match key.code {
        KeyCode::Esc => Some(Decision::Reject { feedback: None }),
        KeyCode::Enter => Some(decision_from(prompt)),
        KeyCode::Left => {
            prompt.selected = prompt.selected.saturating_sub(1);
            None
        }
        KeyCode::Right | KeyCode::Tab => {
            prompt.selected = (prompt.selected + 1).min(2);
            None
        }
        KeyCode::Backspace => {
            prompt.reply.pop();
            None
        }
        KeyCode::Char(c) => {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                prompt.reply.push(c);
            }
            None
        }
        _ => None,
    }
}

fn handle_question_key(key: KeyEvent, app: &mut AppState) -> Option<QuestionAnswer> {
    let question = app.question.as_mut()?;
    match key.code {
        KeyCode::Esc => Some(QuestionAnswer::Cancelled),
        KeyCode::Enter => {
            if question.options.is_empty() || (question.allow_custom && !question.input.is_empty())
            {
                Some(QuestionAnswer::FreeText(std::mem::take(
                    &mut question.input,
                )))
            } else {
                Some(QuestionAnswer::Selected(question.selected))
            }
        }
        KeyCode::Up => {
            question.selected = question.selected.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            if !question.options.is_empty() {
                question.selected =
                    (question.selected + 1).min(question.options.len().saturating_sub(1));
            }
            None
        }
        KeyCode::Backspace => {
            question.input.pop();
            None
        }
        KeyCode::Char(c) => {
            if (question.options.is_empty() || question.allow_custom)
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            {
                question.input.push(c);
            }
            None
        }
        _ => None,
    }
}

fn apply_reasoning(agent: &mut AgentSpec, app: &mut AppState, level: &str) -> String {
    let trimmed = level.trim().to_ascii_lowercase();
    if matches!(trimmed.as_str(), "off" | "none") {
        agent.reasoning = None;
        app.reasoning_effort = None;
        return "thinking effort disabled".to_string();
    }
    if let Some(effort) = ReasoningEffort::parse(&trimmed) {
        agent.reasoning = Some(effort);
        app.reasoning_effort = Some(effort.as_str().to_string());
        return format!("thinking effort: {}", effort.as_str());
    }
    format!("unknown thinking effort '{level}' (use low|medium|high|off)")
}

fn spawn_turn(
    engine: &Arc<SessionEngine>,
    agent: &AgentSpec,
    session: SessionId,
    prompt: String,
    done_tx: &mpsc::UnboundedSender<()>,
    cancel: &CancellationToken,
) -> JoinHandle<()> {
    let engine = engine.clone();
    let agent = agent.clone();
    let done_tx = done_tx.clone();
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let prompt = reference::expand_mentions(&agent.workdir, &prompt)
            .unwrap_or_else(|e| format!("{prompt}\n\n[reference expansion error: {e}]"));
        if let Err(e) = engine.admit_user_prompt(session, prompt).await {
            let _ = engine
                .inject_system_message(session, format!("input error: {e}"))
                .await;
        } else if let Err(e) = engine.run_turn(session, &agent, cancel).await {
            let _ = engine
                .inject_system_message(session, format!("turn error: {e}"))
                .await;
        }
        let _ = done_tx.send(());
    })
}

pub struct RunOptions {
    pub model: String,
    pub models: Vec<String>,
    pub asks: mpsc::UnboundedReceiver<AskRequest>,
    pub questions: mpsc::UnboundedReceiver<QuestionRequest>,
    pub initial_session: SessionId,
    pub initial_yolo: bool,
}

pub async fn run(
    engine: Arc<SessionEngine>,
    mut agent: AgentSpec,
    options: RunOptions,
) -> anyhow::Result<()> {
    let RunOptions {
        model,
        models,
        mut asks,
        mut questions,
        initial_session,
        initial_yolo,
    } = options;
    let history = HistoryStore::from_env();
    let profiles = agents::builtin_profiles();
    let base_system_prompt = agent.system_prompt.clone();
    let mut session = initial_session;
    let _ = history.create_session(
        session,
        agent.model.as_str(),
        agent.name.as_str(),
        &agent.workdir.to_string_lossy(),
    );
    for env in engine.replay(session).await.unwrap_or_default() {
        let _ = history.append_envelope(session, &env);
    }
    let mut hydrated_sessions = HashSet::new();
    hydrated_sessions.insert(session);

    let mut bus = engine.bus().subscribe();
    let app = AppState {
        model,
        session_label: session.to_string().chars().take(12).collect(),
        projection: engine
            .read_projection(session)
            .await
            .context("prime projection")?,
        yolo: initial_yolo,
        reasoning_effort: agent.reasoning.map(|effort| effort.as_str().to_string()),
        ..AppState::default()
    };
    let mut controller =
        Controller::with_models_and_sessions(app, models, session_summaries(&history));
    controller.set_agents(agents::dialog_items(&profiles));
    controller.set_references(all_reference_items(&agent.workdir, &profiles));
    controller.set_custom_commands(custom_commands(&agent.workdir));

    install_panic_hook();
    enable_raw_mode().context("enable raw mode")?;
    execute!(io::stdout(), EnterAlternateScreen).context("enter alternate screen")?;
    let _guard = TerminalGuard;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("initialize terminal")?;

    let mut turn_cancel = CancellationToken::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
    let mut events = EventStream::new();
    let mut current_turn: Option<JoinHandle<()>> = None;
    let mut pending: Option<oneshot::Sender<Decision>> = None;
    let mut pending_question: Option<oneshot::Sender<QuestionAnswer>> = None;

    terminal
        .draw(|f| yaca_tui::draw(f, &mut controller.app))
        .context("draw")?;

    loop {
        tokio::select! {
            biased;
            msg = bus.recv() => match msg {
                Ok(env) => {
                    if env.event.session() == Some(session) {
                        controller.app.apply(&env);
                        let _ = history.append_envelope(session, &env);
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    let projection = engine.read_projection(session).await.unwrap_or_default();
                    controller.app.projection = projection;
                }
                Err(RecvError::Closed) => break,
            },
            _ = done_rx.recv() => {
                while let Ok(env) = bus.try_recv() {
                    if env.event.session() == Some(session) {
                        controller.app.apply(&env);
                        let _ = history.append_envelope(session, &env);
                    }
                }
                controller.app.running = false;
            }
            maybe_ask = asks.recv(), if controller.app.permission.is_none() => {
                if let Some(req) = maybe_ask {
                    if controller.app.yolo {
                        let _ = req.reply.send(Decision::AllowOnce);
                        continue;
                    }
                    let title = match req.session {
                        Some(origin) if origin != session => format!(
                            "{} · subagent {}",
                            action_label(req.action),
                            origin.to_string().chars().take(8).collect::<String>()
                        ),
                        _ => action_label(req.action),
                    };
                    pending = Some(req.reply);
                    controller.app.permission = Some(PermissionPrompt {
                        title,
                        detail: req.resource.pattern(),
                        selected: 0,
                        reply: String::new(),
                    });
                }
            }
            maybe_question = questions.recv(), if controller.app.permission.is_none() && controller.app.question.is_none() => {
                if let Some(req) = maybe_question {
                    let (options, allow_custom, input) = match req.kind {
                        QuestionKind::Select { options, allow_custom } => {
                            (options, allow_custom, String::new())
                        }
                        QuestionKind::FreeText { default } => {
                            (Vec::new(), false, default.unwrap_or_default())
                        }
                    };
                    pending_question = Some(req.reply);
                    controller.app.question = Some(QuestionPrompt {
                        prompt: req.prompt,
                        options,
                        selected: 0,
                        input,
                        allow_custom,
                    });
                }
            }
            maybe = events.next() => match maybe {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    if controller.app.permission.is_some() {
                        if let Some(decision) = handle_permission_key(key, &mut controller.app) {
                            if let Some(tx) = pending.take() {
                                let _ = tx.send(decision);
                            }
                            controller.app.permission = None;
                        }
                    } else if controller.app.question.is_some() {
                        if let Some(answer) = handle_question_key(key, &mut controller.app) {
                            if let Some(tx) = pending_question.take() {
                                let _ = tx.send(answer);
                            }
                            controller.app.question = None;
                        }
                    } else {
                        match controller.handle_key(key) {
                            TuiEffect::Exit => break,
                            TuiEffect::Interrupt => {
                                turn_cancel.cancel();
                            }
                            TuiEffect::SelectModel(model) => {
                                agent.model = ModelRef::new(model);
                            }
                            TuiEffect::SelectReasoning(level) => {
                                let message =
                                    apply_reasoning(&mut agent, &mut controller.app, &level);
                                let _ = engine.inject_system_message(session, message).await;
                            }
                            TuiEffect::SelectAgent(name) => {
                                if let Some(profile) = agents::profile_by_name(&profiles, &name) {
                                    agents::apply_profile(&mut agent, &base_system_prompt, profile);
                                    let _ = engine
                                        .inject_system_message(
                                            session,
                                            format!("selected agent profile: {name}"),
                                        )
                                        .await;
                                }
                            }
                            TuiEffect::SystemMessage(message) => {
                                let _ = engine.inject_system_message(session, message).await;
                            }
                            TuiEffect::CompactTranscript => {
                                let summary = compact_summary(&controller.app.projection);
                                let _ = engine.compact_context(session, summary).await;
                            }
                            TuiEffect::InitProject => {
                                let message = match init_project_instructions(&agent.workdir) {
                                    Ok(result) if result.created => {
                                        format!("created {}", result.path.display())
                                    }
                                    Ok(result) => {
                                        format!("{} already exists", result.path.display())
                                    }
                                    Err(e) => format!("init error: {e:#}"),
                                };
                                let _ = engine.inject_system_message(session, message).await;
                            }
                            TuiEffect::ExportTranscript => {
                                let message = match export_transcript(session, &controller.app.projection) {
                                    Ok(path) => format!("exported transcript to {}", path.display()),
                                    Err(e) => format!("export error: {e:#}"),
                                };
                                let _ = engine.inject_system_message(session, message).await;
                            }
                            TuiEffect::NewSession => {
                                turn_cancel.cancel();
                                let new_session = engine
                                    .create(CreateSession {
                                        parent: None,
                                        agent: agent.name.clone(),
                                        model: agent.model.clone(),
                                        workdir: agent.workdir.to_string_lossy().into_owned(),
                                    })
                                    .await
                                    .context("create new session")?;
                                session = new_session;
                                hydrated_sessions.insert(session);
                                let _ = history.create_session(
                                    session,
                                    agent.model.as_str(),
                                    agent.name.as_str(),
                                    &agent.workdir.to_string_lossy(),
                                );
                                controller.app.projection = engine
                                    .read_projection(session)
                                    .await
                                    .context("read new session projection")?;
                                controller.app.session_label =
                                    session.to_string().chars().take(12).collect();
                                controller.app.input.clear();
                                controller.app.scroll_back = 0;
                                controller.app.running = false;
                                controller.set_sessions(session_summaries(&history));
                                controller.set_agents(agents::dialog_items(&profiles));
                                controller.set_references(all_reference_items(&agent.workdir, &profiles));
                                controller.set_custom_commands(custom_commands(&agent.workdir));
                            }
                            TuiEffect::ResumeSession(id) => {
                                if let Ok(uuid) = uuid::Uuid::parse_str(&id) {
                                    let resume = SessionId::from_uuid(uuid);
                                    if !hydrated_sessions.contains(&resume) {
                                        let _ = history.hydrate_store(engine.store(), resume).await;
                                        hydrated_sessions.insert(resume);
                                    }
                                    session = resume;
                                    if let Some(meta) = meta_for(&history, &id) {
                                        controller.app.model = meta.model.clone();
                                        agent.model = ModelRef::new(meta.model);
                                        let workdir = PathBuf::from(meta.workdir);
                                        controller.set_agents(agents::dialog_items(&profiles));
                                        controller.set_references(all_reference_items(&workdir, &profiles));
                                        controller.set_custom_commands(custom_commands(&workdir));
                                    }
                                    controller.app.projection = engine
                                        .read_projection(session)
                                        .await
                                        .context("read resumed session projection")?;
                                    controller.app.session_label =
                                        session.to_string().chars().take(12).collect();
                                    controller.app.input.clear();
                                    controller.app.scroll_back = 0;
                                    controller.app.running = false;
                                }
                            }
                            TuiEffect::Submit(prompt) => {
                                let prompt = route_agent_mention(
                                    &profiles,
                                    &mut agent,
                                    &base_system_prompt,
                                    prompt,
                                );
                                controller.app.running = true;
                                turn_cancel = CancellationToken::new();
                                current_turn = Some(spawn_turn(
                                    &engine, &agent, session, prompt, &done_tx, &turn_cancel,
                                ));
                            }
                            TuiEffect::SubmitConfigured {
                                prompt,
                                agent: command_agent,
                                model,
                            } => {
                                if let Some(model) = model {
                                    agent.model = ModelRef::new(model);
                                }
                                if let Some(profile) = command_agent
                                    .as_deref()
                                    .and_then(|name| agents::profile_by_name(&profiles, name))
                                {
                                    agents::apply_profile(
                                        &mut agent,
                                        &base_system_prompt,
                                        profile,
                                    );
                                }
                                let prompt = route_agent_mention(
                                    &profiles,
                                    &mut agent,
                                    &base_system_prompt,
                                    prompt,
                                );
                                controller.app.running = true;
                                turn_cancel = CancellationToken::new();
                                current_turn = Some(spawn_turn(
                                    &engine, &agent, session, prompt, &done_tx, &turn_cancel,
                                ));
                            }
                            TuiEffect::None => {}
                        }
                    }
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    let _ = controller.handle_mouse(mouse);
                }
                Some(Ok(Event::Paste(text))) => {
                    let _ = controller.handle_paste(&text);
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
        terminal
            .draw(|f| yaca_tui::draw(f, &mut controller.app))
            .context("draw")?;
    }

    if let Some(tx) = pending.take() {
        let _ = tx.send(Decision::Reject {
            feedback: Some("cancelled".to_string()),
        });
    }
    while let Ok(req) = asks.try_recv() {
        let _ = req.reply.send(Decision::Reject {
            feedback: Some("cancelled".to_string()),
        });
    }
    if let Some(tx) = pending_question.take() {
        let _ = tx.send(QuestionAnswer::Cancelled);
    }
    while let Ok(req) = questions.try_recv() {
        let _ = req.reply.send(QuestionAnswer::Cancelled);
    }
    turn_cancel.cancel();
    if let Some(handle) = current_turn.take() {
        let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn export_writes_markdown_transcript_file() {
        let root = std::env::temp_dir().join(format!(
            "yaca-export-test-{}-{}",
            now_millis(),
            std::process::id()
        ));
        let session = SessionId::new();
        let projection = yaca_proto::Projection::default();

        let path = write_transcript_export(&root, session, &projection).unwrap();

        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.starts_with("# yaca session "));
        assert!(text.contains("```text"));
    }

    #[test]
    fn init_project_writes_agents_md_without_overwriting_existing_file() {
        let root = std::env::temp_dir().join(format!(
            "yaca-init-test-{}-{}",
            now_millis(),
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let created = init_project_instructions(&root).unwrap();
        assert!(created.created);
        assert!(root.join("AGENTS.md").exists());

        let second = init_project_instructions(&root).unwrap();
        assert!(!second.created);
    }
}
