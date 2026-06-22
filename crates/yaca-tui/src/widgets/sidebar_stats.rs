use yaca_proto::{PartProjection, Role, ToolPartState};

use super::error::is_system_error_text;
use crate::AppState;

#[derive(Default)]
pub struct TranscriptStats {
    pub messages: usize,
    pub tools: usize,
    pub errors: usize,
    pub attachments: usize,
    pub estimated_tokens: u64,
}

pub fn transcript_stats(state: &AppState) -> TranscriptStats {
    let mut stats = TranscriptStats {
        attachments: state.attachments.len(),
        estimated_tokens: estimated_input_tokens(state),
        ..TranscriptStats::default()
    };
    for message in &state.projection.session.messages {
        stats.messages += 1;
        let mut system_text = String::new();
        for part in &message.parts {
            match part {
                PartProjection::Text { text, .. } => {
                    if message.role == Role::System {
                        system_text.push_str(text);
                    }
                }
                PartProjection::Reasoning { .. } => {}
                PartProjection::Tool { state, .. } => {
                    stats.tools += 1;
                    if matches!(state, ToolPartState::Error { .. }) {
                        stats.errors += 1;
                    }
                }
            }
        }
        if message.role == Role::System && is_system_error_text(&system_text) {
            stats.errors += 1;
        }
    }
    stats
}

fn estimated_input_tokens(state: &AppState) -> u64 {
    let mut chars = state.input.chars().count();
    for message in &state.projection.session.messages {
        for part in &message.parts {
            match part {
                PartProjection::Text { text, .. } | PartProjection::Reasoning { text, .. } => {
                    chars = chars.saturating_add(text.chars().count());
                }
                PartProjection::Tool { name, state, .. } => {
                    chars = chars.saturating_add(name.as_str().chars().count());
                    chars = chars.saturating_add(tool_chars(state));
                }
            }
        }
    }
    u64::try_from(chars.saturating_add(3) / 4).unwrap_or(u64::MAX)
}

fn tool_chars(state: &ToolPartState) -> usize {
    match state {
        ToolPartState::Pending { input } | ToolPartState::Running { input } => {
            input.to_string().chars().count()
        }
        ToolPartState::Completed { input, output, .. } => input
            .to_string()
            .chars()
            .count()
            .saturating_add(output.to_string().chars().count()),
        ToolPartState::Error { input, message } => input
            .to_string()
            .chars()
            .count()
            .saturating_add(message.chars().count()),
    }
}
