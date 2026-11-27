//! Shared helpers for the `xtask` integration tests.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Resolve the `xtask` binary path, preferring nextest's runtime-remapped
/// `NEXTEST_BIN_EXE_xtask` so archived test runs work across directories.
pub fn xtask_bin() -> String {
    std::env::var("NEXTEST_BIN_EXE_xtask")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_xtask"))
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_xtask").to_owned())
}

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
