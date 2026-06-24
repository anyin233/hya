use crate::render::text::{Attrs, Line, Span};
use crate::theme::ResolvedTheme;

const MAX_DIFF_LINES: usize = 40;

#[must_use]
pub fn render_unified(diff: &str, theme: &ResolvedTheme) -> Vec<Line> {
    let mut out = Vec::new();
    let mut new_line = 0i64;
    let mut emitted = 0usize;
    let body: Vec<&str> = diff.lines().filter(|raw| !is_file_header(raw)).collect();
    let total = body.len();
    for raw in body {
        if emitted >= MAX_DIFF_LINES {
            out.push(indent(
                format!("\u{2026} +{} lines", total - emitted),
                theme.diff_line_number,
                None,
            ));
            break;
        }
        if let Some(rest) = raw.strip_prefix("@@") {
            new_line = parse_new_start(rest);
            out.push(indent(raw.to_owned(), theme.diff_hunk_header, None));
            emitted += 1;
            continue;
        }
        let (color, background, gutter) = match raw.as_bytes().first() {
            Some(b'+') => {
                let line = new_line;
                new_line += 1;
                (
                    theme.diff_added,
                    Some(theme.diff_added_bg),
                    gutter(Some(line)),
                )
            }
            Some(b'-') => (
                theme.diff_removed,
                Some(theme.diff_removed_bg),
                gutter(None),
            ),
            _ => {
                let line = new_line;
                new_line += 1;
                (theme.diff_context, None, gutter(Some(line)))
            }
        };
        out.push(Line(vec![
            Span::styled(
                format!("{gutter} "),
                Some(theme.diff_line_number),
                None,
                Attrs::default(),
            ),
            Span::styled(raw.to_owned(), Some(color), background, Attrs::default()),
        ]));
        emitted += 1;
    }
    out
}

const SPLIT_SEPARATOR: &str = " \u{2502} ";

enum DiffRow {
    Hunk(String),
    Context {
        old_no: i64,
        new_no: i64,
        text: String,
    },
    Change {
        old: Option<(i64, String)>,
        new: Option<(i64, String)>,
    },
}

#[must_use]
pub fn render_split(diff: &str, width: usize, theme: &ResolvedTheme) -> Vec<Line> {
    let col = width.saturating_sub(SPLIT_SEPARATOR.chars().count()) / 2;
    if col < 8 {
        return render_unified(diff, theme);
    }
    let rows = split_rows(diff);
    let total = rows.len();
    let mut out = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        if index >= MAX_DIFF_LINES {
            out.push(indent(
                format!("\u{2026} +{} lines", total - index),
                theme.diff_line_number,
                None,
            ));
            break;
        }
        out.push(render_row(&row, col, theme));
    }
    out
}

fn split_rows(diff: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_no = 1i64;
    let mut new_no = 1i64;
    let mut pending_old: Vec<String> = Vec::new();
    let mut pending_new: Vec<String> = Vec::new();
    for raw in diff.lines().filter(|raw| !is_file_header(raw)) {
        if let Some(rest) = raw.strip_prefix("@@") {
            flush_change(
                &mut rows,
                &mut pending_old,
                &mut pending_new,
                &mut old_no,
                &mut new_no,
            );
            old_no = parse_old_start(rest);
            new_no = parse_new_start(rest);
            rows.push(DiffRow::Hunk(raw.to_owned()));
            continue;
        }
        match raw.as_bytes().first() {
            Some(b'-') => pending_old.push(raw[1..].to_owned()),
            Some(b'+') => pending_new.push(raw[1..].to_owned()),
            _ => {
                flush_change(
                    &mut rows,
                    &mut pending_old,
                    &mut pending_new,
                    &mut old_no,
                    &mut new_no,
                );
                let text = raw.strip_prefix(' ').unwrap_or(raw).to_owned();
                rows.push(DiffRow::Context {
                    old_no,
                    new_no,
                    text,
                });
                old_no += 1;
                new_no += 1;
            }
        }
    }
    flush_change(
        &mut rows,
        &mut pending_old,
        &mut pending_new,
        &mut old_no,
        &mut new_no,
    );
    rows
}

fn flush_change(
    rows: &mut Vec<DiffRow>,
    pending_old: &mut Vec<String>,
    pending_new: &mut Vec<String>,
    old_no: &mut i64,
    new_no: &mut i64,
) {
    let count = pending_old.len().max(pending_new.len());
    for index in 0..count {
        let old = pending_old.get(index).map(|text| {
            let number = *old_no;
            *old_no += 1;
            (number, text.clone())
        });
        let new = pending_new.get(index).map(|text| {
            let number = *new_no;
            *new_no += 1;
            (number, text.clone())
        });
        rows.push(DiffRow::Change { old, new });
    }
    pending_old.clear();
    pending_new.clear();
}

fn render_row(row: &DiffRow, col: usize, theme: &ResolvedTheme) -> Line {
    let separator = || {
        Span::styled(
            SPLIT_SEPARATOR.to_owned(),
            Some(theme.diff_line_number),
            None,
            Attrs::default(),
        )
    };
    match row {
        DiffRow::Hunk(text) => indent(text.clone(), theme.diff_hunk_header, None),
        DiffRow::Context {
            old_no,
            new_no,
            text,
        } => Line(vec![
            Span::styled(
                split_cell(Some(*old_no), text, col),
                Some(theme.diff_context),
                None,
                Attrs::default(),
            ),
            separator(),
            Span::styled(
                split_cell(Some(*new_no), text, col),
                Some(theme.diff_context),
                None,
                Attrs::default(),
            ),
        ]),
        DiffRow::Change { old, new } => {
            let left = match old {
                Some((number, text)) => Span::styled(
                    split_cell(Some(*number), text, col),
                    Some(theme.diff_removed),
                    Some(theme.diff_removed_bg),
                    Attrs::default(),
                ),
                None => Span::styled(
                    " ".repeat(col),
                    Some(theme.diff_context),
                    None,
                    Attrs::default(),
                ),
            };
            let right = match new {
                Some((number, text)) => Span::styled(
                    split_cell(Some(*number), text, col),
                    Some(theme.diff_added),
                    Some(theme.diff_added_bg),
                    Attrs::default(),
                ),
                None => Span::styled(
                    " ".repeat(col),
                    Some(theme.diff_context),
                    None,
                    Attrs::default(),
                ),
            };
            Line(vec![left, separator(), right])
        }
    }
}

fn split_cell(line_no: Option<i64>, content: &str, col: usize) -> String {
    let number = line_no.map_or_else(|| "    ".to_owned(), |value| format!("{value:>4}"));
    let avail = col.saturating_sub(5);
    let shown: String = content.chars().take(avail).collect();
    let pad = avail.saturating_sub(shown.chars().count());
    format!("{number} {shown}{}", " ".repeat(pad))
}

fn parse_old_start(hunk: &str) -> i64 {
    hunk.split('-')
        .nth(1)
        .and_then(|rest| rest.split([',', ' ']).next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1)
}

fn is_file_header(raw: &str) -> bool {
    raw.starts_with("Index:")
        || raw.starts_with("===")
        || raw.starts_with("--- ")
        || raw.starts_with("+++ ")
}

fn gutter(line: Option<i64>) -> String {
    line.map_or_else(|| "    ".to_owned(), |value| format!("{value:>4}"))
}

fn parse_new_start(hunk: &str) -> i64 {
    hunk.split('+')
        .nth(1)
        .and_then(|rest| rest.split([',', ' ']).next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1)
}

fn indent(
    text: String,
    color: crate::contracts::Rgba,
    background: Option<crate::contracts::Rgba>,
) -> Line {
    Line(vec![Span::styled(
        text,
        Some(color),
        background,
        Attrs::default(),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};

    fn theme() -> ResolvedTheme {
        resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap()
    }

    const SAMPLE: &str = "Index: /tmp/foo.txt\n===================================================================\n--- /tmp/foo.txt\n+++ /tmp/foo.txt\n@@ -1,3 +1,3 @@\n line one\n-old value here\n+new value now\n line three\n";

    #[test]
    fn unified_skips_file_headers_and_keeps_hunk_and_changes() {
        let lines = render_unified(SAMPLE, &theme());
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.0.iter().map(|s| s.text.clone()).collect())
            .collect();
        let joined = rendered.join("\n");
        assert!(!joined.contains("Index:"), "file header leaked: {joined}");
        assert!(!joined.contains("==="), "separator leaked: {joined}");
        assert!(
            joined.contains("@@ -1,3 +1,3 @@"),
            "hunk header missing: {joined}"
        );
        assert!(joined.contains("-old value here"), "removed line missing");
        assert!(joined.contains("+new value now"), "added line missing");
        assert!(joined.contains("line one"), "context line missing");
    }

    #[test]
    fn added_and_removed_lines_use_diff_colors() {
        let theme = theme();
        let lines = render_unified(SAMPLE, &theme);
        let added = lines
            .iter()
            .find(|l| l.0.iter().any(|s| s.text.starts_with('+')))
            .expect("an added line");
        let added_span = added.0.iter().find(|s| s.text.starts_with('+')).unwrap();
        assert_eq!(added_span.fg, Some(theme.diff_added));
        assert_eq!(added_span.bg, Some(theme.diff_added_bg));
    }

    #[test]
    fn split_view_pairs_old_left_new_right_and_context_both_columns() {
        let theme = theme();
        let lines = render_split(SAMPLE, 80, &theme);
        let change_row = lines
            .iter()
            .find(|l| l.0.iter().any(|s| s.text.contains("old value here")))
            .expect("a row containing the removed text");
        assert_eq!(
            change_row.0.len(),
            3,
            "split row = left | separator | right"
        );
        let left = &change_row.0[0];
        let right = &change_row.0[2];
        assert!(
            left.text.contains("old value here"),
            "removed text on the left"
        );
        assert_eq!(left.fg, Some(theme.diff_removed));
        assert!(
            right.text.contains("new value now"),
            "added text paired on the right: {:?}",
            right.text
        );
        assert_eq!(right.fg, Some(theme.diff_added));

        let context_row = lines
            .iter()
            .find(|l| l.0.len() == 3 && l.0[0].text.contains("line one"))
            .expect("a context row with 'line one'");
        assert!(
            context_row.0[2].text.contains("line one"),
            "context mirrored right"
        );
    }

    #[test]
    fn split_view_falls_back_to_unified_when_too_narrow() {
        let theme = theme();
        let narrow = render_split(SAMPLE, 10, &theme);
        let unified = render_unified(SAMPLE, &theme);
        assert_eq!(narrow.len(), unified.len(), "narrow split == unified");
    }
}
