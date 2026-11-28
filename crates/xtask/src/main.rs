//! Repository automation for a workspace checkout. Run
//! `cargo run -p xtask -- --help` to list the available tasks.

mod boundary;
mod first_party_release;
mod gen_api;
mod gen_plugin;
mod gen_relay;
mod lifecycle_bench;
mod matrix_check;
mod package_bundle;
mod release_rehearsal;
mod release_staging;
mod rings;
mod startup_bench;

type Task = fn(Vec<String>) -> anyhow::Result<()>;

const TASKS: &[(&str, Task)] = &[
    ("boundary-check", boundary::run),
    ("ring-report", boundary::ring_report),
    ("startup-bench", startup_bench::run),
    ("lifecycle-bench", lifecycle_bench::run),
    ("matrix-check", matrix_check::run),
    ("package-bundle", package_bundle::run),
    (
        "package-native-tool-library",
        package_bundle::run_native_library,
    ),
    ("stage-first-party-bundles", first_party_release::run),
    ("stage-release", release_staging::run),
    ("stage-bun-programs", release_staging::run_bun_programs),
    ("release-rehearsal", release_rehearsal::run),
    ("gen-api", gen_api::run),
    ("gen-relay", gen_relay::run),
    ("gen-plugin", gen_plugin::run),
];

fn main() {
    let mut args = std::env::args().skip(1);
    let task = args.next();
    let Some((_, run)) = TASKS
        .iter()
        .find(|(name, _)| Some(*name) == task.as_deref())
    else {
        eprintln!("Usage: cargo run -p xtask -- <task> [args...]\n\nTasks:");
        for (name, _) in TASKS {
            eprintln!("  {name}");
        }
        return;
    };

    if let Err(error) = run(args.collect()) {
        eprintln!("xtask: {error:#}");
        std::process::exit(1);
    }
}
