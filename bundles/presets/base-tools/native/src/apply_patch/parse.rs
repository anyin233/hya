#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UpdateChunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub context: Option<String>,
    pub end_of_file: bool,
}

impl Hunk {
    pub(crate) fn path(&self) -> &str {
        match self {
            Hunk::Add { path, .. } | Hunk::Delete { path } | Hunk::Update { path, .. } => path,
        }
    }

    pub(crate) fn move_path(&self) -> Option<&str> {
        match self {
            Hunk::Update { move_path, .. } => move_path.as_deref(),
            Hunk::Add { .. } | Hunk::Delete { .. } => None,
        }
    }
}

pub(crate) fn parse_patch(patch_text: &str) -> Result<Vec<Hunk>, String> {
    let cleaned = patch_text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = cleaned.lines().collect();
    let begin = lines
        .iter()
        .position(|line| line.trim() == "*** Begin Patch")
        .ok_or_else(|| "invalid patch format: missing Begin marker".to_string())?;
    let end = lines
        .iter()
        .position(|line| line.trim() == "*** End Patch")
        .ok_or_else(|| "invalid patch format: missing End marker".to_string())?;
    if begin >= end {
        return Err("invalid patch format: Begin marker must precede End marker".to_string());
    }

    let mut hunks = Vec::new();
    let mut i = begin + 1;
    while i < end {
        let line = lines[i];
        if let Some(path) = header_value(line, "*** Add File:") {
            let (contents, next) = parse_add_content(&lines, i + 1, end)?;
            hunks.push(Hunk::Add { path, contents });
            i = next;
        } else if let Some(path) = header_value(line, "*** Delete File:") {
            hunks.push(Hunk::Delete { path });
            i += 1;
        } else if let Some(path) = header_value(line, "*** Update File:") {
            let (move_path, body_start) = if i + 1 < end && lines[i + 1].starts_with("*** Move to:")
            {
                (header_value(lines[i + 1], "*** Move to:"), i + 2)
            } else {
                (None, i + 1)
            };
            let (chunks, next) = parse_update_chunks(&lines, body_start, end);
            hunks.push(Hunk::Update {
                path,
                move_path,
                chunks,
            });
            i = next;
        } else {
            i += 1;
        }
    }
    Ok(hunks)
}

fn header_value(line: &str, prefix: &str) -> Option<String> {
    let value = line.strip_prefix(prefix)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_add_content(lines: &[&str], start: usize, end: usize) -> Result<(String, usize), String> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end && !is_file_header(lines[i]) {
        let line = lines[i];
        let Some(content) = line.strip_prefix('+') else {
            return Err("add file lines must start with '+'".to_string());
        };
        out.push(content.to_string());
        i += 1;
    }
    Ok((format_lines(&out), i))
}

fn parse_update_chunks(lines: &[&str], start: usize, end: usize) -> (Vec<UpdateChunk>, usize) {
    let mut chunks = Vec::new();
    let mut i = start;
    while i < end && !is_file_header(lines[i]) {
        if !lines[i].starts_with("@@") {
            i += 1;
            continue;
        }
        let raw_context = lines[i].trim_start_matches("@@").trim();
        let context = (!raw_context.is_empty()).then(|| raw_context.to_string());
        i += 1;

        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();
        let mut end_of_file = false;
        while i < end && !lines[i].starts_with("@@") && !is_file_header(lines[i]) {
            let line = lines[i];
            if line == "*** End of File" {
                end_of_file = true;
                i += 1;
                break;
            }
            if let Some(content) = line.strip_prefix(' ') {
                old_lines.push(content.to_string());
                new_lines.push(content.to_string());
            } else if let Some(content) = line.strip_prefix('-') {
                old_lines.push(content.to_string());
            } else if let Some(content) = line.strip_prefix('+') {
                new_lines.push(content.to_string());
            }
            i += 1;
        }
        chunks.push(UpdateChunk {
            old_lines,
            new_lines,
            context,
            end_of_file,
        });
    }
    (chunks, i)
}

fn is_file_header(line: &str) -> bool {
    line.starts_with("*** Add File:")
        || line.starts_with("*** Delete File:")
        || line.starts_with("*** Update File:")
        || line.trim() == "*** End Patch"
}

fn format_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}
