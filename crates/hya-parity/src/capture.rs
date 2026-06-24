//! Terminal-capture normalization for parity comparisons.

/// Normalize a captured terminal frame for stable diffing: trim trailing whitespace on
/// each line and drop trailing blank lines. (Cursor/SGR handling is added in W10.)
#[must_use]
pub fn normalize(frame: &str) -> String {
    let mut lines: Vec<&str> = frame.lines().map(str::trim_end).collect();
    while matches!(lines.last(), Some(&"")) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_space_and_blank_lines() {
        assert_eq!(normalize("a  \nb\t\n\n\n"), "a\nb");
    }
}
