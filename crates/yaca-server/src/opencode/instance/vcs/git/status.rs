use std::path::Path;

use crate::ApiError;

use super::{GitItem, status_name, text};

pub(super) fn items(workdir: &Path) -> Result<Vec<GitItem>, ApiError> {
    let out = text(
        workdir,
        &["status", "--porcelain=v1", "-uall", "--no-renames"],
    )?;
    let mut items: Vec<_> = out.lines().filter_map(item_from_line).collect();
    items.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(items)
}

fn item_from_line(line: &str) -> Option<GitItem> {
    let code = line.get(0..2)?;
    Some(GitItem {
        file: line.get(3..)?.to_string(),
        code: code.to_string(),
        status: status_name(code),
    })
}
