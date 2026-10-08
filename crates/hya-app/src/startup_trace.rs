//! Opt-in startup phase marks (`HYA_STARTUP_TRACE=1|true`, or a trace file).
//!
//! Each mark is one newline-delimited JSON object on stderr:
//! `{"hya_startup":true,"mark":"<name>","wall_ms":<unix millis>[,"detail":"<text>"]}`.
//! `cargo run -p xtask -- startup-bench` parses them into a per-phase waterfall.
//! Marks never change behavior; with the variable unset they cost one
//! environment lookup per opt-in variable. `HYA_STARTUP_TRACE_FILE` appends the
//! same JSONL to a file shared with frontend processes.

/// Whether `HYA_STARTUP_TRACE` is `1` or `true` (case-insensitive).
#[must_use]
pub fn enabled() -> bool {
    std::env::var_os("HYA_STARTUP_TRACE")
        .map(|value| {
            let text = value.to_string_lossy();
            text.eq_ignore_ascii_case("1") || text.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// Emit one startup mark when tracing is enabled.
pub fn mark(mark: &str, detail: Option<&str>) {
    let file = std::env::var_os("HYA_STARTUP_TRACE_FILE");
    if !enabled() && file.is_none() {
        return;
    }
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let mut value = serde_json::json!({ "hya_startup": true, "mark": mark, "wall_ms": wall_ms, "pid": std::process::id() });
    if let Some(detail) = detail {
        value["detail"] = serde_json::Value::String(detail.to_owned());
    }
    let line = value.to_string();
    if enabled() {
        eprintln!("{line}");
    }
    if let Some(path) = file {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
    }
}
