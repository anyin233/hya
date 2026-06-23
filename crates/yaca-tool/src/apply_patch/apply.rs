use std::path::Path;

use crate::apply_patch::parse::{Hunk, UpdateChunk};
use crate::tool::ToolError;
use crate::utf8_bom;

#[derive(Clone, Copy)]
pub(crate) enum FileAction {
    Add,
    Modify,
    Delete,
}

impl FileAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FileAction::Add => "add",
            FileAction::Modify => "modify",
            FileAction::Delete => "delete",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            FileAction::Add => "A",
            FileAction::Modify => "M",
            FileAction::Delete => "D",
        }
    }
}

pub(crate) struct FileSummary {
    pub path: String,
    pub action: FileAction,
    pub additions: usize,
    pub deletions: usize,
    pub bom: bool,
}

impl FileSummary {
    pub(crate) fn line(&self) -> String {
        format!("{} {}", self.action.marker(), self.path)
    }
}

pub(crate) async fn apply_hunk(workdir: &Path, hunk: Hunk) -> Result<FileSummary, ToolError> {
    match hunk {
        Hunk::Add { path, contents } => {
            let target = workdir.join(&path);
            let (bom, contents) = utf8_bom::split(&contents);
            write_with_parent(&target, contents, bom).await?;
            Ok(FileSummary {
                path,
                action: FileAction::Add,
                additions: contents.lines().count(),
                deletions: 0,
                bom,
            })
        }
        Hunk::Delete { path } => {
            let target = workdir.join(&path);
            let (bom, old) = utf8_bom::read_text(&target).await?;
            tokio::fs::remove_file(&target).await?;
            Ok(FileSummary {
                path,
                action: FileAction::Delete,
                additions: 0,
                deletions: old.lines().count(),
                bom,
            })
        }
        Hunk::Update {
            path,
            move_path,
            chunks,
        } => {
            let target = workdir.join(&path);
            let (source_has_bom, old) = utf8_bom::read_text(&target).await?;
            let new = derive_new_contents(&path, &old, source_has_bom, &chunks)?;
            let additions = count_added_lines(&old, &new.text);
            let deletions = count_deleted_lines(&old, &new.text);
            let final_path = move_path.as_deref().unwrap_or(&path);
            let final_target = workdir.join(final_path);
            write_with_parent(&final_target, &new.text, new.bom).await?;
            if move_path.is_some() && final_target != target {
                tokio::fs::remove_file(&target).await?;
            }
            Ok(FileSummary {
                path: final_path.to_string(),
                action: FileAction::Modify,
                additions,
                deletions,
                bom: new.bom,
            })
        }
    }
}

async fn write_with_parent(path: &Path, content: &str, bom: bool) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, utf8_bom::encode(content, bom)).await?;
    Ok(())
}

struct NewContent {
    text: String,
    bom: bool,
}

fn derive_new_contents(
    file_path: &str,
    original: &str,
    source_has_bom: bool,
    chunks: &[UpdateChunk],
) -> Result<NewContent, ToolError> {
    if chunks.is_empty() {
        return Ok(NewContent {
            text: ensure_trailing_newline(original),
            bom: source_has_bom,
        });
    }
    let mut lines = split_content_lines(original);
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let context_lines = [context.clone()];
            let Some(context_index) = seek_sequence(&lines, &context_lines, line_index, false)
            else {
                return Err(ToolError::Other(format!(
                    "failed to find context '{context}' in {file_path}"
                )));
            };
            line_index = context_index + 1;
        }

        if chunk.old_lines.is_empty() {
            replacements.push((line_index.min(lines.len()), 0usize, chunk.new_lines.clone()));
            continue;
        }

        let Some(found) = seek_sequence(&lines, &chunk.old_lines, line_index, chunk.end_of_file)
        else {
            return Err(ToolError::Other(format!(
                "failed to find expected lines in {file_path}:\n{}",
                chunk.old_lines.join("\n")
            )));
        };
        replacements.push((found, chunk.old_lines.len(), chunk.new_lines.clone()));
        line_index = found + chunk.old_lines.len();
    }

    replacements.sort_by_key(|(start, _, _)| *start);
    for (start, delete_count, new_lines) in replacements.into_iter().rev() {
        lines.splice(start..start + delete_count, new_lines);
    }
    let formatted = format_lines(&lines);
    let (incoming_has_bom, text) = utf8_bom::split(&formatted);
    Ok(NewContent {
        text: text.to_string(),
        bom: source_has_bom || incoming_has_bom,
    })
}

fn split_content_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = content.split('\n').map(ToString::to_string).collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn format_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn ensure_trailing_newline(content: &str) -> String {
    if content.is_empty() || content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    }
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    end_of_file: bool,
) -> Option<usize> {
    find_sequence(lines, pattern, start, end_of_file, |a, b| a == b)
        .or_else(|| {
            find_sequence(lines, pattern, start, end_of_file, |a, b| {
                a.trim_end() == b.trim_end()
            })
        })
        .or_else(|| {
            find_sequence(lines, pattern, start, end_of_file, |a, b| {
                a.trim() == b.trim()
            })
        })
}

fn find_sequence<F>(
    lines: &[String],
    pattern: &[String],
    start: usize,
    end_of_file: bool,
    mut matches: F,
) -> Option<usize>
where
    F: FnMut(&str, &str) -> bool,
{
    if pattern.is_empty() || pattern.len() > lines.len() {
        return None;
    }
    if end_of_file {
        let from_end = lines.len() - pattern.len();
        if from_end >= start && sequence_matches(lines, pattern, from_end, &mut matches) {
            return Some(from_end);
        }
    }
    (start..=lines.len() - pattern.len())
        .find(|candidate| sequence_matches(lines, pattern, *candidate, &mut matches))
}

fn sequence_matches<F>(lines: &[String], pattern: &[String], start: usize, matches: &mut F) -> bool
where
    F: FnMut(&str, &str) -> bool,
{
    pattern
        .iter()
        .enumerate()
        .all(|(offset, expected)| matches(&lines[start + offset], expected))
}

fn count_added_lines(old: &str, new: &str) -> usize {
    new.lines().count().saturating_sub(old.lines().count())
}

fn count_deleted_lines(old: &str, new: &str) -> usize {
    old.lines().count().saturating_sub(new.lines().count())
}
