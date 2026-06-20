//! Dev tooling entrypoint (e.g. `cargo xtask migrate`). Phase 0 scaffold.
fn main() {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("migrate") => eprintln!("xtask migrate: not yet implemented (Phase 1)"),
        _ => eprintln!("usage: cargo xtask <migrate>"),
    }
}
