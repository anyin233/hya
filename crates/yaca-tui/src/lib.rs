//! `yaca-tui` — ratatui rendering of the projected agent state.
//!
//! Pure view: [`AppState`] holds the projection plus interaction state (input
//! buffer, scrollback, in-flight flag) and [`draw`] paints a chat layout. All
//! terminal I/O and the event loop live in the binary so this stays testable.

use ratatui::Frame;
use yaca_proto::{Envelope, Projection};

mod layout;
mod theme;
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
    pub yolo: bool,
    pub exit_armed: bool,
    pub running: bool,
    pub scroll_back: u16,
    pub model: String,
    pub session_label: String,
    pub reasoning_effort: Option<String>,
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
}

impl PermissionPrompt {
    #[must_use]
    pub fn options(&self) -> [String; 3] {
        [
            "Allow once".to_string(),
            format!("Allow all {}", self.title),
            "Deny".to_string(),
        ]
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
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let theme = theme::Theme::yaca_dark();
    let layout = layout::app_layout(frame.area());
    widgets::render_status(frame, layout.status, state, &theme);
    widgets::render_timeline(frame, layout.timeline, state, &theme);
    if let Some(sidebar) = layout.sidebar {
        widgets::render_sidebar(frame, sidebar, state, &theme);
    }
    widgets::render_prompt(frame, layout.prompt, state, &theme);
    widgets::render_footer(frame, layout.footer, state, &theme);

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
