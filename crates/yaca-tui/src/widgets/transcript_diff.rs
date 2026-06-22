#[derive(Clone, Copy)]
pub(crate) enum DiffLineKind {
    Hunk,
    Added,
    Removed,
    Context,
}

pub(crate) struct DiffDisplayLine {
    pub text: String,
    pub kind: DiffLineKind,
}

pub(crate) fn format_unified_diff(output: &str) -> Option<Vec<DiffDisplayLine>> {
    let mut old_line = 0;
    let mut new_line = 0;
    let mut saw_hunk = false;
    let mut lines = Vec::new();

    for line in output.lines() {
        if let Some((old_start, new_start)) = parse_hunk_starts(line) {
            old_line = old_start;
            new_line = new_start;
            saw_hunk = true;
            lines.push(DiffDisplayLine {
                text: format!("     {line}"),
                kind: DiffLineKind::Hunk,
            });
        } else if saw_hunk && line.starts_with('-') && !line.starts_with("---") {
            lines.push(numbered_line(
                old_line,
                '-',
                &line[1..],
                DiffLineKind::Removed,
            ));
            old_line = old_line.saturating_add(1);
        } else if saw_hunk && line.starts_with('+') && !line.starts_with("+++") {
            lines.push(numbered_line(
                new_line,
                '+',
                &line[1..],
                DiffLineKind::Added,
            ));
            new_line = new_line.saturating_add(1);
        } else if saw_hunk && line.starts_with(' ') {
            lines.push(numbered_line(
                new_line,
                ' ',
                &line[1..],
                DiffLineKind::Context,
            ));
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        } else {
            lines.push(DiffDisplayLine {
                text: format!("     {line}"),
                kind: DiffLineKind::Context,
            });
        }
    }

    saw_hunk.then_some(lines)
}

fn numbered_line(number: u64, sign: char, text: &str, kind: DiffLineKind) -> DiffDisplayLine {
    DiffDisplayLine {
        text: format!("{number:>4} {sign}{text}"),
        kind,
    }
}

fn parse_hunk_starts(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old_start = parse_range_start(parts.next()?, '-')?;
    let new_start = parse_range_start(parts.next()?, '+')?;
    Some((old_start, new_start))
}

fn parse_range_start(token: &str, sign: char) -> Option<u64> {
    let range = token.strip_prefix(sign)?;
    range
        .split_once(',')
        .map_or(range, |(start, _)| start)
        .parse()
        .ok()
}
