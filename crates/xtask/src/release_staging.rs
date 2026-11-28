//! Stage the release package and the source installer's Bun programs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};

use crate::first_party_release::{self, StageOptions, StagedBundle};

pub(crate) const BUN_VERSION: &str = "1.4.2";
pub(crate) const PACKAGED_BUN: &str = "lib/hya/bin/bun";
pub(crate) const ARGUS_PACKAGE: &str = "examples/hya-argus-example.hyabundle";

/// One Bun program the archive ships under `lib/hya` with production dependencies.
pub(crate) struct BunRuntime {
    /// Workspace directory the program is copied from.
    pub(crate) source: &'static str,
    /// Package-relative directory it is installed into.
    pub(crate) destination: &'static str,
    /// Top-level files copied as-is.
    pub(crate) files: &'static [&'static str],
    /// Directories copied recursively.
    pub(crate) directories: &'static [&'static str],
}

/// The Bun extension adapter for `kind: bun` plugins.
pub(crate) const BUN_ADAPTER_RUNTIME: BunRuntime = BunRuntime {
    source: "crates/hya-plugin-bun/adapter",
    destination: "lib/hya/bun-adapter",
    files: &["package.json", "bun.lock"],
    directories: &["src"],
};

/// The Claude Code adapter for local imports and `kind: claude` plugins.
pub(crate) const CLAUDE_ADAPTER_RUNTIME: BunRuntime = BunRuntime {
    source: "crates/hya-plugin-claude/adapter",
    destination: "lib/hya/claude-adapter",
    files: &["package.json", "bun.lock"],
    directories: &["src"],
};

/// The OpenTUI terminal UI bare `hya` runs.
pub(crate) const TUI_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-tui",
    destination: "lib/hya/tui",
    files: &[
        "package.json",
        "bun.lock",
        "bunfig.toml",
        "tsconfig.json",
        "frontend-version.ts",
    ],
    directories: &["src"],
};

/// The WebUI host that serves the TUI to the browser over a PTY.
pub(crate) const TUI_WEB_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-tui-web",
    destination: "lib/hya/tui-web",
    files: &["package.json", "bun.lock", "tsconfig.json"],
    directories: &["src", "web"],
};

/// The TUI extension SDK: the shared extension host the TUI starts, and the
/// QuickJS VM bundle TUI extensions run in.
pub(crate) const TUI_SDK_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-tui-sdk",
    destination: "lib/hya/tui-sdk",
    files: &["package.json", "bun.lock", "tsconfig.json"],
    directories: &["src"],
};

/// The core Code Mode VM runtime (`code-mode__run`).
pub(crate) const CODE_MODE_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-code-mode",
    destination: "lib/hya/code-mode",
    files: &["package.json", "runtime.ts"],
    directories: &[],
};

/// Every Bun program in the release archive, in staging order.
pub(crate) const PACKAGED_RUNTIMES: [&BunRuntime; 6] = [
    &BUN_ADAPTER_RUNTIME,
    &CLAUDE_ADAPTER_RUNTIME,
    &CODE_MODE_RUNTIME,
    &TUI_SDK_RUNTIME,
    &TUI_RUNTIME,
    &TUI_WEB_RUNTIME,
];

pub(crate) fn release_bun() -> Result<PathBuf> {
    let bun = bun_on_path()?;
    let version = Command::new(&bun).arg("--version").output()?;
    ensure!(
        version.status.success() && String::from_utf8_lossy(&version.stdout).trim() == BUN_VERSION,
        "Bun from PATH must report version {BUN_VERSION}"
    );
    bun.canonicalize().context("resolve release Bun executable")
}

/// Stage the package tree consumed by both the release workflow and rehearsal.
pub(crate) fn stage(root: &Path, options: &StageOptions, bun: &Path) -> Result<Vec<StagedBundle>> {
    let package = &options.package_root;
    let backend = package.join("bin/hya");
    copy_file(&options.library_dir.join("hya"), &backend)?;
    set_executable(&backend)?;
    let migration = package.join("bin/hya-migration");
    copy_file(&options.library_dir.join("hya-migration"), &migration)?;
    set_executable(&migration)?;
    for file in ["README.md", "THIRD_PARTY_NOTICES"] {
        copy_file(&root.join(file), &package.join(file))?;
    }

    let packaged_bun = package.join(PACKAGED_BUN);
    copy_file(bun, &packaged_bun)?;
    set_executable(&packaged_bun)?;
    let packaged_bun = packaged_bun.canonicalize()?;
    stage_bun_programs(root, package, &packaged_bun)?;
    let tui = package.join(TUI_RUNTIME.destination);
    copy_file(
        &root.join(TUI_RUNTIME.source).join("build.ts"),
        &tui.join("build.ts"),
    )?;
    run_bun(&packaged_bun, &tui, &["run", "build"])?;

    crate::package_bundle::package_directory(
        &root.join("bundles/examples/argus-example"),
        &package.join(ARGUS_PACKAGE),
    )?;
    first_party_release::stage(options)
}

/// Stage the Bun programs in an isolated package tree for the source installer.
fn stage_bun_programs(root: &Path, package: &Path, bun: &Path) -> Result<()> {
    for runtime in PACKAGED_RUNTIMES {
        let source = root.join(runtime.source);
        let destination = package.join(runtime.destination);
        for file in runtime.files {
            copy_file(&source.join(file), &destination.join(file))?;
        }
        for directory in runtime.directories {
            copy_directory_contents(&source.join(directory), &destination.join(directory))?;
        }
        if runtime.files.contains(&"bun.lock") {
            run_bun(
                bun,
                &destination,
                &["install", "--frozen-lockfile", "--production"],
            )?;
        }
    }
    Ok(())
}

fn run_bun(bun: &Path, directory: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(bun)
        .args(args)
        .current_dir(directory)
        .status()
        .with_context(|| format!("run Bun in {}", directory.display()))?;
    ensure!(
        status.success(),
        "Bun {args:?} failed in {}: {status}",
        directory.display()
    );
    Ok(())
}

pub(crate) fn run(args: Vec<String>) -> Result<()> {
    let usage = "usage: cargo run -p xtask -- stage-release --library-dir <dir> \
                 --package-root <dir> [--version <semver>] [--target <triple> --assets <dir>]";
    if matches!(args.as_slice(), [flag] if flag == "--help" || flag == "-h") {
        println!("{usage}");
        return Ok(());
    }
    let options = first_party_release::parse_options(args, usage)?;
    stage(&repo_root()?, &options, &release_bun()?)?;
    Ok(())
}

pub(crate) fn run_bun_programs(args: Vec<String>) -> Result<()> {
    let usage = "usage: cargo run -p xtask -- stage-bun-programs --package-root <dir>";
    match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{usage}");
            Ok(())
        }
        [flag, package] if flag == "--package-root" => {
            let bun = bun_on_path()?.canonicalize()?;
            stage_bun_programs(&repo_root()?, Path::new(package), &bun)
        }
        _ => bail!("{usage}"),
    }
}

/// Return the workspace root from this crate's manifest directory.
pub(crate) fn repo_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("resolve workspace root from CARGO_MANIFEST_DIR")
}

/// Find the Bun executable on `PATH`.
fn bun_on_path() -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bun"))
        .find(|candidate| candidate.is_file())
        .context("bun is not on PATH")
}

/// Copy one source file and preserve a path-specific failure context.
pub(crate) fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::copy(source, destination)
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))?;
    Ok(())
}

/// Recursively copy directory contents while rejecting symlinked release inputs.
fn copy_directory_contents(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).with_context(|| format!("create {}", destination.display()))?;
    let entries = fs::read_dir(source)
        .with_context(|| format!("read release source directory {}", source.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .with_context(|| format!("inspect {}", source_path.display()))?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "release source contains symlink {}",
            source_path.display()
        );
        if metadata.is_dir() {
            copy_directory_contents(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            copy_file(&source_path, &destination_path)?;
        } else {
            bail!(
                "release source contains unsupported path {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

/// Mark one packaged executable as user-runnable on Unix hosts.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("inspect executable {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("set executable permissions on {}", path.display()))?;
    Ok(())
}

/// Keep release files unchanged on platforms without Unix mode bits.
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}
