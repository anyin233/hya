use hya_proto::{PartProjection, Projection, Role, ToolPartState};

pub enum TimelinePart {
    Text(String),
    Reasoning(String),
    Tool {
        name: String,
        input: String,
        status: ToolStatus,
    },
}

pub enum ToolStatus {
    Pending,
    Running,
    Completed { time_ms: u64 },
    Error { message: String },
}

pub struct TimelineItem {
    pub role: Role,
    pub parts: Vec<TimelinePart>,
}

#[must_use]
pub fn timeline_items(projection: &Projection) -> Vec<TimelineItem> {
    projection
        .session
        .messages
        .iter()
        .map(|message| TimelineItem {
            role: message.role,
            parts: message.parts.iter().map(part_to_timeline).collect(),
        })
        .collect()
}

fn part_to_timeline(part: &PartProjection) -> TimelinePart {
    match part {
        PartProjection::Text { text, .. } => TimelinePart::Text(text.clone()),
        PartProjection::Reasoning { text, .. } => TimelinePart::Reasoning(text.clone()),
        PartProjection::Tool { name, state, .. } => TimelinePart::Tool {
            name: name.to_string(),
            input: tool_input(state),
            status: match state {
                ToolPartState::Pending { .. } => ToolStatus::Pending,
                ToolPartState::Running { .. } => ToolStatus::Running,
                ToolPartState::Completed { time_ms, .. } => {
                    ToolStatus::Completed { time_ms: *time_ms }
                }
                ToolPartState::Error { message, .. } => ToolStatus::Error {
                    message: ellipsize(message, 40),
                },
            },
        },
    }
}

fn tool_input(state: &ToolPartState) -> String {
    let value = match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
    };
    if value.is_null() {
        String::new()
    } else {
        ellipsize(&value.to_string(), 48)
    }
}

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}
