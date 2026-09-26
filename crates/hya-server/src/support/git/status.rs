use std::path::Path;

use crate::ApiError;

use super::{GitItem, status_name, text};

pub(super) fn items(workdir: &Path) -> Result<Vec<GitItem>, ApiError> {
    items_in(workdir, &[".".to_owned()])
}

/// Status items restricted to `pathspecs` (relative to `workdir`).
pub(super) fn items_in(workdir: &Path, pathspecs: &[String]) -> Result<Vec<GitItem>, ApiError> {
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "-uall",
        "--no-renames",
        "-z",
        "--",
    ];
    args.extend(pathspecs.iter().map(String::as_str));
    let out = text(workdir, &args)?;
    let mut items: Vec<_> = out
        .split('\0')
        .filter(|line| !line.is_empty())
        .filter_map(item_from_line)
        .collect();
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
