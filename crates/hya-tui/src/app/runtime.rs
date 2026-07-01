use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hya_sdk::{Client, SdkError};
use serde_json::json;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::contracts::{Key, KeyEvent, PromptDoc};
use crate::keymap::{
    default_dispatcher, DispatchOutcome, KeymapDispatcher, KeymapMode, ParseKeyBindingError,
};
use crate::render::{
    scroll::ScrollState,
    transcript::{format_store_transcript, TranscriptOptions},
};
use crate::screens;
use crate::state::{AppState, Route};
use crate::theme::{builtin_theme, builtin_themes, resolve, Mode, ThemeError, DEFAULT_THEME};
use crate::tui::{spawn_input_task, Tui};
use crate::widgets::dialog_select::{DialogSelect, DialogSelectItem};

use super::{AppEvent, MouseKind};

const COMMAND_PAGE_UP: &str = "session.page.up";
const COMMAND_PAGE_DOWN: &str = "session.page.down";
const COMMAND_LINE_UP: &str = "session.line.up";
const COMMAND_LINE_DOWN: &str = "session.line.down";
const COMMAND_HALF_UP: &str = "session.half.page.up";
const COMMAND_HALF_DOWN: &str = "session.half.page.down";
const COMMAND_FIRST: &str = "session.first";
const COMMAND_LAST: &str = "session.last";
const COMMAND_SESSION_NEW: &str = "session.new";
const COMMAND_COMMAND_PALETTE: &str = "command.palette.show";
const COMMAND_AGENT_LIST: &str = "agent.list";
const COMMAND_MODEL_LIST: &str = "model.list";
const COMMAND_YOLO_SWITCH: &str = "permission.yolo.switch";
const COMMAND_VARIANT_LIST: &str = "variant.list";
const COMMAND_VARIANT_CYCLE: &str = "variant.cycle";
const COMMAND_SESSION_LIST: &str = "session.list";
const COMMAND_SESSION_TIMELINE: &str = "session.timeline";
const COMMAND_THEME_LIST: &str = "theme.switch";
const COMMAND_SESSION_RENAME: &str = "session.rename";
const COMMAND_SESSION_DELETE: &str = "session.delete";
const COMMAND_SESSION_COMPACT: &str = "session.compact";
const COMMAND_HELP: &str = "app.help";
const COMMAND_THEME_MODE: &str = "theme.switch_mode";
const COMMAND_TOGGLE_TIMESTAMPS: &str = "session.toggle_timestamps";
const COMMAND_MESSAGES_COPY: &str = "messages.copy";
const COMMAND_SESSION_COPY: &str = "session.copy";
const COMMAND_SESSION_EXPORT: &str = "session.export";
const COMMAND_SESSION_UNDO: &str = "session.undo";
const COMMAND_SESSION_REDO: &str = "session.redo";
const COMMAND_SIDEBAR_TOGGLE: &str = "session.sidebar.toggle";
const COMMAND_SESSION_INTERRUPT: &str = "session.interrupt";
const COMMAND_HYA_STATUS: &str = "hya.status";
const COMMAND_PROMPT_EDITOR: &str = "prompt.editor";
const COMMAND_SESSION_CHILD_FIRST: &str = "session.child.first";
const COMMAND_SESSION_PARENT: &str = "session.parent";
const COMMAND_SESSION_CHILD_NEXT: &str = "session.child.next";
const COMMAND_SESSION_CHILD_PREVIOUS: &str = "session.child.previous";
const COMMAND_SESSION_QUEUED_PROMPTS: &str = "session.queued_prompts";
const COMMAND_TIPS_TOGGLE: &str = "tips.toggle";
const COMMAND_TOGGLE_CONCEAL: &str = "session.toggle.conceal";
const TOAST_DURATION: Duration = Duration::from_millis(3000);
const CONNECTING_TIP: &str =
    "\u{27f3} Starting backend\u{2026} you can type now; prompts send once it is ready";
const CONNECTING_PLACEHOLDER: &str = "Starting backend\u{2026} type to queue a prompt";

const PALETTE_TUI_COMMANDS: &[(&str, &str, &str, &str, bool)] = &[
    (
        "New session",
        COMMAND_SESSION_NEW,
        "ctrl+x n",
        "Session",
        true,
    ),
    (
        "Switch session",
        COMMAND_SESSION_LIST,
        "ctrl+x l",
        "Session",
        true,
    ),
    (
        "Switch model",
        COMMAND_MODEL_LIST,
        "ctrl+x m",
        "Agent",
        true,
    ),
    ("YOLO mode", COMMAND_YOLO_SWITCH, "", "Permission", true),
    (
        "Switch variant",
        COMMAND_VARIANT_LIST,
        "ctrl+t",
        "Agent",
        false,
    ),
    (
        "Switch agent",
        COMMAND_AGENT_LIST,
        "ctrl+x a",
        "Agent",
        false,
    ),
    (
        "Copy last assistant message",
        COMMAND_MESSAGES_COPY,
        "ctrl+x y",
        "Session",
        false,
    ),
    (
        "Copy session transcript",
        COMMAND_SESSION_COPY,
        "",
        "Session",
        false,
    ),
    (
        "Export session transcript",
        COMMAND_SESSION_EXPORT,
        "ctrl+x x",
        "Session",
        false,
    ),
    ("Theme mode", COMMAND_THEME_MODE, "", "System", false),
    (
        "Timestamps",
        COMMAND_TOGGLE_TIMESTAMPS,
        "",
        "Session",
        false,
    ),
    (
        "Open editor",
        COMMAND_PROMPT_EDITOR,
        "ctrl+x e",
        "Session",
        false,
    ),
];

fn command_palette_items(
    yolo: bool,
    theme_mode: Mode,
    show_timestamps: bool,
    command_names: &[String],
) -> Vec<DialogSelectItem<String>> {
    let suggested = PALETTE_TUI_COMMANDS
        .iter()
        .filter(|(_, _, _, _, suggested)| *suggested)
        .map(|(title, id, keybind, _, _)| {
            DialogSelectItem::new(
                palette_command_title(title, id, yolo, theme_mode, show_timestamps),
                (*id).to_owned(),
            )
            .with_description((*keybind).to_owned())
            .with_category("Suggested")
            .unfiltered_only()
        });
    let commands = PALETTE_TUI_COMMANDS
        .iter()
        .map(|(title, id, keybind, category, _)| {
            DialogSelectItem::new(
                palette_command_title(title, id, yolo, theme_mode, show_timestamps),
                (*id).to_owned(),
            )
            .with_description((*keybind).to_owned())
            .with_category((*category).to_owned())
        });
    let slash = command_names
        .iter()
        .map(|name| DialogSelectItem::new(name.clone(), name.clone()).with_category("Command"));
    suggested.chain(commands).chain(slash).collect()
}

fn palette_command_title(
    title: &str,
    id: &str,
    yolo: bool,
    theme_mode: Mode,
    show_timestamps: bool,
) -> String {
    match id {
        COMMAND_YOLO_SWITCH => {
            if yolo {
                "Disable YOLO mode"
            } else {
                "Enable YOLO mode"
            }
        }
        COMMAND_THEME_MODE => match theme_mode {
            Mode::Dark => "Switch to light mode",
            Mode::Light => "Switch to dark mode",
        },
        COMMAND_TOGGLE_TIMESTAMPS => {
            if show_timestamps {
                "Hide timestamps"
            } else {
                "Show timestamps"
            }
        }
        _ => title,
    }
    .to_owned()
}

fn toggle_yolo(yolo: &mut bool) -> &'static str {
    *yolo = !*yolo;
    if *yolo {
        "yolo mode enabled"
    } else {
        "yolo mode disabled"
    }
}

pub struct RunTuiInput {
    pub tui: Tui,
    pub state: AppState,
    pub client: Arc<dyn Client>,
    pub events: mpsc::UnboundedReceiver<AppEvent>,
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub input_task: Option<tokio::task::JoinHandle<()>>,
    pub default_agent: Option<String>,
    pub default_model: Option<(String, String)>,
    pub agent_names: Vec<String>,
}

impl Drop for RunTuiInput {
    fn drop(&mut self) {
        if let Some(input_task) = self.input_task.take() {
            input_task.abort();
        }
    }
}

#[derive(Debug, Error)]
pub enum AppRunError {
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
    #[error("sdk error: {0}")]
    Sdk(#[from] SdkError),
    #[error("theme error: {0}")]
    Theme(#[from] ThemeError),
    #[error("keymap error: {0}")]
    Keymap(#[from] ParseKeyBindingError),
}

pub async fn run_tui(input: RunTuiInput) -> Result<(), AppRunError> {
    let mut runtime = Runtime::new(input)?;
    let result = async {
        runtime.refresh_session_list();
        runtime.draw().await?;
        loop {
            let event = if runtime.leader_pending {
                let timeout = runtime.dispatcher.leader_timeout();
                match tokio::time::timeout(timeout, runtime.input.events.recv()).await {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(_) => {
                        runtime.clear_leader();
                        runtime.draw().await?;
                        continue;
                    }
                }
            } else if let Some(deadline) = runtime.toast_deadline() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, runtime.input.events.recv()).await {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(_) => {
                        runtime.clear_toast();
                        runtime.draw().await?;
                        continue;
                    }
                }
            } else if runtime.animating {
                match tokio::time::timeout(
                    crate::widgets::spinner::INTERVAL,
                    runtime.input.events.recv(),
                )
                .await
                {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(_) => {
                        runtime.draw().await?;
                        continue;
                    }
                }
            } else {
                match runtime.input.events.recv().await {
                    Some(event) => event,
                    None => break,
                }
            };
            let quit = runtime.handle_event(event).await?;
            runtime.open_pending_editor().await?;
            runtime.draw().await?;
            if quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    runtime.abort_input_task().await;
    runtime.input.tui.restore()?;
    result
}

#[must_use]
pub fn prompt_request_body(doc: &PromptDoc) -> serde_json::Value {
    json!({"parts":[{"type":"text","text":doc.text}]})
}

fn model_spec(provider_id: &str, model_id: &str, variant: Option<&str>) -> serde_json::Value {
    let mut spec = json!({ "providerID": provider_id, "modelID": model_id });
    if let Some(variant) = variant {
        spec["variant"] = json!(variant);
    }
    spec
}

fn trailing_mention(text: &str) -> Option<String> {
    let at = text.rfind('@')?;
    let after = &text[at + 1..];
    if after.chars().any(char::is_whitespace) {
        return None;
    }
    Some(after.to_owned())
}

fn slash_command(text: &str, names: &[String]) -> Option<(String, String)> {
    let rest = text.strip_prefix('/')?;
    let (name, arguments) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim_start()),
        None => (rest, ""),
    };
    if name.is_empty() || name.contains('/') || !names.iter().any(|candidate| candidate == name) {
        return None;
    }
    Some((name.to_owned(), arguments.to_owned()))
}

/// The command name in `/name ...` input that has command syntax (one leading `/`-prefixed
/// token with no further `/`), whether or not the command is registered. Distinguishes an
/// unknown command (`/bogus`) from a path (`/usr/bin`) or a plain prompt, so the former can
/// be rejected instead of silently sent to the model.
fn command_like_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    let name = match rest.split_once(char::is_whitespace) {
        Some((name, _)) => name,
        None => rest,
    };
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

/// Built-in slash commands that are client-side UI actions (open a selector/dialog, start a
/// session) rather than prompt macros run by the backend. Returns the client command id to
/// dispatch via `Runtime::handle_command`. Without this, typing `/help`, `/model` or
/// `/new` in the prompt would be sent to the model as a prompt (or rejected as unknown for
/// names absent from the backend command catalog), so the command never performs its action.
/// Aliases mirror the `--mini` TUI vocabulary so both frontends accept the same names. Any
/// trailing arguments are ignored — the action is still invoked. Prompt-macro commands and
/// custom/unknown commands fall through.
fn builtin_client_command(text: &str) -> Option<&'static str> {
    let name = command_like_name(text)?;
    match name {
        "help" => Some(COMMAND_HELP),
        "model" | "models" => Some(COMMAND_MODEL_LIST),
        "new" | "clear" => Some(COMMAND_SESSION_NEW),
        "agent" | "agents" => Some(COMMAND_AGENT_LIST),
        "sessions" | "resume" => Some(COMMAND_SESSION_LIST),
        "compact" => Some(COMMAND_SESSION_COMPACT),
        "tools" | "mcp" => Some(COMMAND_HYA_STATUS),
        "think" => Some(COMMAND_VARIANT_LIST),
        "export" => Some(COMMAND_SESSION_EXPORT),
        "?" => Some(COMMAND_HELP),
        _ => None,
    }
}

fn builtin_quit_command(text: &str) -> bool {
    matches!(command_like_name(text), Some("quit" | "exit" | "q"))
}

#[derive(Debug, PartialEq, Eq)]
struct EditorCommand {
    program: String,
    args: Vec<String>,
}

fn parse_editor_command(editor: &str) -> Option<EditorCommand> {
    let mut parts = editor.split_whitespace();
    let program = parts.next()?.to_owned();
    Some(EditorCommand {
        program,
        args: parts.map(str::to_owned).collect(),
    })
}

fn editor_command_from_env() -> Option<EditorCommand> {
    std::env::var("VISUAL")
        .ok()
        .and_then(|value| parse_editor_command(&value))
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .and_then(|value| parse_editor_command(&value))
        })
}

fn normalize_editor_content(content: &str) -> String {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_owned()
}

fn run_editor_command(command: &EditorCommand, prompt_text: &str) -> io::Result<String> {
    let file = std::env::temp_dir().join(format!("hya-edit-{}.md", timestamp_nanos()));
    if let Err(error) = fs::write(&file, prompt_text) {
        let _ = fs::remove_file(&file);
        return Err(error);
    }
    let status = Command::new(&command.program)
        .args(&command.args)
        .arg(&file)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    let result = match status {
        Ok(status) if status.success() => fs::read_to_string(&file),
        Ok(status) => Err(io::Error::other(format!("editor exited with {status}"))),
        Err(error) => Err(error),
    };
    let _ = fs::remove_file(&file);
    result
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn rect_contains(area: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

fn hit(rect: Option<crate::contracts::Rect>, column: u16, row: u16) -> bool {
    rect.is_some_and(|r| {
        column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
    })
}

#[derive(Clone, Copy)]
enum DialogKind {
    CommandPalette,
    AgentSwitch,
    ModelSwitch,
    VariantList,
    SessionList,
    Timeline,
    ThemeList,
    Help,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            DialogKind::CommandPalette => "Commands",
            DialogKind::AgentSwitch => "Agents",
            DialogKind::ModelSwitch => "Select model",
            DialogKind::VariantList => "Select variant",
            DialogKind::SessionList => "Sessions",
            DialogKind::Timeline => "Timeline",
            DialogKind::ThemeList => "Themes",
            DialogKind::Help => "Keybinds",
        }
    }
}

struct ActiveDialog {
    select: DialogSelect<String>,
    kind: DialogKind,
}

enum PromptAction {
    RenameSession(String),
}

struct PromptDialog {
    action: PromptAction,
    title: &'static str,
    input: String,
}

enum ConfirmAction {
    DeleteSession(String),
}

struct ConfirmDialog {
    action: ConfirmAction,
    title: &'static str,
    message: String,
    selected: usize,
}

#[derive(Clone)]
struct ActiveQuestion {
    request_id: String,
    directory: Option<String>,
    request: serde_json::Value,
}

struct Runtime {
    input: RunTuiInput,
    prompt: PromptDoc,
    submitted_prompts: Vec<String>,
    /// Prompts typed before the backend finished connecting, replayed on `BackendReady`.
    pending_prompts: Vec<String>,
    backend_ready: bool,
    history_index: Option<usize>,
    command_names: Vec<String>,
    model_names: Vec<(String, String, String)>,
    model_limits: std::collections::HashMap<String, i64>,
    model_variants: std::collections::HashMap<String, Vec<String>>,
    active_variant: Option<String>,
    mcp_status: Vec<(String, String)>,
    lsp_status: Vec<(String, String, String)>,
    formatter_status: Vec<String>,
    plugin_status: Vec<(String, Option<String>)>,
    status_dialog_open: bool,
    session_entries: Vec<(String, String)>,
    timeline_entries: Vec<(String, String, String)>,
    theme_names: Vec<String>,
    dialog: Option<ActiveDialog>,
    prompt_dialog: Option<PromptDialog>,
    confirm_dialog: Option<ConfirmDialog>,
    session_dialog_pending: bool,
    timeline_dialog_pending: bool,
    active_agent: Option<String>,
    active_model: Option<(String, String)>,
    agent_models: std::collections::HashMap<String, (String, String)>,
    leader_pending: bool,
    toast: Option<(String, Instant)>,
    tip: &'static str,
    placeholder: &'static str,
    started: Instant,
    animating: bool,
    show_timestamps: bool,
    sidebar_visible: bool,
    show_tips: bool,
    subagent_nav: bool,
    dispatcher: KeymapDispatcher,
    theme: crate::theme::ResolvedTheme,
    theme_name: String,
    theme_mode: Mode,
    scroll: ScrollState,
    permission_selected: usize,
    permission_stage: screens::permission::Stage,
    permission_reject_message: String,
    active_permission: Option<(String, String, bool)>,
    question_state: screens::question::QuestionState,
    active_question: Option<ActiveQuestion>,
    pending_editor: bool,
    prompt_hits: crate::screens::prompt_box::PromptHits,
    /// Client-side auto-approve ("yolo"): when on, tool permission requests are
    /// answered automatically instead of prompting. Toggled by the internal YOLO switch.
    yolo: bool,
    /// Permission request ids already auto-approved under yolo, so each is replied
    /// to exactly once and never flashes on screen.
    yolo_replied: std::collections::HashSet<String>,
}

impl Runtime {
    fn new(input: RunTuiInput) -> Result<Self, AppRunError> {
        let theme = resolve(
            &builtin_theme(DEFAULT_THEME)?.ok_or(ThemeError::MissingReference {
                name: DEFAULT_THEME.to_owned(),
            })?,
            Mode::Dark,
        )?;
        let active_agent = input.default_agent.clone();
        let active_model = input.default_model.clone();
        let theme_names: Vec<String> = builtin_themes()
            .map(|themes| themes.keys().map(|name| (*name).to_owned()).collect())
            .unwrap_or_default();
        Ok(Self {
            input,
            prompt: PromptDoc::default(),
            submitted_prompts: Vec::new(),
            pending_prompts: Vec::new(),
            backend_ready: false,
            history_index: None,
            command_names: Vec::new(),
            model_names: Vec::new(),
            model_limits: std::collections::HashMap::new(),
            model_variants: std::collections::HashMap::new(),
            active_variant: None,
            mcp_status: Vec::new(),
            lsp_status: Vec::new(),
            formatter_status: Vec::new(),
            plugin_status: Vec::new(),
            status_dialog_open: false,
            session_entries: Vec::new(),
            timeline_entries: Vec::new(),
            theme_names,
            dialog: None,
            prompt_dialog: None,
            confirm_dialog: None,
            session_dialog_pending: false,
            timeline_dialog_pending: false,
            active_agent,
            active_model,
            agent_models: std::collections::HashMap::new(),
            leader_pending: false,
            toast: None,
            tip: screens::home::random_tip(),
            placeholder: screens::home::random_placeholder(),
            started: Instant::now(),
            animating: false,
            show_timestamps: false,
            sidebar_visible: true,
            show_tips: true,
            subagent_nav: false,
            dispatcher: default_dispatcher()?,
            theme,
            theme_name: DEFAULT_THEME.to_owned(),
            theme_mode: Mode::Dark,
            scroll: ScrollState::default(),
            permission_selected: 0,
            permission_stage: screens::permission::Stage::Permission,
            permission_reject_message: String::new(),
            active_permission: None,
            question_state: screens::question::QuestionState::default(),
            active_question: None,
            pending_editor: false,
            prompt_hits: crate::screens::prompt_box::PromptHits::default(),
            yolo: false,
            yolo_replied: std::collections::HashSet::new(),
        })
    }

    fn model_label(&self) -> Option<String> {
        let (provider_id, model_id) = self.active_model.as_ref()?;
        let value = format!("{provider_id}/{model_id}");
        let label = self
            .model_names
            .iter()
            .find(|(candidate, _, _)| *candidate == value)
            .map_or_else(|| model_id.clone(), |(_, title, _)| title.clone());
        Some(label)
    }

    fn model_label_display(&self) -> Option<String> {
        let base = self.model_label()?;
        Some(match &self.active_variant {
            Some(variant) => format!("{base} [{variant}]"),
            None => base,
        })
    }

    fn provider_label(&self) -> Option<String> {
        let (provider_id, model_id) = self.active_model.as_ref()?;
        let value = format!("{provider_id}/{model_id}");
        self.model_names
            .iter()
            .find(|(candidate, _, _)| *candidate == value)
            .map(|(_, _, provider)| provider.clone())
    }

    fn model_context_limit(&self) -> Option<i64> {
        let (provider_id, model_id) = self.active_model.as_ref()?;
        let value = format!("{provider_id}/{model_id}");
        self.model_limits
            .get(&value)
            .copied()
            .filter(|limit| *limit > 0)
    }

    fn active_model_value(&self) -> Option<String> {
        let (provider_id, model_id) = self.active_model.as_ref()?;
        Some(format!("{provider_id}/{model_id}"))
    }

    fn active_model_variants(&self) -> Vec<String> {
        self.active_model_value()
            .and_then(|value| self.model_variants.get(&value).cloned())
            .unwrap_or_default()
    }

    fn cycle_variant(&mut self) {
        let variants = self.active_model_variants();
        if variants.is_empty() {
            self.toast = Some((
                "No variants for this model".to_owned(),
                Instant::now() + TOAST_DURATION,
            ));
            return;
        }
        self.active_variant = match self.active_variant.as_deref() {
            None => Some(variants[0].clone()),
            Some(current) => match variants.iter().position(|name| name == current) {
                Some(index) if index + 1 < variants.len() => Some(variants[index + 1].clone()),
                _ => None,
            },
        };
        let label = self
            .active_variant
            .clone()
            .unwrap_or_else(|| "Default".to_owned());
        self.toast = Some((format!("Variant: {label}"), Instant::now() + TOAST_DURATION));
    }

    fn open_variant_dialog(&mut self) {
        if self.active_model_variants().is_empty() {
            self.toast = Some((
                "No variants for this model".to_owned(),
                Instant::now() + TOAST_DURATION,
            ));
            return;
        }
        self.open_dialog(DialogKind::VariantList);
    }

    fn mcp_summary(&self) -> Option<(usize, usize, bool)> {
        if self.mcp_status.is_empty() {
            return None;
        }
        let total = self.mcp_status.len();
        let connected = self
            .mcp_status
            .iter()
            .filter(|(_, status)| status == "connected")
            .count();
        let has_error = self.mcp_status.iter().any(|(_, status)| status == "failed");
        Some((connected, total, has_error))
    }

    async fn draw(&mut self) -> Result<(), AppRunError> {
        let route = self.input.state.route.clone();
        let data = self.input.state.data.read().await;
        let dialog = self.dialog.as_ref();
        let whichkey = if self.leader_pending {
            self.dispatcher.continuations()
        } else {
            Vec::new()
        };
        let toast = self
            .toast
            .as_ref()
            .filter(|(_, deadline)| Instant::now() < *deadline)
            .map(|(message, _)| message.clone());
        let active_agent = self.active_agent.as_deref();
        let model_label = self.model_label_display();
        let provider_label = self.provider_label();
        let context_limit = self.model_context_limit();
        let mcp = self.mcp_summary();
        let spinner = crate::widgets::spinner::frame(self.started.elapsed());
        let working = match &route {
            Route::Session { session_id, .. } => data.is_working(session_id),
            _ => false,
        };
        let subagent = match &route {
            Route::Session { session_id, .. } => {
                screens::session::subagent_status(&data, session_id)
            }
            _ => None,
        };
        let subagent_nav = matches!(
            subagent,
            Some(screens::session::SubagentStatus::Child { .. })
        );
        let on_home = matches!(&route, Route::Home { .. } | Route::Plugin { .. });
        let logo_elapsed = self.started.elapsed();
        let mut active_permission_data: Option<(serde_json::Value, serde_json::Value)> =
            match &route {
                Route::Session { session_id, .. } => {
                    data.permissions(session_id).first().map(|request| {
                        (
                            request.clone(),
                            screens::permission::tool_input(&data, request),
                        )
                    })
                }
                _ => None,
            };
        let permission_identity = active_permission_data.as_ref().and_then(|(request, _)| {
            let session = request
                .get("sessionID")
                .and_then(serde_json::Value::as_str)?
                .to_owned();
            let id = request
                .get("id")
                .and_then(serde_json::Value::as_str)?
                .to_owned();
            let is_subagent = data
                .session(&session)
                .and_then(|info| info.parent_id.as_ref())
                .is_some();
            Some((session, id, is_subagent))
        });
        if permission_identity.as_ref().map(|(_, id, _)| id.as_str())
            != self
                .active_permission
                .as_ref()
                .map(|(_, id, _)| id.as_str())
        {
            self.permission_selected = 0;
            self.permission_stage = screens::permission::Stage::Permission;
            self.permission_reject_message.clear();
        }
        self.active_permission = permission_identity;
        if self.yolo {
            // Auto-approve EVERY pending permission, not just the active session's: a subagent
            // runs in a child session whose requests are never the active permission, so without
            // this the subagent blocks forever on an approval the TUI would never surface.
            let pending_ids: Vec<String> = data
                .permissions
                .values()
                .flatten()
                .filter_map(|request| {
                    request
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .collect();
            for id in pending_ids {
                if self.yolo_replied.insert(id.clone()) {
                    let client = Arc::clone(&self.input.client);
                    let tx = self.input.tx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = client.permission_reply(&id, "once", None).await {
                            let _ = tx.send(AppEvent::Toast(format!(
                                "yolo auto-approve failed: {error}"
                            )));
                        }
                    });
                }
            }
        }
        if let Some((_, request_id, _)) = self.active_permission.clone() {
            if self.yolo_replied.contains(&request_id) {
                active_permission_data = None;
                self.active_permission = None;
            }
        }
        let active_question_data: Option<serde_json::Value> = match &route {
            Route::Session { session_id, .. } if active_permission_data.is_none() => {
                data.questions(session_id).first().cloned()
            }
            _ => None,
        };
        let active_question = active_question_data.as_ref().and_then(|request| {
            let session_id = request
                .get("sessionID")
                .and_then(serde_json::Value::as_str)?;
            let request_id = request
                .get("id")
                .and_then(serde_json::Value::as_str)?
                .to_owned();
            let directory = data
                .session(session_id)
                .and_then(|session| session.directory.clone());
            Some(ActiveQuestion {
                request_id,
                directory,
                request: request.clone(),
            })
        });
        if let Some(active) = &active_question {
            self.question_state.sync(&active.request_id);
        }
        self.active_question = active_question;
        let permission_selected = self.permission_selected;
        let permission_stage = self.permission_stage;
        let permission_reject_message = self.permission_reject_message.clone();
        let question_state = self.question_state.clone();
        let show_timestamps = self.show_timestamps;
        let sidebar_visible = self.sidebar_visible;
        let agents = self.input.agent_names.as_slice();
        let model_names = self.model_names.as_slice();
        let status_dialog_open = self.status_dialog_open;
        let yolo = self.yolo;
        let mcp_snapshot = self.mcp_status.as_slice();
        let lsp_snapshot = self.lsp_status.as_slice();
        let formatter_snapshot = self.formatter_status.as_slice();
        let plugin_snapshot = self.plugin_status.as_slice();
        let show_cursor = dialog.is_none()
            && active_permission_data.is_none()
            && active_question_data.is_none()
            && self.prompt_dialog.is_none()
            && self.confirm_dialog.is_none()
            && !status_dialog_open
            && !self.leader_pending;
        let mut prompt_hits = screens::prompt_box::PromptHits::default();
        self.input.tui.terminal_mut().draw(|frame| {
            prompt_hits = match route {
                Route::Home { .. } => screens::home::draw(
                    frame,
                    &screens::home::HomeView {
                        doc: &self.prompt,
                        agents,
                        active_agent,
                        model_label: model_label.as_deref(),
                        provider_label: provider_label.as_deref(),
                        tip: if self.backend_ready {
                            self.show_tips.then_some(self.tip)
                        } else {
                            Some(CONNECTING_TIP)
                        },
                        placeholder: if self.backend_ready {
                            self.placeholder
                        } else {
                            CONNECTING_PLACEHOLDER
                        },
                        mcp,
                        logo_elapsed,
                        show_cursor,
                        yolo,
                    },
                    &self.theme,
                ),
                Route::Session { session_id, .. } => {
                    let view = screens::session::SessionView {
                        store: &data,
                        session_id: &session_id,
                        pending: &self.submitted_prompts,
                        prompt: &self.prompt,
                        agents,
                        model_names,
                        active_agent,
                        model_label: model_label.as_deref(),
                        provider_label: provider_label.as_deref(),
                        context_limit,
                        spinner,
                        show_timestamps,
                        sidebar_visible,
                        subagent,
                        show_cursor,
                        yolo,
                    };
                    screens::session::draw(frame, &view, &mut self.scroll, &self.theme)
                }
                Route::Plugin { .. } => screens::home::draw(
                    frame,
                    &screens::home::HomeView {
                        doc: &self.prompt,
                        agents,
                        active_agent,
                        model_label: model_label.as_deref(),
                        provider_label: provider_label.as_deref(),
                        tip: if self.backend_ready {
                            self.show_tips.then_some(self.tip)
                        } else {
                            Some(CONNECTING_TIP)
                        },
                        placeholder: if self.backend_ready {
                            self.placeholder
                        } else {
                            CONNECTING_PLACEHOLDER
                        },
                        mcp,
                        logo_elapsed,
                        show_cursor,
                        yolo,
                    },
                    &self.theme,
                ),
            };
            if let Some(dialog) = dialog {
                screens::palette::draw(frame, &dialog.select, dialog.kind.title(), &self.theme);
            }
            if !whichkey.is_empty() {
                screens::whichkey::draw(frame, &whichkey, &self.theme);
            }
            if let Some(toast) = &toast {
                screens::toast::draw(frame, toast, &self.theme);
            }
            if let Some((request, input)) = &active_permission_data {
                screens::permission::draw(
                    frame,
                    request,
                    input,
                    permission_stage,
                    permission_selected,
                    &permission_reject_message,
                    &self.theme,
                );
            }
            if let Some(dialog) = &self.prompt_dialog {
                screens::input_dialog::draw_prompt(frame, dialog.title, &dialog.input, &self.theme);
            }
            if let Some(dialog) = &self.confirm_dialog {
                screens::input_dialog::draw_confirm(
                    frame,
                    dialog.title,
                    &dialog.message,
                    dialog.selected,
                    &self.theme,
                );
            }
            if status_dialog_open {
                screens::status::draw(
                    frame,
                    &screens::status::StatusView {
                        mcp: mcp_snapshot,
                        lsp: lsp_snapshot,
                        formatters: formatter_snapshot,
                        plugins: plugin_snapshot,
                    },
                    &self.theme,
                );
            }
            if let Some(request) = &active_question_data {
                screens::question::draw(frame, request, &question_state, &self.theme);
            }
        })?;
        self.prompt_hits = prompt_hits;
        drop(data);
        self.animating = working || on_home;
        self.subagent_nav = subagent_nav;
        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<bool, AppRunError> {
        match event {
            AppEvent::Key(key) => Ok(self.handle_key(key)),
            AppEvent::Paste(text) => {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                self.prompt.insert_str(&normalized);
                self.history_index = None;
                Ok(false)
            }
            AppEvent::Navigate(session_id) => {
                self.input.state.navigate(Route::Session {
                    session_id,
                    prompt: None,
                });
                Ok(false)
            }
            AppEvent::LoadSession(session_id) => {
                self.load_session(session_id);
                Ok(false)
            }
            AppEvent::FileMatches(matches) => {
                self.complete_mention(matches.first().map(String::as_str));
                Ok(false)
            }
            AppEvent::CommandList(names) => {
                self.command_names = names;
                Ok(false)
            }
            AppEvent::ModelList(models) => {
                self.model_limits = models
                    .iter()
                    .map(|(value, _, _, limit, _)| (value.clone(), *limit))
                    .collect();
                self.model_variants = models
                    .iter()
                    .map(|(value, _, _, _, variants)| (value.clone(), variants.clone()))
                    .collect();
                self.model_names = models
                    .into_iter()
                    .map(|(value, title, provider, _, _)| (value, title, provider))
                    .collect();
                Ok(false)
            }
            AppEvent::SessionList(entries) => {
                self.session_entries = entries;
                if self.session_dialog_pending {
                    self.session_dialog_pending = false;
                    self.open_dialog(DialogKind::SessionList);
                }
                Ok(false)
            }
            AppEvent::TimelineList(entries) => {
                self.timeline_entries = entries;
                if self.timeline_dialog_pending {
                    self.timeline_dialog_pending = false;
                    self.open_dialog(DialogKind::Timeline);
                }
                Ok(false)
            }
            AppEvent::McpStatus(status) => {
                self.mcp_status = status;
                Ok(false)
            }
            AppEvent::LspStatus(status) => {
                self.lsp_status = status;
                Ok(false)
            }
            AppEvent::FormatterStatus(status) => {
                self.formatter_status = status;
                Ok(false)
            }
            AppEvent::PluginList(plugins) => {
                self.plugin_status = plugins;
                Ok(false)
            }
            AppEvent::CopyToClipboard(text) => {
                self.toast = Some((
                    if write_osc52_clipboard(&text).is_ok() {
                        "Message copied to clipboard!"
                    } else {
                        "Failed to copy to clipboard"
                    }
                    .to_owned(),
                    Instant::now() + TOAST_DURATION,
                ));
                Ok(false)
            }
            AppEvent::CopySessionTranscriptToClipboard(text) => {
                self.toast = Some((
                    if write_osc52_clipboard(&text).is_ok() {
                        "Session transcript copied to clipboard!"
                    } else {
                        "Failed to copy session transcript"
                    }
                    .to_owned(),
                    Instant::now() + TOAST_DURATION,
                ));
                Ok(false)
            }
            AppEvent::Toast(message) => {
                self.toast = Some((message, Instant::now() + TOAST_DURATION));
                Ok(false)
            }
            AppEvent::AgentList(agents, default_agent) => {
                self.input.agent_names = agents.iter().map(|(name, _)| name.clone()).collect();
                self.agent_models = agents
                    .into_iter()
                    .filter_map(|(name, model)| model.map(|spec| (name, spec)))
                    .collect();
                if self.active_agent.is_none() {
                    self.active_agent = default_agent;
                }
                if self.active_model.is_none() {
                    self.active_model = self
                        .active_agent
                        .as_ref()
                        .and_then(|agent| self.agent_models.get(agent).cloned());
                }
                Ok(false)
            }
            AppEvent::BackendReady => {
                self.backend_ready = true;
                let failures: Vec<String> = self
                    .mcp_status
                    .iter()
                    .filter(|(_, status)| status != "connected" && status != "disabled")
                    .map(|(name, status)| format!("{name} ({status})"))
                    .collect();
                let message = if failures.is_empty() {
                    "Backend ready".to_owned()
                } else {
                    format!(
                        "Backend ready \u{2014} MCP unavailable: {}",
                        failures.join(", ")
                    )
                };
                self.toast = Some((message, Instant::now() + TOAST_DURATION));
                self.drain_pending_prompts();
                Ok(false)
            }
            AppEvent::Mouse { column, row, kind } => {
                match kind {
                    MouseKind::ScrollUp | MouseKind::ScrollDown => {
                        let delta = if kind == MouseKind::ScrollUp { -3 } else { 3 };
                        if let Some(dialog) = self.dialog.as_mut() {
                            if delta < 0 {
                                dialog.select.move_up();
                            } else {
                                dialog.select.move_down();
                            }
                        } else {
                            self.scroll.scroll_by(delta);
                        }
                    }
                    MouseKind::Press => self.handle_mouse_press(column, row),
                    MouseKind::Other => {}
                }
                Ok(false)
            }
            AppEvent::Resize(_, _) | AppEvent::Tick | AppEvent::Internal(_) => Ok(false),
            AppEvent::Sse(event) => {
                super::apply_sse(&mut self.input.state, &event).await;
                Ok(false)
            }
            AppEvent::Quit => Ok(true),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.active_permission.is_some() {
            self.handle_permission_key(key);
            return false;
        }
        if self.active_question.is_some() {
            self.handle_question_key(key);
            return false;
        }
        if self.prompt_dialog.is_some() {
            self.handle_prompt_dialog_key(key);
            return false;
        }
        if self.confirm_dialog.is_some() {
            self.handle_confirm_dialog_key(key);
            return false;
        }
        if self.dialog.is_some() {
            self.handle_dialog_key(key);
            return false;
        }
        if self.status_dialog_open {
            if matches!(key.key, Key::Esc) {
                self.status_dialog_open = false;
            }
            return false;
        }
        if self.leader_pending {
            return self.dispatch_key(key);
        }
        // The focused prompt intercepts editing/submit/exit keys before the global keymap
        // (managed-textarea precedence): several commands share these keys in base mode
        // (Enter is also diff.toggle, ctrl+c is also input.clear), so the prompt must win.
        // While a leader chord is mid-sequence (above), the dispatcher wins instead.
        if let Some(quit) = self.handle_prompt_key(key) {
            return quit;
        }
        self.dispatch_key(key)
    }

    fn dispatch_key(&mut self, key: KeyEvent) -> bool {
        let outcome = self.dispatcher.dispatch(key, KeymapMode::Base);
        self.leader_pending = matches!(outcome, DispatchOutcome::Pending);
        if let DispatchOutcome::Matched(command) = outcome {
            return self.handle_command(command.0.as_str());
        }
        false
    }

    fn clear_leader(&mut self) {
        self.leader_pending = false;
        self.dispatcher.clear_pending();
    }

    fn handle_permission_key(&mut self, key: KeyEvent) {
        use screens::permission::Stage;
        if self.permission_stage == Stage::Reject {
            self.handle_permission_reject(key);
            return;
        }
        let options: &[(&str, &str)] = if self.permission_stage == Stage::Always {
            &screens::permission::ALWAYS_OPTIONS
        } else {
            &screens::permission::OPTIONS
        };
        let count = options.len();
        match key.key {
            Key::Left | Key::Char('h') => {
                self.permission_selected = (self.permission_selected + count - 1) % count;
            }
            Key::Right | Key::Char('l') => {
                self.permission_selected = (self.permission_selected + 1) % count;
            }
            Key::Enter => {
                let action = options[self.permission_selected.min(count - 1)].0;
                self.apply_permission_action(action);
            }
            Key::Esc => {
                if self.permission_stage == Stage::Always {
                    self.permission_stage = Stage::Permission;
                    self.permission_selected = 0;
                } else {
                    self.begin_reject();
                }
            }
            _ => {}
        }
    }

    fn apply_permission_action(&mut self, action: &str) {
        use screens::permission::Stage;
        match action {
            "once" => self.reply_permission("once", None),
            "always" => {
                self.permission_stage = Stage::Always;
                self.permission_selected = 0;
            }
            "reject" => self.begin_reject(),
            "confirm" => self.reply_permission("always", None),
            "cancel" => {
                self.permission_stage = Stage::Permission;
                self.permission_selected = 0;
            }
            _ => {}
        }
    }

    fn begin_reject(&mut self) {
        let is_subagent = self
            .active_permission
            .as_ref()
            .is_some_and(|(_, _, subagent)| *subagent);
        if is_subagent {
            self.permission_stage = screens::permission::Stage::Reject;
            self.permission_selected = 0;
            self.permission_reject_message.clear();
        } else {
            self.reply_permission("reject", None);
        }
    }

    fn handle_permission_reject(&mut self, key: KeyEvent) {
        let no_mods = !key.ctrl && !key.alt && !key.meta;
        match key.key {
            Key::Char(ch) if no_mods => self.permission_reject_message.push(ch),
            Key::Backspace => {
                self.permission_reject_message.pop();
            }
            Key::Enter => {
                let message = self.permission_reject_message.trim().to_owned();
                let message = (!message.is_empty()).then_some(message);
                self.reply_permission("reject", message);
            }
            Key::Esc => {
                self.permission_stage = screens::permission::Stage::Permission;
                self.permission_selected = 0;
            }
            _ => {}
        }
    }

    fn reply_permission(&mut self, reply: &str, message: Option<String>) {
        let Some((_, request_id, _)) = self.active_permission.clone() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        let reply = reply.to_owned();
        tokio::spawn(async move {
            if let Err(error) = client
                .permission_reply(&request_id, &reply, message.as_deref())
                .await
            {
                let _ = tx.send(AppEvent::Toast(format!("permission reply failed: {error}")));
            }
        });
        self.permission_selected = 0;
        self.permission_stage = screens::permission::Stage::Permission;
        self.permission_reject_message.clear();
    }

    fn handle_question_key(&mut self, key: KeyEvent) {
        let Some(active) = self.active_question.clone() else {
            return;
        };
        let no_mods = !key.ctrl && !key.alt && !key.meta;
        if self.question_state.editing() {
            match key.key {
                Key::Char(ch) if no_mods => self.question_state.push_custom_char(ch),
                Key::Backspace if no_mods => self.question_state.pop_custom_char(),
                Key::Enter if no_mods => {
                    let action = self.question_state.save_custom(&active.request);
                    self.apply_question_action(action);
                }
                Key::Esc if no_mods => self.question_state.cancel_edit(),
                _ => {}
            }
            return;
        }
        match key.key {
            Key::Left | Key::Char('h') if no_mods => {
                self.question_state.previous_tab(&active.request)
            }
            Key::Right | Key::Char('l') | Key::Tab if no_mods => {
                self.question_state.next_tab(&active.request);
            }
            Key::BackTab if no_mods => self.question_state.previous_tab(&active.request),
            Key::Up | Key::Char('k') if no_mods => {
                self.question_state.move_selected(&active.request, -1)
            }
            Key::Down | Key::Char('j') if no_mods => {
                self.question_state.move_selected(&active.request, 1)
            }
            Key::Char(ch) if no_mods && ('1'..='9').contains(&ch) => {
                let index = usize::from(ch as u8 - b'1');
                if index < self.question_state.selectable_count(&active.request) {
                    self.question_state.set_selected(index);
                    let action = self.question_state.select(&active.request);
                    self.apply_question_action(action);
                }
            }
            Key::Enter if no_mods && self.question_state.confirm(&active.request) => {
                self.apply_question_action(self.question_state.submit(&active.request));
            }
            Key::Enter if no_mods => {
                let action = self.question_state.select(&active.request);
                self.apply_question_action(action);
            }
            Key::Esc if no_mods => self.reject_question(),
            _ => {}
        }
    }

    fn apply_question_action(&mut self, action: screens::question::QuestionAction) {
        match action {
            screens::question::QuestionAction::None => {}
            screens::question::QuestionAction::Reply { answers } => self.reply_question(answers),
        }
    }

    fn reply_question(&mut self, answers: Vec<Vec<String>>) {
        let Some(active) = self.active_question.clone() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            if let Err(error) = client
                .question_reply(&active.request_id, &answers, active.directory.as_deref())
                .await
            {
                let _ = tx.send(AppEvent::Toast(format!("question reply failed: {error}")));
            }
        });
    }

    fn reject_question(&mut self) {
        let Some(active) = self.active_question.clone() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            if let Err(error) = client
                .question_reject(&active.request_id, active.directory.as_deref())
                .await
            {
                let _ = tx.send(AppEvent::Toast(format!("question reject failed: {error}")));
            }
        });
    }

    fn toast_deadline(&self) -> Option<Instant> {
        self.toast.as_ref().map(|(_, deadline)| *deadline)
    }

    fn clear_toast(&mut self) {
        self.toast = None;
    }

    async fn abort_input_task(&mut self) {
        if let Some(input_task) = self.input.input_task.take() {
            input_task.abort();
            let _ = input_task.await;
        }
    }

    fn respawn_input_task(&mut self) {
        self.input.input_task = Some(spawn_input_task(self.input.tx.clone()));
    }

    async fn open_pending_editor(&mut self) -> Result<(), AppRunError> {
        if !self.pending_editor {
            return Ok(());
        }
        self.pending_editor = false;
        let Some(command) = editor_command_from_env() else {
            self.toast = Some(("No $EDITOR set".to_owned(), Instant::now() + TOAST_DURATION));
            return Ok(());
        };

        let prompt_text = self.prompt.text.clone();
        self.abort_input_task().await;
        self.input.tui.suspend()?;
        let editor_result = run_editor_command(&command, &prompt_text);
        let resume_result = self.input.tui.resume();
        self.respawn_input_task();
        resume_result?;

        match editor_result {
            Ok(content) => {
                self.prompt.set_text(normalize_editor_content(&content));
                self.history_index = None;
            }
            Err(error) => {
                self.toast = Some((
                    format!("Editor failed: {error}"),
                    Instant::now() + TOAST_DURATION,
                ));
            }
        }
        Ok(())
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Option<bool> {
        let no_mods = !key.ctrl && !key.alt && !key.meta;
        if key.ctrl && matches!(key.key, Key::Char('c')) {
            if self.prompt.text.is_empty() {
                return Some(true);
            }
            self.reset_prompt();
            return Some(false);
        }
        if key.ctrl && matches!(key.key, Key::Char('d')) {
            if self.prompt.text.is_empty() {
                if matches!(self.input.state.route, Route::Session { .. }) {
                    self.open_delete_dialog();
                    return Some(false);
                }
                return Some(true);
            }
            self.prompt.delete();
            self.history_index = None;
            return Some(false);
        }
        if key.ctrl || key.alt {
            match key.key {
                Key::Char('a') if key.ctrl => self.prompt.move_line_home(),
                Key::Char('e') if key.ctrl => self.prompt.move_line_end(),
                Key::Char('b') if key.ctrl => self.prompt.move_left(),
                Key::Char('f') if key.ctrl => self.prompt.move_right(),
                Key::Char('b') if key.alt => self.prompt.move_word_left(),
                Key::Char('f') if key.alt => self.prompt.move_word_right(),
                Key::Left => self.prompt.move_word_left(),
                Key::Right => self.prompt.move_word_right(),
                Key::Char('k') if key.ctrl => self.edit_prompt(|p| p.delete_to_line_end()),
                Key::Char('u') if key.ctrl => self.edit_prompt(|p| p.delete_to_line_start()),
                Key::Char('w') if key.ctrl => self.edit_prompt(|p| p.delete_word_left()),
                Key::Char('d') if key.alt => self.edit_prompt(|p| p.delete_word_right()),
                Key::Char('j') if key.ctrl => self.insert_char('\n'),
                Key::Backspace => self.edit_prompt(|p| p.delete_word_left()),
                _ => return None,
            }
            return Some(false);
        }
        match key.key {
            Key::Enter if !no_mods => {
                self.insert_char('\n');
                Some(false)
            }
            Key::Enter => {
                if matches!(self.prompt.text.trim(), "exit" | "quit" | ":q")
                    || builtin_quit_command(&self.prompt.text)
                {
                    return Some(true);
                }
                self.submit_prompt();
                Some(false)
            }
            Key::Up if self.subagent_nav && self.prompt.text.is_empty() => {
                self.goto_parent();
                Some(false)
            }
            Key::Left if self.subagent_nav && self.prompt.text.is_empty() => {
                self.cycle_child(-1);
                Some(false)
            }
            Key::Right if self.subagent_nav && self.prompt.text.is_empty() => {
                self.cycle_child(1);
                Some(false)
            }
            Key::Left => {
                self.prompt.move_left();
                Some(false)
            }
            Key::Right => {
                self.prompt.move_right();
                Some(false)
            }
            Key::Up => {
                if !self.prompt.move_up_line() {
                    self.history_prev();
                }
                Some(false)
            }
            Key::Down => {
                if !self.prompt.move_down_line() {
                    self.history_next();
                }
                Some(false)
            }
            Key::Home => {
                self.prompt.move_buffer_home();
                Some(false)
            }
            Key::End => {
                self.prompt.move_buffer_end();
                Some(false)
            }
            Key::Tab => {
                if trailing_mention(&self.prompt.text).is_some() {
                    self.trigger_file_complete();
                } else {
                    self.cycle_agent();
                }
                Some(false)
            }
            Key::Char(ch) => {
                self.insert_char(ch);
                Some(false)
            }
            Key::Backspace => {
                self.prompt.backspace();
                self.history_index = None;
                Some(false)
            }
            Key::Delete => {
                self.prompt.delete();
                self.history_index = None;
                Some(false)
            }
            _ => None,
        }
    }

    fn insert_char(&mut self, ch: char) {
        self.prompt.insert_char(ch);
        self.history_index = None;
    }

    fn edit_prompt(&mut self, edit: impl FnOnce(&mut PromptDoc)) {
        edit(&mut self.prompt);
        self.history_index = None;
    }

    fn reset_prompt(&mut self) {
        self.prompt.clear_input();
        self.history_index = None;
    }

    fn cycle_agent(&mut self) {
        if self.input.agent_names.is_empty() {
            return;
        }
        let next = match self.active_agent.as_deref().and_then(|name| {
            self.input
                .agent_names
                .iter()
                .position(|agent| agent == name)
        }) {
            Some(index) => (index + 1) % self.input.agent_names.len(),
            None => 0,
        };
        let next_agent = self.input.agent_names[next].clone();
        self.set_active_agent(next_agent);
    }

    /// Select `name` as the active agent and adopt that agent's configured model (resetting the
    /// variant), mirroring OpenCode where each agent carries its own model.
    fn set_active_agent(&mut self, name: String) {
        if let Some(model) = self.agent_models.get(&name).cloned() {
            self.active_model = Some(model);
            self.active_variant = None;
        }
        self.active_agent = Some(name);
    }

    fn history_prev(&mut self) {
        if self.submitted_prompts.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => self.submitted_prompts.len() - 1,
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(index);
        self.prompt.set_text(self.submitted_prompts[index].clone());
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.submitted_prompts.len() {
            self.history_index = Some(index + 1);
            self.prompt
                .set_text(self.submitted_prompts[index + 1].clone());
        } else {
            self.history_index = None;
            self.prompt.clear_input();
        }
    }

    fn trigger_file_complete(&mut self) {
        let Some(query) = trailing_mention(&self.prompt.text) else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            if let Ok(matches) = client.find_files(&query).await {
                let _ = tx.send(AppEvent::FileMatches(matches));
            }
        });
    }

    fn complete_mention(&mut self, top: Option<&str>) {
        let Some(top) = top else {
            return;
        };
        let Some(at) = self.prompt.text.rfind('@') else {
            return;
        };
        self.prompt.text.truncate(at + 1);
        self.prompt.text.push_str(top);
        self.prompt.text.push(' ');
        self.prompt.move_buffer_end();
        self.history_index = None;
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        let items = match kind {
            DialogKind::CommandPalette => command_palette_items(
                self.yolo,
                self.theme_mode,
                self.show_timestamps,
                &self.command_names,
            ),
            DialogKind::AgentSwitch => self
                .input
                .agent_names
                .iter()
                .map(|name| DialogSelectItem::new(name.clone(), name.clone()))
                .collect(),
            DialogKind::ModelSwitch => self
                .model_names
                .iter()
                .map(|(value, title, provider)| {
                    DialogSelectItem::new(format!("{title}  {provider}"), value.clone())
                })
                .collect(),
            DialogKind::VariantList => std::iter::once(DialogSelectItem::new(
                "Default".to_owned(),
                "default".to_owned(),
            ))
            .chain(
                self.active_model_variants()
                    .into_iter()
                    .map(|variant| DialogSelectItem::new(variant.clone(), variant)),
            )
            .collect(),
            DialogKind::SessionList => self
                .session_entries
                .iter()
                .map(|(id, title)| DialogSelectItem::new(title.clone(), id.clone()))
                .collect(),
            DialogKind::Timeline => self
                .timeline_entries
                .iter()
                .map(|(id, title, footer)| {
                    DialogSelectItem::new(title.clone(), id.clone()).with_footer(footer.clone())
                })
                .collect(),
            DialogKind::ThemeList => self
                .theme_names
                .iter()
                .map(|name| DialogSelectItem::new(name.clone(), name.clone()))
                .collect(),
            DialogKind::Help => {
                let mut items: Vec<DialogSelectItem<String>> =
                    crate::keymap::default_binding_specs()
                        .iter()
                        .filter(|(key, value, _)| *value != "none" && *key != "leader")
                        .map(|(key, value, description)| {
                            let group = screens::prompt_box::titlecase(
                                key.split(['_', '.']).next().unwrap_or_default(),
                            );
                            let keybind = value.replace("<leader>", "ctrl+x ");
                            DialogSelectItem::new((*description).to_owned(), (*key).to_owned())
                                .with_description(keybind)
                                .with_category(group)
                        })
                        .collect();
                items.sort_by(|left, right| left.category.cmp(&right.category));
                items
            }
        };
        self.dialog = Some(ActiveDialog {
            select: DialogSelect::new(items),
            kind,
        });
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) {
        let no_mods = !key.ctrl && !key.alt && !key.meta;
        match key.key {
            Key::Esc => self.dialog = None,
            Key::Enter if no_mods => {
                let Some(active) = self.dialog.take() else {
                    return;
                };
                if let Some(value) = active.select.select().cloned() {
                    self.apply_dialog_selection(active.kind, value);
                }
            }
            Key::Up if no_mods => {
                if let Some(active) = self.dialog.as_mut() {
                    active.select.move_up();
                }
            }
            Key::Down if no_mods => {
                if let Some(active) = self.dialog.as_mut() {
                    active.select.move_down();
                }
            }
            Key::Backspace if no_mods => {
                if let Some(active) = self.dialog.as_mut() {
                    let mut filter = active.select.filter().to_owned();
                    filter.pop();
                    active.select.set_filter(filter);
                }
            }
            Key::Char(ch) if no_mods => {
                if let Some(active) = self.dialog.as_mut() {
                    let mut filter = active.select.filter().to_owned();
                    filter.push(ch);
                    active.select.set_filter(filter);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse_press(&mut self, column: u16, row: u16) {
        if self.active_permission.is_some() {
            self.handle_permission_click(column, row);
            return;
        }
        if self.active_question.is_some() {
            return;
        }
        if self.prompt_dialog.is_some() {
            self.handle_prompt_dialog_click(column, row);
            return;
        }
        if self.confirm_dialog.is_some() {
            self.handle_confirm_dialog_click(column, row);
            return;
        }
        if self.dialog.is_some() {
            self.handle_dialog_click(column, row);
            return;
        }
        if self.status_dialog_open {
            return;
        }
        self.handle_prompt_box_click(column, row);
    }

    fn handle_prompt_box_click(&mut self, column: u16, row: u16) {
        let hits = self.prompt_hits;
        if hit(hits.agents, column, row) {
            self.open_dialog(DialogKind::AgentSwitch);
        } else if hit(hits.commands, column, row) {
            self.open_dialog(DialogKind::CommandPalette);
        } else if hit(hits.model, column, row) {
            self.open_dialog(DialogKind::ModelSwitch);
        }
    }

    fn handle_permission_click(&mut self, column: u16, row: u16) {
        let Ok(size) = self.input.tui.terminal_mut().size() else {
            return;
        };
        let screen = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let Some(index) =
            screens::permission::permission_button_at(screen, self.permission_stage, column, row)
        else {
            return;
        };
        let options: &[(&str, &str)] = match self.permission_stage {
            screens::permission::Stage::Permission => &screens::permission::OPTIONS,
            screens::permission::Stage::Always => &screens::permission::ALWAYS_OPTIONS,
            screens::permission::Stage::Reject => return,
        };
        self.permission_selected = index;
        self.apply_permission_action(options[index].0);
    }

    fn handle_prompt_dialog_click(&mut self, column: u16, row: u16) {
        let Ok(size) = self.input.tui.terminal_mut().size() else {
            return;
        };
        let screen = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let area = screens::input_dialog::dialog_area(screen, screens::input_dialog::PROMPT_HEIGHT);
        if !rect_contains(area, column, row) {
            self.prompt_dialog = None;
        }
    }

    fn handle_confirm_dialog_click(&mut self, column: u16, row: u16) {
        let Ok(size) = self.input.tui.terminal_mut().size() else {
            return;
        };
        let screen = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        if let Some(index) = screens::input_dialog::confirm_button_at(screen, column, row) {
            if let Some(dialog) = self.confirm_dialog.as_mut() {
                dialog.selected = index;
            }
            if index == 0 {
                self.submit_confirm_dialog();
            } else {
                self.confirm_dialog = None;
            }
            return;
        }
        let area =
            screens::input_dialog::dialog_area(screen, screens::input_dialog::CONFIRM_HEIGHT);
        if !rect_contains(area, column, row) {
            self.confirm_dialog = None;
        }
    }

    fn handle_dialog_click(&mut self, column: u16, row: u16) {
        let Ok(size) = self.input.tui.terminal_mut().size() else {
            return;
        };
        let screen = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let geo = match self.dialog.as_ref() {
            Some(active) => screens::palette::geometry(screen, &active.select),
            None => return,
        };

        let list_area = geo.list_area;
        let in_list = row >= list_area.y
            && row < list_area.y + list_area.height
            && column >= list_area.x
            && column < list_area.x + list_area.width;
        let clicked_item = if in_list {
            let offset = (row - list_area.y) as usize;
            match geo.rows.get(offset).copied() {
                Some(screens::palette::DialogRow::Item(index)) => Some(index),
                _ => None,
            }
        } else {
            None
        };

        if let Some(index) = clicked_item {
            if let Some(active) = self.dialog.as_mut() {
                active.select.set_selected(index);
            }
            if let Some(active) = self.dialog.take() {
                if let Some(value) = active.select.select().cloned() {
                    self.apply_dialog_selection(active.kind, value);
                }
            }
            return;
        }

        let area = geo.area;
        let inside_box = row >= area.y
            && row < area.y + area.height
            && column >= area.x
            && column < area.x + area.width;
        if !inside_box {
            self.dialog = None;
        }
    }

    fn apply_dialog_selection(&mut self, kind: DialogKind, value: String) {
        match kind {
            DialogKind::CommandPalette => {
                if PALETTE_TUI_COMMANDS
                    .iter()
                    .any(|(_, id, _, _, _)| *id == value)
                {
                    self.handle_command(&value);
                } else {
                    self.run_command(value);
                }
            }
            DialogKind::AgentSwitch => self.set_active_agent(value),
            DialogKind::ModelSwitch => {
                if let Some((provider_id, model_id)) = value.split_once('/') {
                    self.active_model = Some((provider_id.to_owned(), model_id.to_owned()));
                    self.active_variant = None;
                }
            }
            DialogKind::VariantList => {
                self.active_variant = (value != "default").then_some(value);
            }
            DialogKind::SessionList => self.load_session(value),
            DialogKind::Timeline => self.scroll_to_timeline_message(&value),
            DialogKind::ThemeList => {
                self.theme_name = value;
                self.apply_theme();
            }
            DialogKind::Help => {}
        }
    }

    fn apply_theme(&mut self) {
        if let Ok(Some(json)) = builtin_theme(&self.theme_name) {
            if let Ok(resolved) = resolve(&json, self.theme_mode) {
                self.theme = resolved;
            }
        }
    }

    fn request_session_list(&mut self) {
        self.session_dialog_pending = true;
        self.refresh_session_list();
    }

    fn request_timeline_list(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        self.timeline_dialog_pending = true;
        let data = Arc::clone(&self.input.state.data);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            let items = {
                let store = data.read().await;
                screens::session::timeline_dialog_items(&store, &session_id)
            };
            let _ = tx.send(AppEvent::TimelineList(items));
        });
    }

    fn refresh_session_list(&self) {
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            if let Ok(sessions) = client.session_list().await {
                let entries = sessions
                    .into_iter()
                    .map(|session| {
                        let title = session
                            .title
                            .filter(|title| !title.trim().is_empty())
                            .unwrap_or_else(|| session.id.clone());
                        (session.id, title)
                    })
                    .collect();
                let _ = tx.send(AppEvent::SessionList(entries));
            }
        });
    }

    fn load_session(&mut self, session_id: String) {
        self.input.state.navigate(Route::Session {
            session_id: session_id.clone(),
            prompt: None,
        });
        self.reset_prompt();
        let client = Arc::clone(&self.input.client);
        let data = Arc::clone(&self.input.state.data);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            if let Ok(session) = client.session_get(&session_id).await {
                data.write()
                    .await
                    .sessions
                    .insert(session.id.clone(), session);
            }
            if let Ok(messages) = client.session_messages(&session_id).await {
                let mut store = data.write().await;
                for (message, parts) in messages {
                    store.upsert_message(message);
                    for part in parts {
                        store.upsert_part(part);
                    }
                }
                drop(store);
            }
            if let Ok(todos) = client.session_todo(&session_id).await {
                data.write().await.todos.insert(session_id.clone(), todos);
            }
            if let Ok(diffs) = client.session_diff(&session_id).await {
                data.write().await.diffs.insert(session_id.clone(), diffs);
            }
            let _ = tx.send(AppEvent::Toast("Session loaded".to_owned()));
        });
    }

    fn enter_first_child(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let data = Arc::clone(&self.input.state.data);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            let target = {
                let store = data.read().await;
                let root = store
                    .session(&session_id)
                    .and_then(|session| session.parent_id.clone())
                    .unwrap_or_else(|| session_id.clone());
                store
                    .child_sessions(&root)
                    .first()
                    .map(|session| session.id.clone())
            };
            match target {
                Some(child) => {
                    let _ = tx.send(AppEvent::LoadSession(child));
                }
                None => {
                    let _ = tx.send(AppEvent::Toast("No subagent sessions".to_owned()));
                }
            }
        });
    }

    fn goto_parent(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let data = Arc::clone(&self.input.state.data);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            let parent = data
                .read()
                .await
                .session(&session_id)
                .and_then(|session| session.parent_id.clone());
            if let Some(parent) = parent {
                let _ = tx.send(AppEvent::LoadSession(parent));
            }
        });
    }

    fn cycle_child(&mut self, direction: i32) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let data = Arc::clone(&self.input.state.data);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            let target = {
                let store = data.read().await;
                let Some(parent) = store
                    .session(&session_id)
                    .and_then(|session| session.parent_id.clone())
                else {
                    return;
                };
                let siblings: Vec<String> = store
                    .child_sessions(&parent)
                    .into_iter()
                    .map(|session| session.id.clone())
                    .collect();
                if siblings.len() <= 1 {
                    None
                } else {
                    let current = siblings
                        .iter()
                        .position(|id| *id == session_id)
                        .unwrap_or(0);
                    let len = siblings.len() as i32;
                    let next = (current as i32 + direction).rem_euclid(len) as usize;
                    Some(siblings[next].clone())
                }
            };
            if let Some(next) = target {
                let _ = tx.send(AppEvent::LoadSession(next));
            }
        });
    }

    fn show_queued_prompts(&mut self) {
        let message = if self.submitted_prompts.is_empty() {
            "No queued prompts".to_owned()
        } else {
            format!("Submitted prompts: {}", self.submitted_prompts.join(" | "))
        };
        self.toast = Some((message, Instant::now() + TOAST_DURATION));
    }

    fn toggle_contextual(&mut self) {
        self.show_tips = !self.show_tips;
    }

    fn run_command(&mut self, name: String) {
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        let agent = self.active_agent.clone();
        let model = self.active_model.clone();
        let existing = match &self.input.state.route {
            Route::Session { session_id, .. } => Some(session_id.clone()),
            _ => None,
        };
        tokio::spawn(async move {
            let session_id = match existing {
                Some(id) => id,
                None => match client.session_create().await {
                    Ok(session) => {
                        let _ = tx.send(AppEvent::Navigate(session.id.clone()));
                        session.id
                    }
                    Err(_) => return,
                },
            };
            let mut body = json!({ "command": name, "arguments": "" });
            if let Some(agent) = agent {
                body["agent"] = json!(agent);
            }
            if let Some((provider_id, model_id)) = model {
                body["model"] = json!(format!("{provider_id}/{model_id}"));
            }
            let _ = client.session_command(&session_id, body).await;
        });
    }

    fn handle_command(&mut self, command: &str) -> bool {
        match command {
            COMMAND_PAGE_UP => self.scroll.page_up(),
            COMMAND_PAGE_DOWN => self.scroll.page_down(),
            COMMAND_LINE_UP => self.scroll.scroll_by(-1),
            COMMAND_LINE_DOWN => self.scroll.scroll_by(1),
            COMMAND_HALF_UP => self.scroll.half_page_up(),
            COMMAND_HALF_DOWN => self.scroll.half_page_down(),
            COMMAND_FIRST => self.scroll.to_top(),
            COMMAND_LAST => self.scroll.to_bottom(),
            COMMAND_SESSION_NEW => {
                self.input.state.navigate(Route::default());
                self.reset_prompt();
            }
            COMMAND_COMMAND_PALETTE => self.open_dialog(DialogKind::CommandPalette),
            COMMAND_AGENT_LIST => self.open_dialog(DialogKind::AgentSwitch),
            COMMAND_MODEL_LIST => self.open_dialog(DialogKind::ModelSwitch),
            COMMAND_YOLO_SWITCH => {
                let message = toggle_yolo(&mut self.yolo).to_owned();
                self.toast = Some((message, Instant::now() + TOAST_DURATION));
            }
            COMMAND_VARIANT_LIST => self.open_variant_dialog(),
            COMMAND_VARIANT_CYCLE => self.cycle_variant(),
            COMMAND_SESSION_LIST => self.request_session_list(),
            COMMAND_SESSION_TIMELINE => self.request_timeline_list(),
            COMMAND_THEME_LIST => self.open_dialog(DialogKind::ThemeList),
            COMMAND_SESSION_RENAME => self.open_rename_dialog(),
            COMMAND_SESSION_DELETE => self.open_delete_dialog(),
            COMMAND_SESSION_COMPACT => self.spawn_session_compact(),
            COMMAND_HELP => self.open_dialog(DialogKind::Help),
            COMMAND_THEME_MODE => {
                self.theme_mode = match self.theme_mode {
                    Mode::Dark => Mode::Light,
                    Mode::Light => Mode::Dark,
                };
                self.apply_theme();
            }
            COMMAND_TOGGLE_TIMESTAMPS => self.show_timestamps = !self.show_timestamps,
            COMMAND_MESSAGES_COPY => self.spawn_messages_copy(),
            COMMAND_SESSION_COPY => self.spawn_session_copy(),
            COMMAND_SESSION_EXPORT => self.spawn_session_export(),
            COMMAND_SESSION_UNDO => self.spawn_session_undo(),
            COMMAND_SESSION_REDO => self.spawn_session_redo(),
            COMMAND_SIDEBAR_TOGGLE => self.sidebar_visible = !self.sidebar_visible,
            COMMAND_SESSION_INTERRUPT => self.spawn_session_abort(),
            COMMAND_HYA_STATUS => self.status_dialog_open = true,
            COMMAND_PROMPT_EDITOR => self.pending_editor = true,
            COMMAND_SESSION_CHILD_FIRST => self.enter_first_child(),
            COMMAND_SESSION_PARENT => self.goto_parent(),
            COMMAND_SESSION_CHILD_NEXT => self.cycle_child(1),
            COMMAND_SESSION_CHILD_PREVIOUS => self.cycle_child(-1),
            COMMAND_SESSION_QUEUED_PROMPTS => self.show_queued_prompts(),
            COMMAND_TIPS_TOGGLE | COMMAND_TOGGLE_CONCEAL => self.toggle_contextual(),
            other => {
                if let Some(slot) = other.strip_prefix("session.quick_switch.") {
                    if let Ok(slot) = slot.parse::<usize>() {
                        self.quick_switch(slot);
                    }
                }
            }
        }
        false
    }

    fn spawn_messages_copy(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let tx = self.input.tx.clone();
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            let event = {
                let store = data.read().await;
                match screens::session::last_assistant_message_text_status(&store, &session_id) {
                    screens::session::LastAssistantMessageText::Text(text) => {
                        AppEvent::CopyToClipboard(text)
                    }
                    screens::session::LastAssistantMessageText::NoAssistantMessage => {
                        AppEvent::Toast("No assistant messages found".to_owned())
                    }
                    screens::session::LastAssistantMessageText::NoTextParts => {
                        AppEvent::Toast("No text parts found in last assistant message".to_owned())
                    }
                    screens::session::LastAssistantMessageText::EmptyText => AppEvent::Toast(
                        "No text content found in last assistant message".to_owned(),
                    ),
                }
            };
            let _ = tx.send(event);
        });
    }

    fn spawn_session_copy(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let tx = self.input.tx.clone();
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            let event = {
                let store = data.read().await;
                match format_store_transcript(&store, &session_id, TranscriptOptions::default()) {
                    Some(transcript) => AppEvent::CopySessionTranscriptToClipboard(transcript),
                    None => AppEvent::Toast("Failed to copy session transcript".to_owned()),
                }
            };
            let _ = tx.send(event);
        });
    }

    fn spawn_session_export(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let tx = self.input.tx.clone();
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            let transcript = {
                let store = data.read().await;
                format_store_transcript(&store, &session_id, TranscriptOptions::default())
            };
            let Some(transcript) = transcript else {
                let _ = tx.send(AppEvent::Toast(
                    "Failed to export session transcript".to_owned(),
                ));
                return;
            };
            let path = match std::env::current_dir() {
                Ok(cwd) => cwd.join(format!(
                    "session-{}.md",
                    session_id.chars().take(8).collect::<String>()
                )),
                Err(_) => {
                    let _ = tx.send(AppEvent::Toast(
                        "Failed to export session transcript".to_owned(),
                    ));
                    return;
                }
            };
            let message = match std::fs::write(&path, transcript) {
                Ok(()) => format!("Exported to {}", path.display()),
                Err(_) => "Failed to export session transcript".to_owned(),
            };
            let _ = tx.send(AppEvent::Toast(message));
        });
    }

    fn current_session_id(&self) -> Option<String> {
        match &self.input.state.route {
            Route::Session { session_id, .. } => Some(session_id.clone()),
            _ => None,
        }
    }

    fn scroll_to_timeline_message(&mut self, message_id: &str) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let Ok(size) = self.input.tui.terminal_mut().size() else {
            return;
        };
        let width = if size.width > 120 && self.sidebar_visible {
            size.width.saturating_sub(screens::session::SIDEBAR_WIDTH) as usize
        } else {
            size.width as usize
        };
        let agent_color = screens::prompt_box::agent_color(
            &self.theme,
            &self.input.agent_names,
            self.active_agent.as_deref(),
        );
        let spinner = crate::widgets::spinner::frame(self.started.elapsed());
        let target = {
            let Ok(store) = self.input.state.data.try_read() else {
                return;
            };
            let rendered = screens::session::timeline_text(
                &store,
                &session_id,
                &self.submitted_prompts,
                width,
                agent_color,
                &self.input.agent_names,
                &self.model_names,
                spinner,
                self.show_timestamps,
                &self.theme,
            );
            let height = rendered.text.0.len();
            rendered
                .message_offsets
                .into_iter()
                .find_map(|(id, offset)| (id == message_id).then_some((offset, height)))
        };
        if let Some((offset, height)) = target {
            self.scroll.set_content_height(height);
            self.scroll.scroll_to(offset);
        }
    }

    fn quick_switch(&mut self, slot: usize) {
        let Some(index) = slot.checked_sub(1) else {
            return;
        };
        let Some((session_id, _)) = self.session_entries.get(index) else {
            return;
        };
        let session_id = session_id.clone();
        if self.current_session_id().as_deref() == Some(session_id.as_str()) {
            return;
        }
        self.load_session(session_id);
    }

    fn spawn_session_abort(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            if data.read().await.is_working(&session_id) {
                let _ = client.session_abort(&session_id).await;
            }
        });
    }

    fn spawn_session_compact(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let Some((provider_id, model_id)) = self.active_model.clone() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        tokio::spawn(async move {
            let message = match client
                .session_compact(&session_id, &provider_id, &model_id)
                .await
            {
                Ok(()) => "Compacting session\u{2026}".to_owned(),
                Err(error) => format!("compact failed: {error}"),
            };
            let _ = tx.send(AppEvent::Toast(message));
        });
    }

    fn spawn_session_undo(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            let target = {
                let store = data.read().await;
                let revert = store
                    .session(&session_id)
                    .and_then(|session| session.revert_message_id())
                    .map(str::to_owned);
                store.messages.get(&session_id).and_then(|messages| {
                    messages
                        .iter()
                        .rev()
                        .find(|message| {
                            message.role.as_deref() == Some("user")
                                && match revert.as_deref() {
                                    Some(point) => message.id.as_str() < point,
                                    None => true,
                                }
                        })
                        .map(|message| message.id.clone())
                })
            };
            let Some(message_id) = target else {
                return;
            };
            if let Err(error) = client.session_revert(&session_id, &message_id).await {
                let _ = tx.send(AppEvent::Toast(format!("undo failed: {error}")));
            }
        });
    }

    fn spawn_session_redo(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        let data = Arc::clone(&self.input.state.data);
        tokio::spawn(async move {
            let next = {
                let store = data.read().await;
                let Some(point) = store
                    .session(&session_id)
                    .and_then(|session| session.revert_message_id())
                    .map(str::to_owned)
                else {
                    return;
                };
                store.messages.get(&session_id).and_then(|messages| {
                    messages
                        .iter()
                        .find(|message| {
                            message.role.as_deref() == Some("user")
                                && message.id.as_str() > point.as_str()
                        })
                        .map(|message| message.id.clone())
                })
            };
            let result = match next {
                Some(message_id) => client.session_revert(&session_id, &message_id).await,
                None => client.session_unrevert(&session_id).await,
            };
            if let Err(error) = result {
                let _ = tx.send(AppEvent::Toast(format!("redo failed: {error}")));
            }
        });
    }

    fn open_rename_dialog(&mut self) {
        if let Route::Session { session_id, .. } = &self.input.state.route {
            self.prompt_dialog = Some(PromptDialog {
                action: PromptAction::RenameSession(session_id.clone()),
                title: "Rename session",
                input: String::new(),
            });
        }
    }

    fn open_delete_dialog(&mut self) {
        if let Route::Session { session_id, .. } = &self.input.state.route {
            self.confirm_dialog = Some(ConfirmDialog {
                action: ConfirmAction::DeleteSession(session_id.clone()),
                title: "Delete session",
                message: "Delete this session? This cannot be undone.".to_owned(),
                selected: 0,
            });
        }
    }

    fn handle_prompt_dialog_key(&mut self, key: KeyEvent) {
        let no_mods = !key.ctrl && !key.alt && !key.meta;
        match key.key {
            Key::Char(ch) if no_mods => {
                if let Some(dialog) = self.prompt_dialog.as_mut() {
                    dialog.input.push(ch);
                }
            }
            Key::Backspace => {
                if let Some(dialog) = self.prompt_dialog.as_mut() {
                    dialog.input.pop();
                }
            }
            Key::Enter => self.submit_prompt_dialog(),
            Key::Esc => self.prompt_dialog = None,
            _ => {}
        }
    }

    fn submit_prompt_dialog(&mut self) {
        let Some(dialog) = self.prompt_dialog.take() else {
            return;
        };
        match dialog.action {
            PromptAction::RenameSession(session_id) => {
                let title = dialog.input.trim().to_owned();
                if title.is_empty() {
                    return;
                }
                let client = Arc::clone(&self.input.client);
                let tx = self.input.tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = client.session_rename(&session_id, &title).await {
                        let _ = tx.send(AppEvent::Toast(format!("rename failed: {error}")));
                    }
                });
            }
        }
    }

    fn handle_confirm_dialog_key(&mut self, key: KeyEvent) {
        match key.key {
            Key::Left | Key::Right | Key::Char('h') | Key::Char('l') => {
                if let Some(dialog) = self.confirm_dialog.as_mut() {
                    dialog.selected = 1 - dialog.selected;
                }
            }
            Key::Enter => {
                let confirm = self
                    .confirm_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.selected == 0);
                if confirm {
                    self.submit_confirm_dialog();
                } else {
                    self.confirm_dialog = None;
                }
            }
            Key::Esc => self.confirm_dialog = None,
            _ => {}
        }
    }

    fn submit_confirm_dialog(&mut self) {
        let Some(dialog) = self.confirm_dialog.take() else {
            return;
        };
        match dialog.action {
            ConfirmAction::DeleteSession(session_id) => {
                let client = Arc::clone(&self.input.client);
                let tx = self.input.tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = client.session_delete(&session_id).await {
                        let _ = tx.send(AppEvent::Toast(format!("delete failed: {error}")));
                    }
                });
                self.input.state.navigate(Route::default());
            }
        }
    }

    /// Replay prompts queued before the backend was ready, in the order they were typed.
    fn drain_pending_prompts(&mut self) {
        for text in std::mem::take(&mut self.pending_prompts) {
            self.prompt.set_text(text);
            self.submit_prompt();
        }
    }

    fn submit_prompt(&mut self) {
        let text = self.prompt.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if let Some(client_command) = builtin_client_command(&text) {
            self.reset_prompt();
            let _ = self.handle_command(client_command);
            return;
        }
        if !self.backend_ready {
            self.pending_prompts.push(text);
            let queued = self.pending_prompts.len();
            self.toast = Some((
                format!(
                    "Backend still starting \u{2014} queued prompt ({queued}); it will send once ready"
                ),
                Instant::now() + TOAST_DURATION,
            ));
            self.reset_prompt();
            return;
        }
        let agent = self.active_agent.clone();
        let model = self.active_model.clone();
        let variant = self.active_variant.clone();
        let request = if let Some(command) = text.strip_prefix('!').filter(|_| agent.is_some()) {
            Submit::Shell(command.trim().to_owned())
        } else if let Some((name, arguments)) = slash_command(&text, &self.command_names) {
            Submit::Command { name, arguments }
        } else if let Some(name) = command_like_name(&text) {
            self.toast = Some((
                format!("Unknown command /{name} \u{2014} press / then Tab to list commands"),
                Instant::now() + TOAST_DURATION,
            ));
            self.reset_prompt();
            return;
        } else {
            self.submitted_prompts.push(text.clone());
            Submit::Prompt(prompt_request_body(&self.prompt))
        };
        self.reset_prompt();

        let client = Arc::clone(&self.input.client);
        let tx = self.input.tx.clone();
        let existing = match &self.input.state.route {
            Route::Session { session_id, .. } => Some(session_id.clone()),
            _ => None,
        };
        // Off-loop on purpose: session.prompt/shell block until the turn completes, so an
        // await here would freeze redraws and key handling. Navigate is posted back async.
        tokio::spawn(async move {
            let session_id = match existing {
                Some(id) => id,
                None => match client.session_create().await {
                    Ok(session) => {
                        let _ = tx.send(AppEvent::Navigate(session.id.clone()));
                        session.id
                    }
                    Err(error) => {
                        let _ = tx.send(AppEvent::Toast(format!("session create failed: {error}")));
                        return;
                    }
                },
            };
            let _ = match request {
                Submit::Shell(command) => {
                    let mut body = json!({ "command": command });
                    if let Some(agent) = agent {
                        body["agent"] = json!(agent);
                    }
                    if let Some((provider_id, model_id)) = &model {
                        body["model"] = model_spec(provider_id, model_id, variant.as_deref());
                    }
                    client.session_shell(&session_id, body).await
                }
                Submit::Command { name, arguments } => {
                    let mut body = json!({ "command": name, "arguments": arguments });
                    if let Some(agent) = agent {
                        body["agent"] = json!(agent);
                    }
                    if let Some((provider_id, model_id)) = &model {
                        body["model"] = json!(format!("{provider_id}/{model_id}"));
                    }
                    client.session_command(&session_id, body).await
                }
                Submit::Prompt(mut body) => {
                    if let Some(agent) = agent {
                        body["agent"] = json!(agent);
                    }
                    if let Some((provider_id, model_id)) = &model {
                        body["model"] = model_spec(provider_id, model_id, variant.as_deref());
                    }
                    client.session_prompt(&session_id, body).await
                }
            };
        });
    }
}

enum Submit {
    Shell(String),
    Command { name: String, arguments: String },
    Prompt(serde_json::Value),
}

fn write_osc52_clipboard(text: &str) -> io::Result<()> {
    let encoded = base64_encode(text.as_bytes());
    let mut stdout = io::stdout().lock();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let indexes = [
            usize::from(first >> 2),
            usize::from(((first & 0b0000_0011) << 4) | (second >> 4)),
            usize::from(((second & 0b0000_1111) << 2) | (third >> 6)),
            usize::from(third & 0b0011_1111),
        ];
        encoded.push(char::from(TABLE[indexes[0]]));
        encoded.push(char::from(TABLE[indexes[1]]));
        encoded.push(if chunk.len() > 1 {
            char::from(TABLE[indexes[2]])
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            char::from(TABLE[indexes[3]])
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{
        base64_encode, builtin_client_command, builtin_quit_command, command_like_name,
        command_palette_items, normalize_editor_content, parse_editor_command, slash_command,
        toggle_yolo, trailing_mention, COMMAND_THEME_MODE, COMMAND_TOGGLE_TIMESTAMPS,
        COMMAND_YOLO_SWITCH,
    };
    use crate::theme::Mode;

    #[test]
    fn trailing_mention_extracts_partial_after_last_at() {
        assert_eq!(trailing_mention("see @gam").as_deref(), Some("gam"));
        assert_eq!(trailing_mention("@").as_deref(), Some(""));
        assert_eq!(trailing_mention("a @b @c").as_deref(), Some("c"));
        assert_eq!(trailing_mention("plain text").as_deref(), None);
        assert_eq!(trailing_mention("@done now").as_deref(), None);
    }

    #[test]
    fn slash_command_matches_known_names_only() {
        let names = vec!["review".to_owned(), "init".to_owned()];
        assert_eq!(
            slash_command("/review", &names),
            Some(("review".to_owned(), String::new()))
        );
        assert_eq!(
            slash_command("/review the code", &names),
            Some(("review".to_owned(), "the code".to_owned()))
        );
        assert_eq!(slash_command("/unknown", &names), None);
        assert_eq!(slash_command("/usr/bin/x", &names), None);
        assert_eq!(slash_command("plain prompt", &names), None);
    }

    #[test]
    fn command_palette_yolo_title_reflects_current_state() {
        assert_eq!(
            command_titles(COMMAND_YOLO_SWITCH, false, Mode::Dark, false),
            ["Enable YOLO mode", "Enable YOLO mode"]
        );
        assert_eq!(
            command_titles(COMMAND_YOLO_SWITCH, true, Mode::Dark, false),
            ["Disable YOLO mode", "Disable YOLO mode"]
        );
        assert!(!command_palette_items(false, Mode::Dark, false, &[])
            .iter()
            .any(|item| item.title == "Switch YOLO"));
    }

    #[test]
    fn command_palette_theme_mode_title_reflects_current_state() {
        assert_eq!(
            command_titles(COMMAND_THEME_MODE, false, Mode::Dark, false),
            ["Switch to light mode"]
        );
        assert_eq!(
            command_titles(COMMAND_THEME_MODE, false, Mode::Light, false),
            ["Switch to dark mode"]
        );
        assert!(!command_palette_items(false, Mode::Dark, false, &[])
            .iter()
            .any(|item| item.title == "Toggle theme mode"));
    }

    #[test]
    fn command_palette_timestamp_title_reflects_current_state() {
        assert_eq!(
            command_titles(COMMAND_TOGGLE_TIMESTAMPS, false, Mode::Dark, false),
            ["Show timestamps"]
        );
        assert_eq!(
            command_titles(COMMAND_TOGGLE_TIMESTAMPS, false, Mode::Dark, true),
            ["Hide timestamps"]
        );
        assert!(!command_palette_items(false, Mode::Dark, false, &[])
            .iter()
            .any(|item| item.title == "Toggle timestamps"));
    }

    fn command_titles(
        command: &str,
        yolo: bool,
        theme_mode: Mode,
        show_timestamps: bool,
    ) -> Vec<String> {
        command_palette_items(yolo, theme_mode, show_timestamps, &[])
            .into_iter()
            .filter(|item| item.value == command)
            .map(|item| item.title)
            .collect()
    }

    #[test]
    fn toggle_yolo_flips_mode_and_reports_state() {
        let mut yolo = false;

        assert_eq!(toggle_yolo(&mut yolo), "yolo mode enabled");
        assert!(yolo);
        assert_eq!(toggle_yolo(&mut yolo), "yolo mode disabled");
        assert!(!yolo);
    }

    #[test]
    fn base64_encode_uses_standard_alphabet_and_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn parse_editor_command_when_args_present_splits_on_whitespace() {
        let command = parse_editor_command("code --wait").expect("editor command");

        assert_eq!(command.program, "code");
        assert_eq!(command.args, ["--wait"]);
    }

    #[test]
    fn parse_editor_command_when_blank_returns_none() {
        assert!(parse_editor_command("  \t ").is_none());
    }

    #[test]
    fn normalize_editor_content_when_crlf_and_trailing_newlines_returns_prompt_text() {
        assert_eq!(normalize_editor_content("one\r\ntwo\r\n\r\n"), "one\ntwo");
    }

    #[test]
    fn builtin_client_command_routes_builtin_slashes_to_client_actions() {
        // Built-in UI commands map to their client-side action so typing them in the
        // prompt performs the action instead of being sent to the model as a prompt.
        assert_eq!(builtin_client_command("/help"), Some("app.help"));
        assert_eq!(builtin_client_command("/model"), Some("model.list"));
        assert_eq!(builtin_client_command("/models"), Some("model.list"));
        assert_eq!(builtin_client_command("/new"), Some("session.new"));
        assert_eq!(builtin_client_command("/clear"), Some("session.new"));
        assert_eq!(builtin_client_command("/agent"), Some("agent.list"));
        assert_eq!(builtin_client_command("/agents"), Some("agent.list"));
        assert_eq!(builtin_client_command("/sessions"), Some("session.list"));
        assert_eq!(builtin_client_command("/resume"), Some("session.list"));
        assert_eq!(builtin_client_command("/compact"), Some("session.compact"));
        assert_eq!(builtin_client_command("/tools"), Some("hya.status"));
        assert_eq!(builtin_client_command("/mcp"), Some("hya.status"));
        assert_eq!(builtin_client_command("/think"), Some("variant.list"));
        assert_eq!(builtin_client_command("/export"), Some("session.export"));
        assert_eq!(builtin_client_command("/?"), Some("app.help"));
        // Arguments are ignored: the action is still invoked.
        assert_eq!(builtin_client_command("/model gpt-4"), Some("model.list"));
        assert_eq!(builtin_client_command("/think high"), Some("variant.list"));
        // Prompt-macro commands, unknown commands, paths and plain text fall through.
        assert_eq!(builtin_client_command("/review"), None);
        assert_eq!(builtin_client_command("/init"), None);
        assert_eq!(builtin_client_command("/yolo"), None);
        assert_eq!(builtin_client_command("/yolo on"), None);
        assert_eq!(builtin_client_command("/bogus"), None);
        assert_eq!(builtin_client_command("/usr/bin/x"), None);
        assert_eq!(builtin_client_command("plain prompt"), None);
        assert_eq!(builtin_client_command("/"), None);
    }

    #[test]
    fn command_like_name_detects_command_syntax_not_paths() {
        assert_eq!(command_like_name("/yolo on"), Some("yolo"));
        assert_eq!(command_like_name("/review the code"), Some("review"));
        assert_eq!(command_like_name("/bogus"), Some("bogus"));
        assert_eq!(command_like_name("/usr/bin/x"), None);
        assert_eq!(command_like_name("plain prompt"), None);
        assert_eq!(command_like_name("/"), None);
        let names = vec!["review".to_owned()];
        assert_eq!(slash_command("/bogus", &names), None);
        assert_eq!(command_like_name("/bogus"), Some("bogus"));
    }

    #[test]
    fn builtin_quit_command_detects_documented_slash_exit_aliases() {
        assert!(builtin_quit_command("/quit"));
        assert!(builtin_quit_command("/exit"));
        assert!(builtin_quit_command("/q"));
        assert!(builtin_quit_command("/quit now"));
        assert!(!builtin_quit_command("quit"));
        assert!(!builtin_quit_command("/review"));
        assert!(!builtin_quit_command("/usr/bin/x"));
    }
}
