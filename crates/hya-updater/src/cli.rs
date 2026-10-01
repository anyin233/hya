//! `hya update` command surface.
use crate::{
    ApplyOptions, ReleaseMetadata, TrustRoot, UPDATER_PACKAGE_VERSION, UpdaterOwner, apply_update,
    discard_staged_release, layout, load_trust_roots, read_selector, recover_activation,
    write_trust_roots,
};
use clap::Subcommand;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Subcommand)]
/// Subcommands for the `hya update` interface.
pub enum UpdateCommand {
    /// Install the published frontend runtime (TUI and WebUI) beside the backend.
    Tui {
        /// Install this frontend release instead of the latest one.
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Reinstall even when that frontend version is already installed.
        #[arg(long)]
        force: bool,
        /// Install prefix; defaults to the running hya's prefix.
        #[arg(long, value_name = "DIR")]
        prefix: Option<PathBuf>,
    },
    /// Print the updater protocol and package version.
    Version,
    /// Print the active selector and updater-root paths.
    Status {
        /// Command argument `root`.
        #[arg(long)]
        root: PathBuf,
    },
    /// Recover an interrupted activation journal.
    Recover {
        /// Command argument `root`.
        #[arg(long)]
        root: PathBuf,
    },
    /// Verify and stage a package, optionally activating an issued capability.
    Apply {
        /// Command argument `root`.
        #[arg(long)]
        root: PathBuf,
        /// Command argument `metadata`.
        #[arg(long)]
        metadata: PathBuf,
        /// Command argument `package`.
        #[arg(long)]
        package: PathBuf,
        /// Command argument `platform`.
        #[arg(long)]
        platform: String,
        /// Command argument `smoke`.
        #[arg(long)]
        smoke: Option<String>,
        /// Capability JSON written by the trusted owner, bound to candidate and expected generation.
        #[arg(long)]
        authorization: Option<PathBuf>,
        /// Command argument `trust_roots`.
        #[arg(long)]
        trust_roots: Option<PathBuf>,
    },
    /// Owner only: authorize activating release `sequence` over the active
    /// generation and write the capability `apply --authorization` needs.
    /// Asks for confirmation at a terminal; unattended it requires `--yes`.
    Authorize {
        /// Updater root.
        #[arg(long)]
        root: PathBuf,
        /// Candidate release sequence the capability is bound to.
        #[arg(long)]
        sequence: u64,
        /// Where to write the capability JSON.
        #[arg(long)]
        out: PathBuf,
        /// Skip the confirmation (the caller is the owner's supervisor).
        #[arg(long)]
        yes: bool,
    },
    /// Remove a staged, unaccepted release.
    Discard {
        /// Command argument `root`.
        #[arg(long)]
        root: PathBuf,
        /// Command argument `sequence`.
        #[arg(long)]
        sequence: u64,
    },
    /// Write trusted verification roots.
    InitRoots {
        /// Command argument `path`.
        #[arg(long)]
        path: PathBuf,
        /// `KEY_ID=HEX32` trusted-root entries.
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

/// Execute one update subcommand and write its human-readable output.
pub fn run(command: UpdateCommand, out: &mut dyn Write) -> Result<(), String> {
    let lines = match command {
        UpdateCommand::Tui { .. } => {
            return Err("hya update tui must be dispatched by the backend".into());
        }
        UpdateCommand::Version => vec![format!(
            "hya update {UPDATER_PACKAGE_VERSION} protocol {}",
            crate::SUPPORTED_PROTOCOL_VERSION
        )],
        UpdateCommand::Status { root } => {
            let selector = read_selector(&root).map_err(|e| e.to_string())?;
            let l = layout(&root);
            vec![
                format!("root={}", root.display()),
                format!("current_sequence={}", selector.current_sequence),
                format!("accepted_floor={}", selector.accepted_floor),
                format!("generation={}", selector.generation),
                format!("trust_roots={}", l.trust_roots.display()),
                format!("journal={}", l.journal.display()),
                format!("selector={}", l.selector.display()),
                format!("releases={}", l.releases.display()),
            ]
        }
        UpdateCommand::Recover { root } => {
            let selector = recover_activation(&root).map_err(|e| e.to_string())?;
            vec![format!(
                "recovered current={} floor={} generation={}",
                selector.current_sequence, selector.accepted_floor, selector.generation
            )]
        }
        UpdateCommand::Apply {
            root,
            metadata,
            package,
            platform,
            smoke,
            authorization,
            trust_roots,
        } => {
            let text = fs::read_to_string(&metadata)
                .map_err(|e| format!("read metadata {}: {e}", metadata.display()))?;
            let meta: ReleaseMetadata =
                serde_json::from_str(&text).map_err(|e| format!("parse metadata: {e}"))?;
            let roots = trust_roots
                .map(|path| load_trust_roots(&path).map_err(|e| e.to_string()))
                .transpose()?;
            let authorization = authorization
                .map(|path| {
                    fs::read_to_string(&path)
                        .map_err(|e| format!("read authorization {}: {e}", path.display()))
                        .and_then(|text| {
                            serde_json::from_str(&text)
                                .map_err(|e| format!("parse authorization: {e}"))
                        })
                })
                .transpose()?;
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
                activation: authorization.as_ref(),
            })
            .map_err(|e| e.to_string())?;
            let mut lines = vec![
                format!("staged_sequence={}", result.staged.sequence),
                format!("staged_dir={}", result.staged.directory().display()),
            ];
            lines.push(match result.activated {
                Some(sel) => format!(
                    "activated current={} floor={} generation={}",
                    sel.current_sequence, sel.accepted_floor, sel.generation
                ),
                None => "staged_only=true (provide --authorization for activation)".into(),
            });
            lines
        }
        UpdateCommand::Authorize {
            root,
            sequence,
            out: path,
            yes,
        } => {
            let generation = read_selector(&root).map_err(|e| e.to_string())?.generation;
            if !yes {
                confirm_authorization(&root, sequence, generation)?;
            }
            let owner = UpdaterOwner::acquire(&root).map_err(|e| e.to_string())?;
            let authorization = owner
                .authorize(&root, sequence, generation)
                .map_err(|e| e.to_string())?;
            owner
                .write_authorization(&path, &authorization)
                .map_err(|e| e.to_string())?;
            vec![format!(
                "authorized sequence={sequence} generation={generation} capability={}",
                path.display()
            )]
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
/// The owner's explicit consent at a terminal. Unattended callers (no
/// terminal on stdin) must pass `--yes`: authorization is an owner decision,
/// never a side effect of a script that happened to run.
fn confirm_authorization(
    root: &std::path::Path,
    sequence: u64,
    generation: u64,
) -> Result<(), String> {
    use std::io::{BufRead as _, IsTerminal as _};
    if !std::io::stdin().is_terminal() {
        return Err(
            "authorize is an owner decision: run it at a terminal, or pass --yes from the owner's supervisor"
                .to_string(),
        );
    }
    eprint!(
        "Authorize activating release sequence {sequence} over generation {generation} in {}? [y/N] ",
        root.display()
    );
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| format!("read the confirmation: {e}"))?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("authorization cancelled".to_string())
    }
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
        key[i] = u8::from_str_radix(std::str::from_utf8(chunk).map_err(|e| e.to_string())?, 16)
            .map_err(|e| format!("invalid hex in key `{key_id}`: {e}"))?;
    }
    Ok(TrustRoot {
        key_id: key_id.to_string(),
        verifying_key: key,
    })
}
