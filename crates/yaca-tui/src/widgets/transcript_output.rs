const GENERIC_OUTPUT_PREVIEW_LINES: usize = 3;
const DETAILED_OUTPUT_PREVIEW_LINES: usize = 10;
const OUTPUT_MIN_LINE_CHARS: usize = 20;
const OUTPUT_GUTTER_CHARS: u16 = 6;

pub(super) fn collapsed_tool_output(name: &str, output: &str, width: u16) -> String {
    let max_lines = if matches!(name, "ask_user" | "bash" | "shell" | "task" | "todowrite") {
        DETAILED_OUTPUT_PREVIEW_LINES
    } else {
        GENERIC_OUTPUT_PREVIEW_LINES
    };
    collapse_tool_output(output, max_lines, width)
}

fn collapse_tool_output(output: &str, max_lines: usize, width: u16) -> String {
    let max_chars = max_lines
        * usize::from(width.saturating_sub(OUTPUT_GUTTER_CHARS)).max(OUTPUT_MIN_LINE_CHARS);
    let lines = output.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines && output.chars().count() <= max_chars {
        return output.to_string();
    }

    let preview = lines
        .iter()
        .take(max_lines)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if preview.chars().count() > max_chars {
        let head = preview
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        format!("{head}…")
    } else {
        format!("{preview}\n…")
    }
}
