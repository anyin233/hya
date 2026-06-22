//! `yaca-tui` — ratatui rendering of the projected agent state.
//!
//! Pure view: [`AppState`] holds the projection plus interaction state (input
//! buffer, scrollback, in-flight flag) and [`draw`] paints a chat layout. All
//! terminal I/O and the event loop live in the binary so this stays testable.

use ratatui::Frame;
use yaca_proto::{Envelope, Projection};

mod ansi;
mod layout;
mod theme;
mod tool_inputs;
mod tool_labels;
mod tool_questions;
mod tool_tasks;
mod tool_todos;
mod view_model;
mod widgets;

#[derive(Default)]
pub struct AppState {
    pub projection: Projection,
    pub goal: Option<GoalView>,
    pub loop_view: Option<LoopView>,
    pub team: Vec<(String, String)>,
    pub permission: Option<PermissionPrompt>,
    pub question: Option<QuestionPrompt>,
    pub picker: Option<Picker>,
    pub dialog: Option<DialogView>,
    pub attachments: Vec<PromptAttachment>,
    pub input: String,
    pub input_cursor: Option<usize>,
    pub yolo: bool,
    pub exit_armed: bool,
    pub running: bool,
    pub scroll_back: u16,
    pub agent: String,
    pub model: String,
    pub model_provider_label: Option<String>,
    pub session_label: String,
    pub reasoning_effort: Option<String>,
    pub cost_label: Option<String>,
    pub context: ContextView,
    pub mcp: Vec<ConnectorView>,
    pub lsp_status: Option<String>,
    pub branch_label: Option<String>,
    pub workspace_workdir: Option<String>,
    pub changed_files: Vec<ChangedFileView>,
    pub selected_message: Option<usize>,
    pub selected_message_scroll_anchor: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextView {
    pub session_saved_tokens: Option<u64>,
    pub all_time_saved_tokens: Option<u64>,
    pub saved_percent_basis_points: Option<u16>,
    pub current_tokens: Option<u64>,
    pub context_window_tokens: Option<u64>,
    pub spent_label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectorState {
    Connected,
    Failed(String),
    NeedsAuth,
    Disabled,
}

impl ConnectorState {
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Connected => "Connected",
            Self::Failed(error) => error,
            Self::NeedsAuth => "Needs auth",
            Self::Disabled => "Disabled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorView {
    pub name: String,
    pub state: ConnectorState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogView {
    pub title: String,
    pub subtitle: String,
    pub items: Vec<DialogItem>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogItem {
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptAttachment {
    pub placeholder: String,
    pub source_path: Option<String>,
    pub mime: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFileView {
    pub path: String,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

pub struct GoalView {
    pub condition: String,
    pub turns: u32,
    pub last_reason: String,
}

pub struct LoopView {
    pub target: String,
    pub iteration: u32,
    pub budget: u32,
    pub last_score: u8,
}

pub struct PermissionPrompt {
    pub title: String,
    pub detail: String,
    pub selected: usize,
    pub reply: String,
    pub stage: PermissionPromptStage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionPromptStage {
    #[default]
    Permission,
    Always,
    Reject,
}

impl PermissionPrompt {
    #[must_use]
    pub fn options(&self) -> &'static [&'static str] {
        match self.stage {
            PermissionPromptStage::Permission => &["Allow once", "Allow always", "Reject"],
            PermissionPromptStage::Always => &["Confirm", "Cancel"],
            PermissionPromptStage::Reject => &[],
        }
    }
}

pub struct QuestionPrompt {
    pub prompt: String,
    pub options: Vec<String>,
    pub selected: usize,
    pub input: String,
    pub allow_custom: bool,
}

pub struct Picker {
    pub title: String,
    pub entries: Vec<String>,
    pub selected: usize,
}

impl AppState {
    pub fn apply(&mut self, envelope: &Envelope) {
        self.projection.apply(envelope);
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_sub(lines);
    }

    pub fn set_model_identity(&mut self, model: String, provider_label: Option<String>) {
        self.model = model;
        self.model_provider_label = provider_label.filter(|label| !label.trim().is_empty());
    }

    pub fn visible_branch_label(&self) -> Option<&str> {
        let branch = self
            .branch_label
            .as_deref()
            .filter(|branch| !branch.is_empty())?;
        match (&self.workspace_workdir, &self.projection.session.workdir) {
            (Some(workspace), Some(session)) => (workspace == session).then_some(branch),
            (Some(_), None) => None,
            (None, _) => Some(branch),
        }
    }
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let theme = theme::Theme::yaca_dark();
    let area = frame.area();
    let footer_height = 1;
    let prompt_height = widgets::prompt_height(state, layout::main_width(area));
    let layout = layout::app_layout(area, prompt_height, footer_height);
    widgets::render_timeline(frame, layout.timeline, state, &theme);
    if let Some(sidebar) = layout.sidebar {
        widgets::render_sidebar(frame, sidebar, state, &theme);
    }
    widgets::render_runtime_status(frame, layout.runtime_status, state, &theme);
    widgets::render_prompt(frame, layout.prompt, state, &theme, area.width);
    widgets::render_footer(frame, layout.footer, state, &theme, area.width);

    if let Some(prompt) = &state.permission {
        widgets::render_permission(frame, prompt, &theme);
    } else if let Some(question) = &state.question {
        widgets::render_question(frame, question, &theme);
    } else if let Some(picker) = &state.picker {
        widgets::render_picker(frame, picker, &theme);
    } else if let Some(dialog) = &state.dialog {
        widgets::render_dialog(frame, dialog, &theme);
    } else if let Some(cursor) = widgets::prompt_cursor(state, layout.prompt) {
        frame.set_cursor_position(cursor);
    }
}
