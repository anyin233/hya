//! Independent updater CLI (no runtime/plugin/MCP/bundle dependencies).
//!
//! Production activation requires an explicit `--owner-authorized-activation`
//! flag. Signatures alone never switch the selector.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use hya_updater::{
    ApplyOptions, ReleaseMetadata, TrustRoot, UPDATER_PACKAGE_VERSION, apply_update,
    discard_staged_release, layout, load_trust_roots, read_selector, recover_activation,
    write_trust_roots,
};

#[derive(Parser)]
#[command(
    name = "hya-updater",
    version,
    about = "Independent hya release verifier/activator TCB"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print package version and supported metadata protocol.
    Version,
    /// Show selector, accepted floor, and layout paths for an updater root.
    Status {
        /// Updater root directory (holds current, accepted_floor, journal, releases/).
        #[arg(long)]
        root: PathBuf,
    },
    /// Recover interrupted prepare/commit journal state.
    Recover {
        #[arg(long)]
        root: PathBuf,
    },
    /// Verify, fetch from a local package dir, stage, optional smoke, optional activate.
    Apply {
        #[arg(long)]
        root: PathBuf,
        /// Path to signed release metadata JSON.
        #[arg(long)]
        metadata: PathBuf,
        /// Local package directory (or file:// URL) containing named artifacts.
        #[arg(long)]
        package: PathBuf,
        /// Host platform triple (must match metadata.platform).
        #[arg(long)]
        platform: String,
        /// Optional relative smoke command under the staged release.
        #[arg(long)]
        smoke: Option<String>,
        /// Explicitly authorize selector/floor advance. Required for activation.
        #[arg(long)]
        owner_authorized_activation: bool,
        /// Override trust roots path (default: <root>/trust_roots.json).
        #[arg(long)]
        trust_roots: Option<PathBuf>,
    },
    /// Discard a staged-but-not-accepted candidate sequence.
    Discard {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        sequence: u64,
    },
    /// Write a bootstrap trust_roots.json (operator only).
    InitRoots {
        #[arg(long)]
        path: PathBuf,
        /// key_id=hex32 verifying key pairs (repeatable).
        #[arg(long = "root", value_name = "KEY_ID=HEX32", required = true)]
        roots: Vec<String>,
    },
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hya-updater: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Version => {
            println!(
                "hya-updater {} protocol {}",
                UPDATER_PACKAGE_VERSION,
                hya_updater::SUPPORTED_PROTOCOL_VERSION
            );
            Ok(())
        }
        Command::Status { root } => {
            let selector = read_selector(&root).map_err(|e| e.to_string())?;
            let layout = layout(&root);
            println!("root={}", root.display());
            println!("current_sequence={}", selector.current_sequence);
            println!("accepted_floor={}", selector.accepted_floor);
            println!("trust_roots={}", layout.trust_roots.display());
            println!("journal={}", layout.journal.display());
            println!("selector={}", layout.selector.display());
            println!("releases={}", layout.releases.display());
            Ok(())
        }
        Command::Recover { root } => {
            let selector = recover_activation(&root).map_err(|e| e.to_string())?;
            println!(
                "recovered current={} floor={}",
                selector.current_sequence, selector.accepted_floor
            );
            Ok(())
        }
        Command::Apply {
            root,
            metadata,
            package,
            platform,
            smoke,
            owner_authorized_activation,
            trust_roots,
        } => {
            let text = fs::read_to_string(&metadata)
                .map_err(|e| format!("read metadata {}: {e}", metadata.display()))?;
            let meta: ReleaseMetadata =
                serde_json::from_str(&text).map_err(|e| format!("parse metadata: {e}"))?;
            let roots = if let Some(path) = trust_roots {
                Some(load_trust_roots(&path).map_err(|e| e.to_string())?)
            } else {
                None
            };
            let package_source = package.to_string_lossy().into_owned();
            let smoke_ref = smoke.as_deref();
            let result = apply_update(ApplyOptions {
                updater_root: &root,
                metadata: &meta,
                package_source: &package_source,
                trust_roots: roots.as_deref(),
                host_platform: &platform,
                now_unix: now_unix(),
                smoke_command: smoke_ref,
                smoke_args: &[],
                owner_authorized: owner_authorized_activation,
            })
            .map_err(|e| e.to_string())?;
            println!("staged_sequence={}", result.staged.sequence);
            println!("staged_dir={}", result.staged.directory().display());
            match result.activated {
                Some(sel) => {
                    println!(
                        "activated current={} floor={}",
                        sel.current_sequence, sel.accepted_floor
                    );
                }
                None => {
                    println!(
                        "staged_only=true (pass --owner-authorized-activation to commit selector)"
                    );
                }
            }
            Ok(())
        }
        Command::Discard { root, sequence } => {
            discard_staged_release(&root, sequence).map_err(|e| e.to_string())?;
            println!("discarded sequence={sequence}");
            Ok(())
        }
        Command::InitRoots { path, roots } => {
            let mut parsed = Vec::new();
            for entry in roots {
                let (key_id, hex) = entry
                    .split_once('=')
                    .ok_or_else(|| format!("--root expects KEY_ID=HEX32, got `{entry}`"))?;
                if hex.len() != 64 {
                    return Err(format!("verifying key for `{key_id}` must be 64 hex chars"));
                }
                let mut key = [0u8; 32];
                for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
                    let s = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
                    key[i] = u8::from_str_radix(s, 16)
                        .map_err(|e| format!("invalid hex in key `{key_id}`: {e}"))?;
                }
                parsed.push(TrustRoot {
                    key_id: key_id.to_string(),
                    verifying_key: key,
                });
            }
            write_trust_roots(&path, &parsed).map_err(|e| e.to_string())?;
            println!("wrote {}", path.display());
            Ok(())
        }
    }
}
