use std::path::Path;

use similar::{ChangeTag, TextDiff};

use crate::lsp_path::display_path;

pub(crate) struct FileDiff {
    pub patch: String,
    pub additions: usize,
    pub deletions: usize,
}

pub(crate) fn create(path: &Path, content_old: &str, content_new: &str) -> FileDiff {
    let diff = TextDiff::from_lines(content_old, content_new);
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => deletions += change_line_count(change.value()),
            ChangeTag::Insert => additions += change_line_count(change.value()),
            ChangeTag::Equal => {}
        }
    }
    let path = display_path(path);
    FileDiff {
        patch: diff.unified_diff().header(&path, &path).to_string(),
        additions,
        deletions,
    }
}

fn change_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.lines().count().max(1)
}
