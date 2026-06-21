use yaca_proto::{PartProjection, Projection, Role, ToolPartState};

pub enum TimelinePart {
    Text(String),
    Reasoning(String),
    Tool { name: String, status: ToolStatus },
}

pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Error,
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
            status: match state {
                ToolPartState::Pending { .. } => ToolStatus::Pending,
                ToolPartState::Running { .. } => ToolStatus::Running,
                ToolPartState::Completed { .. } => ToolStatus::Completed,
                ToolPartState::Error { .. } => ToolStatus::Error,
            },
        },
    }
}
