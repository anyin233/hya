use std::path::Path;

use serde_json::{Value, json};

use crate::lsp_path::display_path;
use crate::tool::ToolError;

const UTF8_BOM_BYTES: &[u8; 3] = b"\xEF\xBB\xBF";
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_BYTES: usize = 50 * 1024;
const MAX_BYTES_LABEL: &str = "50 KB";

struct LimitedLines {
    raw: Vec<String>,
    count: usize,
    cut: bool,
    more: bool,
    offset: usize,
}

pub(crate) async fn read_file(
    path: &Path,
    workdir: &Path,
    offset: usize,
    limit: usize,
) -> Result<Value, ToolError> {
    let content = read_utf8_text(path).await?;
    let file = limited_lines(&content, offset, limit);
    if file.count < file.offset && !(file.count == 0 && file.offset == 1) {
        return Err(ToolError::Other(format!(
            "Offset {} is out of range for this file ({} lines)",
            file.offset, file.count
        )));
    }

    let last = file.offset + file.raw.len();
    let last = last.saturating_sub(1);
    let truncated = file.more || file.cut;
    let text = file.raw.join("\n");
    let numbered = file
        .raw
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}: {line}", i + file.offset))
        .collect::<Vec<_>>()
        .join("\n");
    let output = format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n{}\n\n{}\n</content>",
        display_path(path),
        numbered,
        file_footer(&file, last),
    );

    Ok(json!({
        "title": relative_title(path, workdir),
        "output": output,
        "content": text,
        "metadata": {
            "preview": file.raw.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
            "truncated": truncated,
            "loaded": [],
            "display": {
                "type": "file",
                "path": display_path(path),
                "text": text,
                "lineStart": file.offset,
                "lineEnd": last,
                "totalLines": file.count,
                "truncated": truncated,
            },
        },
    }))
}

fn limited_lines(content: &str, offset: usize, limit: usize) -> LimitedLines {
    let start = offset.saturating_sub(1);
    let mut raw = Vec::new();
    let mut bytes = 0;
    let mut count = 0;
    let mut cut = false;
    let mut more = false;

    for text in content.lines() {
        count += 1;
        if count <= start {
            continue;
        }
        if raw.len() >= limit {
            more = true;
            continue;
        }

        let line = truncate_line(text);
        let size = line.len() + usize::from(!raw.is_empty());
        if bytes + size <= MAX_BYTES {
            raw.push(line);
            bytes += size;
            continue;
        }

        cut = true;
        more = true;
        break;
    }

    LimitedLines {
        raw,
        count,
        cut,
        more,
        offset,
    }
}

fn truncate_line(text: &str) -> String {
    if text.chars().count() <= MAX_LINE_LENGTH {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(MAX_LINE_LENGTH)
        .map_or(text.len(), |(index, _)| index);
    format!("{}{}", &text[..end], MAX_LINE_SUFFIX)
}

async fn read_utf8_text(path: &Path) -> Result<String, ToolError> {
    let bytes = tokio::fs::read(path).await?;
    let text = std::str::from_utf8(strip_utf8_bom(&bytes))
        .map_err(|err| ToolError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))?;
    Ok(text.to_string())
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM_BYTES).unwrap_or(bytes)
}

fn file_footer(file: &LimitedLines, last: usize) -> String {
    let next = last + 1;
    if file.cut {
        format!(
            "(Output capped at {MAX_BYTES_LABEL}. Showing lines {}-{last}. Use offset={next} to continue.)",
            file.offset
        )
    } else if file.more {
        format!(
            "(Showing lines {}-{last} of {}. Use offset={next} to continue.)",
            file.offset, file.count
        )
    } else {
        format!("(End of file - total {} lines)", file.count)
    }
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}
