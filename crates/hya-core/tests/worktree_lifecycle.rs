//! Integration tests for `hya-core`: tmux capability.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use hya_core::TmuxPaneManager;

#[tokio::test]
async fn tmux_capability_probe_and_degrade() {
    let available = TmuxPaneManager::available().await;
    if !available {
        let mgr = TmuxPaneManager::new("hya-test");
        let result = mgr.open(Path::new("/tmp"), "true").await;
        assert!(result.is_err(), "tmux open must error when tmux is absent");
    }
}
