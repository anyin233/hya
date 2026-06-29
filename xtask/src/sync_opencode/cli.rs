use std::path::PathBuf;

use anyhow::{Context, Result, bail};

#[derive(Debug, Default)]
pub(crate) struct Args {
    pub(crate) dry_run: bool,
    pub(crate) prune: bool,
    pub(crate) help: bool,
    pub(crate) opencode_config: Option<PathBuf>,
    pub(crate) opencode_skill_roots: Vec<PathBuf>,
    pub(crate) hya_config: Option<PathBuf>,
    pub(crate) hya_skills_root: Option<PathBuf>,
}

pub(crate) fn parse_args(raw_args: Vec<String>) -> Result<Args> {
    let mut args = Args::default();
    let mut iter = raw_args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => args.help = true,
            "--dry-run" => args.dry_run = true,
            "--prune" => args.prune = true,
            "--opencode-config" => {
                args.opencode_config = Some(PathBuf::from(
                    iter.next().context("missing value for --opencode-config")?,
                ));
            }
            "--opencode-skill-root" => {
                args.opencode_skill_roots.push(PathBuf::from(
                    iter.next()
                        .context("missing value for --opencode-skill-root")?,
                ));
            }
            "--hya-config" => {
                args.hya_config = Some(PathBuf::from(
                    iter.next().context("missing value for --hya-config")?,
                ));
            }
            "--hya-skills-root" => {
                args.hya_skills_root = Some(PathBuf::from(
                    iter.next().context("missing value for --hya-skills-root")?,
                ));
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    Ok(args)
}

pub(crate) fn print_help() {
    println!(concat!(
        "usage: cargo xtask sync-opencode [OPTIONS]\n\n",
        "Options:\n",
        "  --help, -h                 Show this help\n",
        "  --dry-run                  Print planned migration actions without writing\n",
        "  --prune                    Remove lockfile-managed migrated state\n",
        "  --opencode-config <PATH>   Path to OpenCode opencode.json\n",
        "  --opencode-skill-root <PATH>\n",
        "                             Additional OpenCode skill root (repeatable)\n",
        "  --hya-config <PATH>       Path to target hya config.yaml\n",
        "  --hya-skills-root <PATH>  Path to target hya skills root\n"
    ));
}
