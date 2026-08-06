//! Repository automation, run as `cargo xtask <task>`.
//!
//! These are maintenance tasks that need the workspace checked out, so they live
//! here rather than in CI config or a shell script:
//!
//! - `sync-compat` (alias `migrate`) — re-sync the vendored Compat/OpenCode
//!   adapter sources against their upstream pins.
//! - `startup-bench` — measure backend startup latency.
//! - `matrix-check` — verify the agent test matrix in `docs/testing/` still
//!   matches the scenarios the suites actually declare.
//!
//! An unknown or missing task prints usage and exits successfully, so the binary
//! is safe to invoke from a wrapper that does not know the task list.

mod matrix_check;
mod startup_bench;
mod sync_compat;

fn main() {
    let mut args = std::env::args();
    let _bin = args.next();
    let task = args.next();

    let result = match task.as_deref() {
        Some("sync-compat") => sync_compat::run(args.collect()),
        Some("migrate") => sync_compat::run(args.collect()),
        Some("startup-bench") => startup_bench::run(args.collect()),
        Some("matrix-check") => matrix_check::run(args.collect()),
        _ => {
            eprintln!("usage: cargo xtask <sync-compat|migrate|startup-bench|matrix-check>");
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("xtask: {error:#}");
        std::process::exit(1);
    }
}
