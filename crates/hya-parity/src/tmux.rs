//! tmux driver: launch the binary in a real terminal and capture frames.
//!
//! Skeleton for W0; the full driver (send-keys scripting, resize, side-by-side TS-vs-Rust
//! panes) lands in W5/W10. Kept minimal but real so callers have a stable entry point.

use std::process::Command;

/// Capture the current contents of a tmux pane (`tmux capture-pane -pt <session>`).
///
/// # Errors
/// Returns an error string if tmux is unavailable or the capture command fails.
pub fn capture_pane(session: &str) -> Result<String, String> {
    let out = Command::new("tmux")
        .args(["capture-pane", "-pt", session])
        .output()
        .map_err(|e| format!("tmux not available: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tmux capture-pane failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
