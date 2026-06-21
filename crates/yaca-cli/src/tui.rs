//! Interactive terminal UI — the default `yaca` entry point.
//!
//! Owns terminal setup/teardown and the async event loop; rendering lives in the
//! pure `yaca-tui` crate. A spawned task runs each turn and streams its events
//! back through the engine `EventBus`, which this loop folds into the view.

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
use yaca_tui::{AppState, PermissionPrompt, SessionPicker};

use crate::commands;

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

enum Action {
    None,
    Quit,
    Submit(String),
}

fn handle_key(key: KeyEvent, app: &mut AppState) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        return Action::Quit;
    }
    match key.code {
        KeyCode::Esc => Action::Quit,
        KeyCode::Enter => {
            if app.running || app.input.trim().is_empty() {
                Action::None
            } else {
                app.scroll_back = 0;
                Action::Submit(std::mem::take(&mut app.input))
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            Action::None
        }
        KeyCode::PageUp => {
            app.scroll_up(5);
            Action::None
        }
        KeyCode::PageDown => {
            app.scroll_down(5);
            Action::None
        }
        KeyCode::Up => {
            app.scroll_up(1);
            Action::None
        }
        KeyCode::Down => {
            app.scroll_down(1);
            Action::None
        }
        KeyCode::Char(c) => {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                app.input.push(c);
            }
            Action::None
        }
        _ => Action::None,
    }
}

fn action_label(action: ToolAction) -> &'static str {
    match action {
        ToolAction::Read => "read",
        ToolAction::Edit => "edit",
        ToolAction::Glob => "glob",
        ToolAction::Grep => "grep",
        ToolAction::Bash => "bash",
        ToolAction::Task => "task",
        ToolAction::ExternalDirectory => "external-dir",
    }
}

fn decision_for(selected: usize) -> Decision {
    match selected {
        0 => Decision::AllowOnce,
        1 => Decision::AllowAlways,
        _ => Decision::Reject { feedback: None },
    }
}

fn handle_permission_key(key: KeyEvent, app: &mut AppState) -> Option<Decision> {
    let prompt = app.permission.as_mut()?;
    match key.code {
        KeyCode::Esc => Some(Decision::Reject { feedback: None }),
        KeyCode::Enter => Some(decision_for(prompt.selected)),
        KeyCode::Char('a') => Some(Decision::AllowOnce),
        KeyCode::Char('s') => Some(Decision::AllowAlways),
        KeyCode::Char('d') => Some(Decision::Reject { feedback: None }),
        KeyCode::Left => {
            prompt.selected = prompt.selected.saturating_sub(1);
            None
        }
        KeyCode::Right | KeyCode::Tab => {
            prompt.selected = (prompt.selected + 1).min(2);
            None
        }
        _ => None,
    }
}

enum PickerAction {
    Switch(usize),
    Close,
    None,
}

fn handle_picker_key(key: KeyEvent, app: &mut AppState) -> PickerAction {
    let Some(picker) = app.session_picker.as_mut() else {
        return PickerAction::None;
    };
    match key.code {
        KeyCode::Esc => PickerAction::Close,
        KeyCode::Enter => PickerAction::Switch(picker.selected),
        KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
            PickerAction::None
        }
        KeyCode::Down => {
            picker.selected = (picker.selected + 1).min(picker.entries.len().saturating_sub(1));
            PickerAction::None
        }
        _ => PickerAction::None,
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
    mut asks: mpsc::UnboundedReceiver<AskRequest>,
    initial_session: SessionId,
) -> anyhow::Result<()> {
    let mut session = initial_session;

    let mut bus = engine.bus().subscribe();
    let mut app = AppState {
        model,
        session_label: session.to_string().chars().take(12).collect(),
        projection: engine
            .read_projection(session)
            .await
            .context("prime projection")?,
        ..AppState::default()
    };

    install_panic_hook();
    enable_raw_mode().context("enable raw mode")?;
    execute!(io::stdout(), EnterAlternateScreen).context("enter alternate screen")?;
    let _guard = TerminalGuard;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("initialize terminal")?;

    let cancel = CancellationToken::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
    let mut events = EventStream::new();
    let mut current_turn: Option<JoinHandle<()>> = None;
    let mut pending: Option<oneshot::Sender<Decision>> = None;
    let mut picker_sessions: Vec<SessionId> = Vec::new();
    let template_dirs: Vec<std::path::PathBuf> = {
        let mut v = vec![std::path::PathBuf::from(".yaca/prompts")];
        if let Some(home) = std::env::var_os("HOME") {
            v.push(std::path::PathBuf::from(home).join(".config/yaca/prompts"));
        }
        v
    };

    terminal
        .draw(|f| yaca_tui::draw(f, &mut app))
        .context("draw")?;

    loop {
        tokio::select! {
            biased;
            msg = bus.recv() => match msg {
                Ok(env) => {
                    if env.event.session() == Some(session) {
                        app.apply(&env);
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    let projection = engine.read_projection(session).await.unwrap_or_default();
                    app.projection = projection;
                }
                Err(RecvError::Closed) => break,
            },
            _ = done_rx.recv() => {
                while let Ok(env) = bus.try_recv() {
                    if env.event.session() == Some(session) {
                        app.apply(&env);
                    }
                }
                app.running = false;
            }
            maybe_ask = asks.recv(), if app.permission.is_none() => {
                if let Some(req) = maybe_ask {
                    let title = match req.session {
                        Some(origin) if origin != session => format!(
                            "{} · subagent {}",
                            action_label(req.action),
                            origin.to_string().chars().take(8).collect::<String>()
                        ),
                        _ => action_label(req.action).to_string(),
                    };
                    pending = Some(req.reply);
                    app.permission = Some(PermissionPrompt {
                        title,
                        detail: req.resource.pattern(),
                        selected: 0,
                    });
                }
            }
            maybe = events.next() => match maybe {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                    {
                        break;
                    }
                    if app.permission.is_some() {
                        if let Some(decision) = handle_permission_key(key, &mut app) {
                            if let Some(tx) = pending.take() {
                                let _ = tx.send(decision);
                            }
                            app.permission = None;
                        }
                    } else if app.session_picker.is_some() {
                        match handle_picker_key(key, &mut app) {
                            PickerAction::Switch(i) => {
                                if let Some(&picked) = picker_sessions.get(i) {
                                    session = picked;
                                    app.session_label =
                                        session.to_string().chars().take(12).collect();
                                    app.projection =
                                        engine.read_projection(session).await.unwrap_or_default();
                                }
                                app.session_picker = None;
                            }
                            PickerAction::Close => app.session_picker = None,
                            PickerAction::None => {}
                        }
                    } else {
                        match handle_key(key, &mut app) {
                            Action::Quit => break,
                            Action::Submit(input) => match commands::parse_slash(&input) {
                                Some(commands::Slash::Exit) => break,
                                Some(commands::Slash::Help) => {
                                    let _ = engine
                                        .inject_system_message(session, commands::help_text())
                                        .await;
                                }
                                Some(commands::Slash::Model(m)) if !m.is_empty() => {
                                    agent.model = ModelRef::new(&m);
                                    app.model = m.clone();
                                    let _ = engine
                                        .inject_system_message(session, format!("model set to {m}"))
                                        .await;
                                }
                                Some(commands::Slash::Model(_)) => {
                                    let _ = engine
                                        .inject_system_message(
                                            session,
                                            format!("current model: {}", app.model),
                                        )
                                        .await;
                                }
                                Some(commands::Slash::Clear) => {
                                    if let Ok(new_session) = engine
                                        .create(CreateSession {
                                            parent: None,
                                            agent: agent.name.clone(),
                                            model: agent.model.clone(),
                                            workdir: agent.workdir.to_string_lossy().into_owned(),
                                        })
                                        .await
                                    {
                                        session = new_session;
                                        app.session_label =
                                            session.to_string().chars().take(12).collect();
                                        app.projection = engine
                                            .read_projection(session)
                                            .await
                                            .unwrap_or_default();
                                    }
                                }
                                Some(commands::Slash::Sessions) => {
                                    let sessions =
                                        engine.store().list_sessions().await.unwrap_or_default();
                                    if sessions.is_empty() {
                                        let _ = engine
                                            .inject_system_message(
                                                session,
                                                "no sessions found".to_string(),
                                            )
                                            .await;
                                    } else {
                                        picker_sessions =
                                            sessions.iter().map(|s| s.session).collect();
                                        let entries = sessions
                                            .iter()
                                            .map(|s| format!("{} ({} events)", s.session, s.events))
                                            .collect();
                                        app.session_picker =
                                            Some(SessionPicker { entries, selected: 0 });
                                    }
                                }
                                Some(commands::Slash::Template(name)) => {
                                    match commands::resolve_template(&name, &template_dirs) {
                                        Some(tpl) => {
                                            app.running = true;
                                            current_turn = Some(spawn_turn(
                                                &engine, &agent, session, tpl, &done_tx, &cancel,
                                            ));
                                        }
                                        None => {
                                            let _ = engine
                                                .inject_system_message(
                                                    session,
                                                    format!("unknown command: /{name}"),
                                                )
                                                .await;
                                        }
                                    }
                                }
                                None => {
                                    app.running = true;
                                    current_turn = Some(spawn_turn(
                                        &engine, &agent, session, input, &done_tx, &cancel,
                                    ));
                                }
                            },
                            Action::None => {}
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
        terminal
            .draw(|f| yaca_tui::draw(f, &mut app))
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
    cancel.cancel();
    if let Some(handle) = current_turn.take() {
        let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;
    }
    Ok(())
}
