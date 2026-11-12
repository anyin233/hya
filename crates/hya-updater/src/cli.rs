//! `hya update` command surface.
use clap::Subcommand;
use std::io::Write;

#[derive(Debug, Subcommand)]
/// Subcommands for the `hya update` interface.
pub enum UpdateCommand {
    /// Print the updater package version.
    Version,
}

/// Execute one update subcommand and write its human-readable output.
pub fn run(command: UpdateCommand, out: &mut dyn Write) -> Result<(), String> {
    match command {
        UpdateCommand::Version => writeln!(out, "hya update {}", hya_version::BACKEND_VERSION)
            .map_err(|e| format!("write output: {e}")),
    }
}
