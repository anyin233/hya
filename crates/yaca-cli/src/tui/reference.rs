use std::collections::BTreeSet;
use std::path::Path;

const MAX_MENTIONS: usize = 8;
const MAX_FILE_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_BYTES: usize = 48 * 1024;
const MAX_DIRECTORY_ITEMS: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Mention {
    path: String,
    lines: Option<(usize, usize)>,
}

pub fn expand_mentions(workdir: &Path, input: &str) -> std::io::Result<String> {
    let mentions = mentions(input);
    if mentions.is_empty() {
        return Ok(input.to_string());
    }
    let root = std::fs::canonicalize(workdir)?;
    let mut blocks = Vec::new();
    let mut total_bytes = 0usize;
    for mention in mentions.into_iter().take(MAX_MENTIONS) {
        let resolved = root.join(&mention.path);
        let Ok(canonical) = std::fs::canonicalize(&resolved) else {
            continue;
        };
        if !canonical.starts_with(&root) {
            continue;
        }
        let relative = canonical
            .strip_prefix(&root)
            .unwrap_or(canonical.as_path())
            .to_path_buf();
        let block = if canonical.is_dir() {
            directory_block(&root, &canonical, &relative)?
        } else {
            file_block(&canonical, &relative, mention.lines)?
        };
        total_bytes = total_bytes.saturating_add(block.len());
        blocks.push(block);
        if total_bytes >= MAX_CONTEXT_BYTES {
            break;
        }
    }
    if blocks.is_empty() {
        return Ok(input.to_string());
    }
    Ok(format!(
        "{input}\n\n<context source=\"@mentions\">\n{}\n</context>",
        blocks.join("\n")
    ))
}

fn mentions(input: &str) -> Vec<Mention> {
    let mut found = BTreeSet::new();
    let bytes = input.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] != b'@' || (idx > 0 && !bytes[idx - 1].is_ascii_whitespace()) {
            idx += 1;
            continue;
        }
        let start = idx + 1;
        let mut end = start;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        if end > start
            && let Some(raw) = input.get(start..end)
            && let Some(mention) = parse_mention(raw)
        {
            found.insert(mention);
        }
        idx = end.saturating_add(1);
    }
    found.into_iter().collect()
}

fn parse_mention(raw: &str) -> Option<Mention> {
    let trimmed = raw.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | ')' | ']'));
    if trimmed.is_empty() {
        return None;
    }
    let (path, lines) = match trimmed.split_once("#L") {
        Some((path, range)) => (path, parse_line_range(range)),
        None => (trimmed, None),
    };
    if path.is_empty() || path.starts_with('/') {
        return None;
    }
    Some(Mention {
        path: path.to_string(),
        lines,
    })
}

fn parse_line_range(raw: &str) -> Option<(usize, usize)> {
    let (start, end) = raw
        .split_once('-')
        .map_or((raw, raw), |(start, end)| (start, end));
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    if start == 0 || end < start {
        return None;
    }
    Some((start, end))
}

fn file_block(
    path: &Path,
    relative: &Path,
    lines: Option<(usize, usize)>,
) -> std::io::Result<String> {
    let content = std::fs::read_to_string(path)?;
    let selected = select_lines(&content, lines);
    let truncated = truncate(&selected, MAX_FILE_BYTES);
    let rel = display_path(relative);
    let attrs = lines.map_or_else(String::new, |(start, end)| {
        format!(" lines=\"{start}-{end}\"")
    });
    Ok(format!(
        "<file path=\"{}\"{}>\n{}\n</file>",
        escape_attr(&rel),
        attrs,
        truncated
    ))
}

fn directory_block(root: &Path, path: &Path, relative: &Path) -> std::io::Result<String> {
    let mut entries = Vec::new();
    collect_directory(root, path, 0, &mut entries)?;
    entries.sort();
    entries.truncate(MAX_DIRECTORY_ITEMS);
    Ok(format!(
        "<directory path=\"{}\">\n{}\n</directory>",
        escape_attr(&display_path(relative)),
        entries.join("\n")
    ))
}

fn collect_directory(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    if depth > 2 || out.len() >= MAX_DIRECTORY_ITEMS {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if out.len() >= MAX_DIRECTORY_ITEMS {
            break;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            ".git" | "target" | ".worktrees" | "node_modules"
        ) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        out.push(display_path(relative));
        if entry.file_type()?.is_dir() {
            collect_directory(root, &path, depth + 1, out)?;
        }
    }
    Ok(())
}

fn select_lines(content: &str, lines: Option<(usize, usize)>) -> String {
    let Some((start, end)) = lines else {
        return content.to_string();
    };
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_no = idx + 1;
            (line_no >= start && line_no <= end).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n...[truncated {} bytes]",
        &value[..end],
        value.len() - end
    )
}

fn display_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    fn temp_root() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "yaca-reference-test-{nanos}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn file_mentions_expand_into_bounded_context_blocks() {
        let root = temp_root();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn answer() -> u8 { 42 }\n").unwrap();

        let expanded = super::expand_mentions(&root, "Review @src/lib.rs please").unwrap();

        assert!(expanded.starts_with("Review @src/lib.rs please"));
        assert!(expanded.contains("<file path=\"src/lib.rs\">"));
        assert!(expanded.contains("pub fn answer() -> u8 { 42 }"));
        assert!(expanded.contains("</file>"));
    }

    #[test]
    fn line_range_mentions_include_only_requested_lines() {
        let root = temp_root();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.rs"), "one\ntwo\nthree\nfour\n").unwrap();

        let expanded = super::expand_mentions(&root, "Explain @main.rs#L2-3").unwrap();

        assert!(expanded.contains("<file path=\"main.rs\" lines=\"2-3\">"));
        assert!(!expanded.contains("\none\n"));
        assert!(expanded.contains("two\nthree"));
        assert!(!expanded.contains("\nfour\n"));
    }

    #[test]
    fn directory_mentions_expand_to_a_short_listing() {
        let root = temp_root();
        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("README.md"), "# Docs\n").unwrap();

        let expanded = super::expand_mentions(&root, "Summarize @docs").unwrap();

        assert!(expanded.contains("<directory path=\"docs\">"));
        assert!(expanded.contains("docs/README.md"));
        assert!(expanded.contains("</directory>"));
    }
}
