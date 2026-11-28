//! Shared fixtures and helpers for `hya-tool` integration tests.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolCtx};
use tokio_util::sync::CancellationToken;

pub fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

pub fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

pub fn tempdir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-{tag}-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

pub struct TempDirGuard(pub PathBuf);

impl TempDirGuard {
    pub fn new(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    ToolCtx::new(workdir, permission, CancellationToken::new())
}
