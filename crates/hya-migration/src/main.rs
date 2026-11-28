//! `hya-migration`: standalone v1-to-v2 migration and SHA-256 rollback CLI.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use hya_migration::bundle_v1_to_v2::{BundleCheckStatus, DiagnosticSeverity, RegistryCheckStatus};
use hya_migration::config_v1_to_v2::ConfigCheckStatus;
use hya_migration::db_v1_to_v2::DbCheckStatus;
use hya_migration::tui_v1_to_v2::TuiCheckStatus;
use hya_migration::{
    CheckReport, MigrationTargets, RunOptions, RunReport, check_all, rollback, run_migration,
};

#[derive(Debug, Parser)]
#[command(
    name = "hya-migration",
    version = hya_version::VERSION,
    about = "Standalone v1-to-v2 offline migration and SHA-256 rollback tool for hya"
)]
struct Cli {
    /// Emit machine-readable JSON output instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect sessions.db, config.yaml, tui.toml, tui.json, registry.sqlite3, and .hya/bundles/ without writing.
    Check(TargetArgs),
    /// Migrate v1 artifacts to v2 atomically after writing a SHA-256 backup manifest.
    Run {
        #[command(flatten)]
        targets: TargetArgs,
        /// Simulate all migrations and print planned actions without modifying any file.
        #[arg(long)]
        dry_run: bool,
        /// Directory where the SHA-256 backup manifest and snapshots will be stored.
        #[arg(long = "backup-dir", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
    /// Verify every file in a backup directory against its SHA-256 manifest, then atomically restore originals.
    Rollback {
        /// Backup snapshot directory containing `manifest.json`.
        #[arg(long = "backup", alias = "backup-dir", value_name = "DIR")]
        backup: PathBuf,
    },
}

#[derive(Debug, Args)]
struct TargetArgs {
    /// Path to `sessions.db`.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Path to `config.yaml`.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Path to `tui.toml`.
    #[arg(long = "tui-toml", value_name = "PATH")]
    tui_toml: Option<PathBuf>,
    /// Path to `tui.json`.
    #[arg(long = "tui-json", value_name = "PATH")]
    tui_json: Option<PathBuf>,
    /// Path to `registry.sqlite3`.
    #[arg(long, value_name = "PATH")]
    registry: Option<PathBuf>,
    /// Bundle directory (e.g. `.hya/bundles/`) to inspect or migrate (repeatable).
    #[arg(long = "bundles-dir", value_name = "DIR")]
    bundles_dirs: Vec<PathBuf>,
}

impl TargetArgs {
    fn resolve(self) -> MigrationTargets {
        let mut defaults = MigrationTargets::resolve_defaults();
        if let Some(db) = self.db {
            defaults.db = db;
        }
        if let Some(config) = self.config {
            let parent = config.parent().map(std::path::Path::to_path_buf);
            defaults.config = config;
            if self.tui_toml.is_none()
                && let Some(dir) = &parent
            {
                defaults.tui_toml = dir.join("tui.toml");
            }
            if self.tui_json.is_none()
                && let Some(dir) = &parent
            {
                defaults.tui_json = dir.join("tui.json");
            }
        }
        if let Some(tui_toml) = self.tui_toml {
            defaults.tui_toml = tui_toml;
        }
        if let Some(tui_json) = self.tui_json {
            defaults.tui_json = tui_json;
        }
        if let Some(registry) = self.registry {
            defaults.registry = registry;
        }
        if !self.bundles_dirs.is_empty() {
            defaults.bundles_dirs = self.bundles_dirs;
        }
        defaults
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hya-migration error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn execute(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Check(args) => {
            let targets = args.resolve();
            let report = check_all(&targets).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_check_report(&report);
            }
        }
        Command::Run {
            targets,
            dry_run,
            backup_dir,
        } => {
            let targets = targets.resolve();
            let options = RunOptions {
                dry_run,
                backup_dir,
            };
            let report = run_migration(&targets, &options).await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_run_report(&report);
            }
        }
        Command::Rollback { backup } => {
            let report = rollback(&backup)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Rollback complete from {} ({} verified, {} restored, {} created files removed)",
                    report.backup_dir.display(),
                    report.verified_files,
                    report.restored_files.len(),
                    report.removed_created_files.len()
                );
                for restored in &report.restored_files {
                    println!("  restored: {}", restored.display());
                }
                for removed in &report.removed_created_files {
                    println!("  removed:  {}", removed.display());
                }
            }
        }
    }
    Ok(())
}

fn print_check_report(report: &CheckReport) {
    println!("hya-migration check (read-only)");
    println!(
        "  sessions.db ({}): {:?}",
        report.db.path.display(),
        report.db.status
    );
    if report.db.status == DbCheckStatus::NeedsMigration {
        for reason in &report.db.reasons {
            println!("    - {reason}");
        }
    }
    println!(
        "  config.yaml ({}): {:?}",
        report.config.path.display(),
        report.config.status
    );
    if report.config.status == ConfigCheckStatus::NeedsMigration {
        for reason in &report.config.reasons {
            println!("    - {reason}");
        }
    }
    println!(
        "  tui.toml    ({}): {:?}",
        report.tui.tui_toml_path.display(),
        report.tui.tui_toml_status
    );
    if report.tui.tui_toml_status == TuiCheckStatus::NeedsMigration {
        for reason in &report.tui.tui_toml_reasons {
            println!("    - {reason}");
        }
    }
    println!(
        "  tui.json    ({}): {:?}",
        report.tui.tui_json_path.display(),
        report.tui.tui_json_status
    );
    if report.tui.tui_json_status == TuiCheckStatus::NeedsMigration {
        for reason in &report.tui.tui_json_reasons {
            println!("    - {reason}");
        }
    }
    println!(
        "  registry    ({}): {:?}",
        report.bundles.registry.path.display(),
        report.bundles.registry.status
    );
    if report.bundles.registry.status == RegistryCheckStatus::NeedsMigration {
        for diag in &report.bundles.registry.diagnostics {
            println!(
                "    - [{:?}] {}: {}",
                diag.severity, diag.target, diag.message
            );
        }
    }
    for manifest in &report.bundles.manifests {
        println!(
            "  bundle      ({}): {:?}",
            manifest.manifest_path.display(),
            manifest.status
        );
        if manifest.status == BundleCheckStatus::NeedsMigration {
            for reason in &manifest.reasons {
                println!("    - {reason}");
            }
        }
        for diag in &manifest.diagnostics {
            let tag = match diag.severity {
                DiagnosticSeverity::Info => "info",
                DiagnosticSeverity::Warning => "warn",
                DiagnosticSeverity::Error => "error",
            };
            println!("    - [{tag}] {}: {}", diag.target, diag.message);
        }
    }
    if report.needs_migration {
        println!("Result: v1 artifacts detected; run `hya-migration run` to migrate.");
    } else {
        println!("Result: all inspected artifacts are ready for hya v2.");
    }
}

fn print_run_report(report: &RunReport) {
    if report.dry_run {
        println!("hya-migration run (--dry-run, no files modified)");
    } else {
        println!("hya-migration run complete");
    }
    if let Some(dir) = &report.backup_dir {
        println!("  backup:   {}", dir.display());
    }
    println!(
        "  sessions.db: migrated={}, sessions={}, events={}, rewritten={}, claims={}, integrity_ok={}, store_verified={}",
        report.db.migrated,
        report.db.sessions_copied,
        report.db.events_copied,
        report.db.events_rewritten,
        report.db.claims_migrated,
        report.db.integrity_ok,
        report.db.store_verified
    );
    for note in &report.db.notes {
        println!("    - {note}");
    }
    println!(
        "  config.yaml: migrated={}, bundle_configs={}",
        report.config.migrated,
        report.config.bundle_configs_written.len()
    );
    for note in &report.config.notes {
        println!("    - {note}");
    }
    println!(
        "  tui:         tui_toml_migrated={}, tui_json_migrated={}",
        report.tui.tui_toml_migrated, report.tui.tui_json_migrated
    );
    for note in &report.tui.notes {
        println!("    - {note}");
    }
    println!(
        "  bundles:     manifests_migrated={}, registry_migrated={}, disabled_remapped={}, rows_upgraded={}, needs_reinstall={}",
        report.bundles.manifests_migrated.len(),
        report.bundles.registry_migrated,
        report.bundles.registry_disabled_remapped,
        report.bundles.registry_rows_upgraded,
        report.bundles.registry_rows_needing_reinstall
    );
    for diag in &report.bundles.diagnostics {
        println!(
            "    - [{:?}] {}: {}",
            diag.severity, diag.target, diag.message
        );
    }
}
