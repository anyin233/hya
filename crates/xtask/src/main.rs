//! Repository automation, run as `cargo xtask <task>`.
//!
//! These are maintenance tasks that need the workspace checked out, so they live
//! here rather than in CI config or a shell script:
//!
//! - `startup-bench` — measure backend startup latency.
//! - `matrix-check` — verify the agent test matrix in `docs/testing/` still
//!   matches the scenarios the suites actually declare.
//! - `package-bundle` — validate a source tree and emit deterministic public
//!   `.hyabundle` bytes through the shared package writer.
//! - `package-native-tool-bundle` — stage one built Rust tool-family executable
//!   into its policy source and emit a deterministic public package.
//!
//! - `release-rehearsal` — validate and smoke the non-publishing release asset.
//! - `gen-api` — regenerate the `hya.v1` contract crate and its docs from
//!   `proto/hya/v1` (Rust codegen + API reference + OpenAPI).
//!
//! An unknown or missing task prints usage and exits successfully, so the binary
//! is safe to invoke from a wrapper that does not know the task list.

mod gen_api;
mod matrix_check;
mod package_bundle;
mod release_rehearsal;
mod startup_bench;

fn main() {
    let mut args = std::env::args();
    let _bin = args.next();
    let task = args.next();

    let result = match task.as_deref() {
        Some("package-bundle") => package_bundle::run(args.collect()),
        Some("package-native-tool-bundle") => package_bundle::run_native(args.collect()),
        Some("release-rehearsal") => release_rehearsal::run(args.collect()),
        Some("startup-bench") => startup_bench::run(args.collect()),
        Some("matrix-check") => matrix_check::run(args.collect()),
        Some("gen-api") => gen_api::run(args.collect()),
        _ => {
            eprintln!(
                "usage: cargo xtask <startup-bench|matrix-check|package-bundle|package-native-tool-bundle|release-rehearsal|gen-api>"
            );
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("xtask: {error:#}");
        std::process::exit(1);
    }
}
