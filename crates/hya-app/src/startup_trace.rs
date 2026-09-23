//! Opt-in startup phase marks (`HYA_STARTUP_TRACE=1|true`).
//!
//! Each mark is one newline-delimited JSON object on stderr:
//! `{"hya_startup":true,"mark":"<name>","wall_ms":<unix millis>[,"detail":"<text>"]}`.
//! `cargo run -p xtask -- startup-bench` parses them into a per-phase waterfall.
//! Marks never change behavior; with the variable unset they cost one
//! environment lookup.

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
    if !enabled() {
        return;
    }
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    match detail {
        Some(detail) => {
            let escaped = detail.replace('\\', "\\\\").replace('"', "\\\"");
            eprintln!(
                r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms},"detail":"{escaped}"}}"#
            );
        }
        None => eprintln!(r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms}}}"#),
    }
}
