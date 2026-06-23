use ratatui::text::Line;

use super::sidebar_context::{meta, push_section};
use crate::AppState;
use crate::theme::Theme;

pub(super) fn push_runtime(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    if state.goal.is_none() && state.loop_view.is_none() && !state.yolo && !state.running {
        return;
    }
    lines.push(Line::from(""));
    push_section(lines, "Runtime", theme.accent, theme);
    if state.running {
        lines.push(meta("state Running", theme.warning));
    }
    if let Some(goal) = &state.goal {
        lines.push(meta(
            format!("GOAL:{} turns {}", goal.condition, goal.turns),
            theme.text,
        ));
    }
    if let Some(loop_view) = &state.loop_view {
        lines.push(meta(
            format!(
                "LOOP:{} iter {}/{} score {}",
                loop_view.target, loop_view.iteration, loop_view.budget, loop_view.last_score
            ),
            theme.text,
        ));
    }
    if state.yolo {
        lines.push(meta("mode YOLO", theme.warning));
    }
}
