//! Shared helpers for the `xtask` integration tests.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Create a uniquely-named temp directory for one test, tagged with `label`.
pub fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-xtask-{label}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Write a minimal valid `SKILL.md` under `root/name` with the given description.
pub fn write_skill(root: &Path, name: &str, description: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
    )
    .expect("write skill");
}
