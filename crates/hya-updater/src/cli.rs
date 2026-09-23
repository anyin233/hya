//! `hya update` — the command-line surface of the update TCB.
//!
//! The unified `hya` executable dispatches `hya update …` here before any
//! runtime composition (config, bundles, providers, plugins, MCP, or session
//! storage), so these commands share only this crate's dependency-free trust
//! boundary. Production activation still requires an explicit
//! `--owner-authorized-activation` flag; signatures alone never switch the
//! selector.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Subcommand;

use crate::{
    ApplyOptions, ReleaseMetadata, TrustRoot, UPDATER_PACKAGE_VERSION, apply_update,
    discard_staged_release, layout, load_trust_roots, read_selector, recover_activation,
    write_trust_roots,
};

/// `hya update` subcommands.
#[derive(Debug, Subcommand)]
pub enum UpdateCommand {
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
        /// Updater root directory.
        #[arg(long)]
        root: PathBuf,
    },
    /// Verify, fetch from a local package dir, stage, optional smoke, optional activate.
    Apply {
        /// Updater root directory.
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
        /// Updater root directory.
        #[arg(long)]
        root: PathBuf,
        /// Staged candidate sequence to discard.
        #[arg(long)]
        sequence: u64,
    },
    /// Write a bootstrap trust_roots.json (operator only).
    InitRoots {
        /// Destination `trust_roots.json` path.
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

/// Run one `hya update` subcommand, writing its report lines to `out`.
///
/// # Errors
/// Returns a human-readable message when a step fails; the updater root is
/// left in the state the library pipeline guarantees for that failure.
pub fn run(command: UpdateCommand, out: &mut dyn Write) -> Result<(), String> {
    let lines = match command {
        UpdateCommand::Version => vec![format!(
            "hya update {UPDATER_PACKAGE_VERSION} protocol {}",
            crate::SUPPORTED_PROTOCOL_VERSION
        )],
        UpdateCommand::Status { root } => {
            let selector = read_selector(&root).map_err(|e| e.to_string())?;
            let layout = layout(&root);
            vec![
                format!("root={}", root.display()),
                format!("current_sequence={}", selector.current_sequence),
                format!("accepted_floor={}", selector.accepted_floor),
                format!("trust_roots={}", layout.trust_roots.display()),
                format!("journal={}", layout.journal.display()),
                format!("selector={}", layout.selector.display()),
                format!("releases={}", layout.releases.display()),
            ]
        }
        UpdateCommand::Recover { root } => {
            let selector = recover_activation(&root).map_err(|e| e.to_string())?;
            vec![format!(
                "recovered current={} floor={}",
                selector.current_sequence, selector.accepted_floor
            )]
        }
        UpdateCommand::Apply {
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
            let result = apply_update(ApplyOptions {
                updater_root: &root,
                metadata: &meta,
                package_source: &package_source,
                trust_roots: roots.as_deref(),
                host_platform: &platform,
                now_unix: now_unix(),
                smoke_command: smoke.as_deref(),
                smoke_args: &[],
                owner_authorized: owner_authorized_activation,
            })
            .map_err(|e| e.to_string())?;
            let mut lines = vec![
                format!("staged_sequence={}", result.staged.sequence),
                format!("staged_dir={}", result.staged.directory().display()),
            ];
            lines.push(match result.activated {
                Some(sel) => format!(
                    "activated current={} floor={}",
                    sel.current_sequence, sel.accepted_floor
                ),
                None => "staged_only=true (pass --owner-authorized-activation to commit selector)"
                    .to_string(),
            });
            lines
        }
        UpdateCommand::Discard { root, sequence } => {
            discard_staged_release(&root, sequence).map_err(|e| e.to_string())?;
            vec![format!("discarded sequence={sequence}")]
        }
        UpdateCommand::InitRoots { path, roots } => {
            let parsed = roots
                .iter()
                .map(|entry| parse_trust_root(entry))
                .collect::<Result<Vec<_>, _>>()?;
            write_trust_roots(&path, &parsed).map_err(|e| e.to_string())?;
            vec![format!("wrote {}", path.display())]
        }
    };
    for line in lines {
        writeln!(out, "{line}").map_err(|e| format!("write output: {e}"))?;
    }
    Ok(())
}

fn parse_trust_root(entry: &str) -> Result<TrustRoot, String> {
    let (key_id, hex) = entry
        .split_once('=')
        .ok_or_else(|| format!("--root expects KEY_ID=HEX32, got `{entry}`"))?;
    if hex.len() != 64 {
        return Err(format!("verifying key for `{key_id}` must be 64 hex chars"));
    }
    let mut key = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        key[i] =
            u8::from_str_radix(s, 16).map_err(|e| format!("invalid hex in key `{key_id}`: {e}"))?;
    }
    Ok(TrustRoot {
        key_id: key_id.to_string(),
        verifying_key: key,
    })
}
