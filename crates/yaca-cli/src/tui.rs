//! Interactive terminal UI — the default `yaca` entry point.
//!
//! Owns terminal setup/teardown and the async event loop; rendering lives in the
//! pure `yaca-tui` crate. A spawned task runs each turn and streams its events
//! back through the engine `EventBus`, which this loop folds into the view.

use std::collections::HashSet;
use std::io;
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
use yaca_core::{AgentSpec, CreateSession, SessionEngine};
use yaca_proto::{ModelRef, SessionId};
use yaca_tool::{Action as ToolAction, AskRequest, Decision};
use yaca_tui::{AppState, PermissionPrompt};

use self::controller::{Controller, SessionSummary, TuiEffect};
use self::history::{HistoryStore, SessionMeta};

mod commands;
mod controller;
#[cfg(test)]
mod harness;
mod history;

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

pub async fn run(
    engine: Arc<SessionEngine>,
    mut agent: AgentSpec,
    model: String,
    models: Vec<String>,
    mut asks: mpsc::UnboundedReceiver<AskRequest>,
) -> anyhow::Result<()> {
    let history = HistoryStore::from_env();
    let mut session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .context("create session")?;
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
        ..AppState::default()
    };
    let mut controller =
        Controller::with_models_and_sessions(app, models, session_summaries(&history));

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
                    pending = Some(req.reply);
                    controller.app.permission = Some(PermissionPrompt {
                        title: action_label(req.action),
                        detail: req.resource.pattern(),
                        selected: 0,
                        reply: String::new(),
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
                    } else {
                        match controller.handle_key(key) {
                            TuiEffect::Exit => break,
                            TuiEffect::Interrupt => {
                                turn_cancel.cancel();
                            }
                            TuiEffect::SelectModel(model) => {
                                agent.model = ModelRef::new(model);
                            }
                            TuiEffect::SystemMessage(message) => {
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
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
        terminal
            .draw(|f| yaca_tui::draw(f, &mut controller.app))
            .context("draw")?;
    }

    turn_cancel.cancel();
    if let Some(handle) = current_turn.take() {
        let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;
    }
    Ok(())
}
