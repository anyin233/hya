use crate::AppState;

use super::identity::active_agent_label;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssistantBlockStatus {
    Completed { duration_ms: Option<u64> },
    Streaming,
}

pub(super) fn assistant_metadata_label(state: &AppState, status: AssistantBlockStatus) -> String {
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let provider = state
        .model_provider_label
        .as_deref()
        .filter(|label| !label.is_empty())
        .map_or(String::new(), |label| format!(" {label}"));
    let identity = format!("▣ {} · {model}{provider}", active_agent_label(state));
    match status {
        AssistantBlockStatus::Completed { duration_ms } => format!(
            "{identity} · {}",
            duration_ms
                .map(format_duration)
                .unwrap_or_else(|| "completed".to_string())
        ),
        AssistantBlockStatus::Streaming => identity,
    }
}

pub(super) fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1_000.0);
    }
    if ms < 3_600_000 {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1_000;
        return format!("{minutes}m {seconds}s");
    }
    if ms < 86_400_000 {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        return format!("{hours}h {minutes}m");
    }
    let days = ms / 86_400_000;
    let hours = (ms % 86_400_000) / 3_600_000;
    format!("{days}d {hours}h")
}
