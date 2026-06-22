use super::transcript_tools::status_label;
use crate::view_model::TimelinePart;

pub(super) fn text_from_parts(parts: &[TimelinePart]) -> String {
    let mut text = String::new();
    for part in parts {
        match part {
            TimelinePart::Text(value) => text.push_str(value),
            TimelinePart::Reasoning(_) => {}
            TimelinePart::Tool { name, status, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("tool {name} {}", status_label(status)));
            }
        }
    }
    text
}
