//! Rehearse the GitHub release workflow without creating or publishing a release.
//!
//! Build locked release inputs and stage them through the same code as CI,
//! then archive and smoke-test the installed layout in a temporary directory.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};

use crate::first_party_release::StageOptions;
use crate::release_staging::{
    self, ARGUS_PACKAGE, BUN_ADAPTER_RUNTIME, BUN_VERSION, BunRuntime, CLAUDE_ADAPTER_RUNTIME,
    PACKAGED_BUN, PACKAGED_RUNTIMES, TUI_RUNTIME, TUI_SDK_RUNTIME, TUI_WEB_RUNTIME, copy_file,
    repo_root,
};

/// Highest text-lockfile version [`BUN_VERSION`] can read.
const BUN_LOCKFILE_VERSION: u64 = 2;
/// Targets the release matrix builds; a rehearsal runs on one of these hosts.
const RELEASE_TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
];

/// Require the rehearsal target to be the machine it runs on.
///
/// A rehearsal builds, packages, and smoke-runs the release natively, exactly
/// like that target's matrix job, so it cannot rehearse a foreign target.
fn require_host_target(target: &str, host: &str) -> Result<()> {
    ensure!(
        target == host,
        "release target `{target}` must be rehearsed on a `{target}` host; this host is `{host}`"
    );
    Ok(())
}

/// Require the Bun adapter lockfile to be readable by the pinned Bun.
///
/// A newer Bun writes a lockfile version the release's pinned Bun rejects, which
/// would only surface when `bun install --frozen-lockfile` runs during packaging.
fn validate_bun_lockfile(root: &Path) -> Result<()> {
    for runtime in PACKAGED_RUNTIMES {
        if !runtime.files.contains(&"bun.lock") {
            continue;
        }
        let lockfile = read_text(root, &format!("{}/bun.lock", runtime.source))?;
        let version = parse_bun_lockfile_version(&lockfile)
            .with_context(|| format!("read {}/bun.lock", runtime.source))?;
        require_supported_bun_lockfile(runtime.source, version)?;
    }
    Ok(())
}

/// Read `lockfileVersion` from Bun's text lockfile.
fn parse_bun_lockfile_version(lockfile: &str) -> Result<u64> {
    lockfile
        .lines()
        .find_map(|line| line.trim().strip_prefix("\"lockfileVersion\":"))
        .map(|value| value.trim().trim_end_matches(','))
        .context("bun.lock has no lockfileVersion")?
        .parse()
        .context("bun.lock lockfileVersion is not a number")
}

/// Reject lockfile versions newer than the pinned Bun understands.
fn require_supported_bun_lockfile(source: &str, version: u64) -> Result<()> {
    ensure!(
        version <= BUN_LOCKFILE_VERSION,
        "{source}/bun.lock uses lockfileVersion {version}, which Bun {BUN_VERSION} cannot \
         read; regenerate it with Bun {BUN_VERSION}"
    );
    Ok(())
}

/// Return the host target triple reported by `rustc -vV`.
fn host_target(root: &Path) -> Result<String> {
    let output = run_checked(OsStr::new("rustc"), &arg_list(&["-vV"]), root, &[], &[])
        .context("read the host target from rustc -vV")?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV did not report a host target")
}
const BINARY_NAME: &str = "hya";
/// Crates whose release `cdylib` the native bundles (tool families and
/// internal bundles) package.
const NATIVE_BUNDLE_CRATES: [&str; 10] = [
    "hya-hashline",
    "hya-coding-extras",
    "hya-extended-tools",
    "hya-network",
    "hya-channel-tools",
    "hya-compaction",
    "hya-plan",
    "hya-goal",
    "hya-loop",
    "hya-workflow-bundle",
];

/// Production dependencies the staged TUI must contain (besides its native package).
const TUI_DEPENDENCIES: [&str; 3] = ["@opentui/core", "@opentui/solid", "solid-js"];
/// Development-only packages a production TUI install must not contain.
const TUI_DEV_ONLY: [&str; 1] = ["bun-types"];
/// Production dependencies the staged WebUI host must contain.
const TUI_WEB_DEPENDENCIES: [&str; 2] = ["@xterm/xterm", "@xterm/addon-fit"];
/// Development-only packages a production WebUI host install must not contain.
const TUI_WEB_DEV_ONLY: [&str; 4] = [
    "@playwright/test",
    "@opentui/core",
    "bun-types",
    "typescript",
];
const TUI_RUNTIME_FILES: [&str; 2] = ["src/main.ts", "dist/app.js"];
const TUI_WEB_RUNTIME_FILES: [&str; 6] = [
    "src/main.ts",
    "src/frames.ts",
    "web/index.html",
    "web/client.ts",
    "web/style.css",
    "node_modules/@xterm/xterm/css/xterm.css",
];
/// The extension host's entry points the TUI starts.
const TUI_SDK_RUNTIME_FILES: [&str; 4] = [
    "src/main.ts",
    "src/host.ts",
    "src/worker.ts",
    "src/jitWorker.ts",
];
/// Production dependencies the staged SDK must contain (the QuickJS VM).
const TUI_SDK_DEPENDENCIES: [&str; 2] = [
    "quickjs-emscripten-core",
    "@jitl/quickjs-wasmfile-release-sync",
];
/// Development-only packages a production SDK install must not contain.
const TUI_SDK_DEV_ONLY: [&str; 2] = ["bun-types", "typescript"];

/// Return the OpenTUI native package `bun install` selects on a release target.
///
/// `@opentui/core` loads its renderer from a per-platform optional dependency,
/// so each release job must install on its own runner.
fn opentui_native_package(target: &str) -> Result<&'static str> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok("@opentui/core-linux-x64"),
        "aarch64-unknown-linux-gnu" => Ok("@opentui/core-linux-arm64"),
        "aarch64-apple-darwin" => Ok("@opentui/core-darwin-arm64"),
        _ => bail!("no OpenTUI native package is known for release target `{target}`"),
    }
}
/// Command-line options for one non-publishing rehearsal.
#[derive(Debug)]
struct Options {
    version: String,
    target: String,
}

/// Temporary workspace used for the archive and its extraction smoke tests.
struct ScratchDirectory {
    path: PathBuf,
}

impl ScratchDirectory {
    /// Create a unique temporary directory for one rehearsal.
    fn create() -> Result<Self> {
        let base = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("read system clock while creating release rehearsal directory")?
            .as_nanos();
        for attempt in 0..100_u32 {
            let path = base.join(format!(
                "hya-release-rehearsal-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("create release rehearsal directory {}", path.display())
                    });
                }
            }
        }
        bail!(
            "could not create a unique release rehearsal directory under {}",
            base.display()
        )
    }

    /// Return the temporary directory path.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDirectory {
    /// Remove the temporary rehearsal workspace after the command finishes.
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "release-rehearsal: failed to clean temporary directory {}: {error}",
                self.path.display()
            );
        }
    }
}

/// Run `release-rehearsal` with the supplied arguments.
///
/// # Errors
/// Returns an error when arguments, release metadata, or any release command
/// fails. The command never performs a publishing action.
///
/// Workflow syntax is checked separately with `actionlint`.
pub fn run(args: Vec<String>) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }
    let options = parse_args(&args)?;
    let root = repo_root()?;
    validate_release_metadata(&root, &options.version, &options.target)?;
    require_host_target(&options.target, &host_target(&root)?)?;
    let bun = release_staging::release_bun()?;
    prepare_and_build(&root, &options.target)?;
    rehearse_package(&root, &options.version, &options.target, &bun)?;

    println!(
        "release-rehearsal: ok — version {}, target {}, no publish",
        options.version, options.target
    );
    Ok(())
}

/// Print the command usage without touching the repository or running tools.
fn print_usage() {
    println!(
        "usage: cargo run -p xtask -- release-rehearsal \
         --version <semver> --target <target> --no-publish"
    );
}

/// Parse and validate the release rehearsal command-line arguments.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut version = None;
    let mut target = None;
    let mut no_publish = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--version" => {
                version = Some(next_argument(args, &mut index, "--version")?);
            }
            "--target" => {
                target = Some(next_argument(args, &mut index, "--target")?);
            }
            "--no-publish" => {
                ensure!(!no_publish, "duplicate --no-publish flag");
                no_publish = true;
            }
            argument => bail!("unknown release-rehearsal argument `{argument}`; use --no-publish"),
        }
        index += 1;
    }

    ensure!(no_publish, "release-rehearsal requires --no-publish");
    let version = version.context("release-rehearsal requires --version <semver>")?;
    let target = target.context("release-rehearsal requires --target <target>")?;
    ensure!(is_safe_target(&target), "invalid release target `{target}`");

    Ok(Options { version, target })
}

/// Consume the value following one named command-line option.
fn next_argument(args: &[String], index: &mut usize, option: &str) -> Result<String> {
    *index += 1;
    let value = args
        .get(*index)
        .with_context(|| format!("{option} requires a value"))?;
    ensure!(
        !value.starts_with('-'),
        "{option} requires a value, found `{value}`"
    );
    Ok(value.clone())
}

/// Validate the semver, unified version files, and newest-only changelog contract.
fn validate_release_metadata(root: &Path, version: &str, target: &str) -> Result<()> {
    ensure!(
        is_semver(version),
        "release version `{version}` is not semver-shaped"
    );
    ensure!(
        RELEASE_TARGETS.contains(&target),
        "release target `{target}` is not one of the release targets {RELEASE_TARGETS:?}"
    );
    let (manifest_version, minimum_backend_version) = aggregate_versions(root)?;
    ensure!(
        release_version_at_least(&manifest_version, &minimum_backend_version)?,
        "hya version `{manifest_version}` is below frontend minimum `{minimum_backend_version}`"
    );
    let frontend_source = read_text(root, "packages/hya-tui/frontend-version.ts")?;
    ensure!(
        frontend_source.contains(&format!(
            "export const frontendVersion = \"{manifest_version}\""
        )),
        "packages/hya-tui/frontend-version.ts does not carry unified version {manifest_version}"
    );
    ensure!(
        frontend_source.contains(&format!(
            "export const minimumBackendVersion = \"{minimum_backend_version}\""
        )),
        "packages/hya-tui/frontend-version.ts does not carry minimum backend version {minimum_backend_version}"
    );
    validate_bun_lockfile(root)?;
    let manifest: toml::Value =
        toml::from_str(&read_text(root, "Cargo.toml")?).context("parse Cargo.toml")?;
    let workspace_version = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .context("Cargo.toml [workspace.package].version is missing")?;
    ensure!(
        workspace_version == version,
        "Cargo.toml workspace version `{workspace_version}` does not match `{version}`"
    );
    ensure!(
        manifest_version == version,
        "versions.toml version `{manifest_version}` does not match `{version}`"
    );
    validate_lockfile_versions(&read_text(root, "Cargo.lock")?, version)?;
    validate_newest_changelog(&read_text(root, "CHANGELOG.md")?, "CHANGELOG.md", version)?;
    let readme = read_text(root, "README.md")?;
    ensure!(
        readme.contains(&format!("`{version}`")),
        "README.md does not carry release version `{version}`"
    );
    Ok(())
}

fn validate_newest_changelog(source: &str, path: &str, version: &str) -> Result<()> {
    let first_heading = source.lines().find(|line| line.starts_with("# "));
    let expected_heading = format!("# {version}");
    ensure!(
        first_heading == Some(expected_heading.as_str()),
        "{path} first heading must be `{expected_heading}`"
    );
    if let Some(heading) = source.lines().filter(|line| line.starts_with("# ")).nth(1) {
        bail!("{path} must be newest-only; found extra heading `{heading}`");
    }
    Ok(())
}

/// Validate the aggregate backend package and placeholder component versions.
fn validate_lockfile_versions(lockfile: &str, version: &str) -> Result<()> {
    let mut found = false;
    for package in lockfile.split("[[package]]").skip(1) {
        let Some(name) = lockfile_field(package, "name") else {
            continue;
        };
        if name != "hya" && !name.starts_with("hya-") {
            continue;
        }
        found = true;
        let package_version = lockfile_field(package, "version")
            .with_context(|| format!("Cargo.lock package {name} has no version"))?;
        let expected = if name == "hya-backend" {
            version
        } else {
            "0.0.0"
        };
        ensure!(
            package_version == expected,
            "Cargo.lock package {name} has version `{package_version}`, expected `{expected}`"
        );
    }
    ensure!(found, "Cargo.lock contains no hya packages");
    Ok(())
}

/// Read one repository text file with a path-specific error.
fn read_text(root: &Path, relative: &str) -> Result<String> {
    let path = root.join(relative);
    fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
}

fn aggregate_versions(root: &Path) -> Result<(String, String)> {
    let source = read_text(root, "versions.toml")?;
    let manifest: toml::Value = toml::from_str(&source).context("parse versions.toml")?;
    let value = |key: &str| {
        manifest
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("versions.toml `{key}` is missing"))
    };
    Ok((value("version")?, value("minimum_backend_version")?))
}

fn release_version_at_least(actual: &str, minimum: &str) -> Result<bool> {
    let parse = |version: &str| -> Result<[u64; 3]> {
        let without_build = version.split_once('+').map_or(version, |(core, _)| core);
        let core = without_build
            .split_once('-')
            .map_or(without_build, |(core, _)| core);
        let parts = core
            .split('.')
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(parts.len() == 3, "version `{version}` is not semver-shaped");
        Ok([parts[0], parts[1], parts[2]])
    };
    Ok(parse(actual)? >= parse(minimum)?)
}

/// Read one field from a Cargo.lock package block.
fn lockfile_field<'a>(package: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field} = \"");
    package
        .lines()
        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))
}

/// Check the semver grammar used by the release workflow without another crate.
fn is_semver(version: &str) -> bool {
    let (without_build, build) = match version.split_once('+') {
        Some((core, build)) => (core, Some(build)),
        None => (version, None),
    };
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (without_build, None),
    };
    let core_parts: Vec<&str> = core.split('.').collect();
    if core_parts.len() != 3 || core_parts.iter().any(|part| part.is_empty()) {
        return false;
    }
    if core_parts
        .iter()
        .any(|part| !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    [prerelease, build].into_iter().flatten().all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    })
}

/// Reject target strings that could escape the target build directory.
fn is_safe_target(target: &str) -> bool {
    !target.is_empty()
        && target != "."
        && !target.contains("..")
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Check whether a Git tag matches the canonical `vx.x.x` / `vx.x.x-<suffix>` pattern.
#[cfg(test)]
fn is_release_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    !version.contains('+') && is_semver(version)
}

/// Run the locked target release builds.
fn prepare_and_build(root: &Path, target: &str) -> Result<()> {
    let args = vec![
        "build".to_owned(),
        "--release".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "hya-backend".to_owned(),
        "--bins".to_owned(),
        "--target".to_owned(),
        target.to_owned(),
    ];
    run_checked(OsStr::new("cargo"), &args, root, &[], &[])
        .context("run locked release build for hya-backend")?;
    let mut libraries = arg_list(&["build", "--release", "--locked"]);
    for library in NATIVE_BUNDLE_CRATES {
        libraries.extend(["-p".to_owned(), library.to_owned()]);
    }
    libraries.extend(["--lib".to_owned(), "--target".to_owned(), target.to_owned()]);
    run_checked(OsStr::new("cargo"), &libraries, root, &[], &[])
        .context("run locked release build for the native bundle libraries")?;
    Ok(())
}

/// Reproduce the unified workflow archive, checksum, extraction, and smoke checks.
fn rehearse_package(root: &Path, version: &str, target: &str, bun: &Path) -> Result<()> {
    let scratch = ScratchDirectory::create()?;
    let dist = scratch.path().join("dist");
    fs::create_dir_all(&dist).with_context(|| format!("create {}", dist.display()))?;
    let package_name = format!("{BINARY_NAME}-{version}-{target}");
    let package_root = dist.join(&package_name);

    let metadata = run_checked(
        OsStr::new("cargo"),
        &arg_list(&["metadata", "--format-version", "1", "--no-deps", "--locked"]),
        root,
        &[],
        &[],
    )?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let target_dir = metadata["target_directory"]
        .as_str()
        .context("cargo metadata did not report a target directory")?;
    let release_inputs = Path::new(target_dir).join(target).join("release");
    let staged = release_staging::stage(
        root,
        &StageOptions {
            version: version.to_owned(),
            library_dir: release_inputs,
            package_root: package_root.clone(),
            target: Some(target.to_owned()),
            assets: Some(dist.clone()),
        },
        bun,
    )
    .context("stage the release package")?;
    let example = package_root.join(ARGUS_PACKAGE);
    run_checked(
        OsStr::new("7z"),
        &["t".to_owned(), example.display().to_string()],
        root,
        &[],
        &[],
    )
    .context("test packaged example archive")?;
    let seven_zip_listing = run_checked(
        OsStr::new("7z"),
        &[
            "l".to_owned(),
            "-slt".to_owned(),
            example.display().to_string(),
        ],
        root,
        &[],
        &[],
    )
    .context("list packaged example archive")?;
    verify_example_listing(&String::from_utf8_lossy(&seven_zip_listing.stdout))?;

    let assets = staged
        .iter()
        .map(|bundle| {
            bundle
                .asset
                .as_ref()
                .and_then(|asset| asset.file_name())
                .and_then(OsStr::to_str)
                .map(str::to_owned)
                .context("first-party bundle asset has no UTF-8 file name")
        })
        .collect::<Result<Vec<_>>>()?;

    let archive_name = format!("{package_name}.tar.gz");
    let archive = dist.join(&archive_name);
    run_checked(
        OsStr::new("tar"),
        &[
            "-czf".to_owned(),
            archive.display().to_string(),
            "-C".to_owned(),
            dist.display().to_string(),
            package_name.clone(),
        ],
        root,
        // macOS tar would otherwise add AppleDouble `._*` entries.
        &[("COPYFILE_DISABLE", OsString::from("1"))],
        &[],
    )
    .context("create release tar.gz archive")?;

    write_and_verify_checksums(&dist, target, &archive_name, &assets)?;

    verify_package_layout(&package_root, target)?;
    let extract_root = scratch.path().join("extract");
    fs::create_dir_all(&extract_root)
        .with_context(|| format!("create {}", extract_root.display()))?;
    run_checked(
        OsStr::new("tar"),
        &[
            "-xzf".to_owned(),
            archive.display().to_string(),
            "-C".to_owned(),
            extract_root.display().to_string(),
        ],
        root,
        &[],
        &[],
    )
    .context("extract release archive")?;
    let extracted = extract_root.join(&package_name);
    verify_package_layout(&extracted, target)?;
    verify_archive_listing(root, &archive, &package_name, &scratch)?;
    smoke_packaged_release(&extracted, &scratch, version)?;
    verify_tui_imports_resolve(&extracted, &scratch)?;
    smoke_release_installer(root, &dist, &archive_name, target, version, &scratch)?;
    for bundle in &staged {
        let asset = bundle
            .asset
            .as_ref()
            .context("first-party bundle asset is missing")?;
        let installed = bundle
            .installed
            .strip_prefix(&package_root)
            .context("staged bundle is outside the package root")?;
        ensure!(
            fs::read(asset).with_context(|| format!("read {}", asset.display()))?
                == fs::read(extracted.join(installed))
                    .with_context(|| format!("read extracted {}", installed.display()))?,
            "release asset {} differs from the archived package",
            asset.display()
        );
    }
    Ok(())
}

/// Write SHA256SUMS for the archive and bundle assets, then verify it like CI.
fn write_and_verify_checksums(
    dist: &Path,
    _target: &str,
    archive_name: &str,
    assets: &[String],
) -> Result<()> {
    let mut files = arg_list(&["-a", "256"]);
    files.push(archive_name.to_owned());
    files.extend(assets.iter().cloned());
    let checksum = run_checked(OsStr::new("shasum"), &files, dist, &[], &[])?;
    let sums_name = "SHA256SUMS".to_owned();
    let sums = dist.join(&sums_name);
    fs::write(&sums, &checksum.stdout)
        .with_context(|| format!("write checksum manifest {}", sums.display()))?;
    run_checked(
        OsStr::new("shasum"),
        &[
            "-a".to_owned(),
            "256".to_owned(),
            "-c".to_owned(),
            sums_name,
        ],
        dist,
        &[],
        &[],
    )
    .context("verify release checksums")?;
    Ok(())
}

/// Verify the example bundle paths and the absence of its source-tree prefix.
fn verify_example_listing(listing: &str) -> Result<()> {
    require_listing_line(listing, "Path = bundle.yaml", "7z example listing")?;
    require_listing_line(
        listing,
        "Path = workflows/argus.hya.md",
        "7z example listing",
    )?;
    ensure!(
        !listing.contains("bundles/examples/argus-example"),
        "packaged example contains its source-tree prefix"
    );
    Ok(())
}

/// Verify required runtime files before archiving.
fn verify_package_layout(package_root: &Path, target: &str) -> Result<()> {
    require_file(&package_root.join("bin").join("hya"), "packaged binary")?;
    require_file(&package_root.join(PACKAGED_BUN), "packaged Bun")?;

    verify_staged_program(
        package_root,
        &BUN_ADAPTER_RUNTIME,
        &["src/main.ts"],
        &[],
        &[],
    )?;
    verify_staged_program(
        package_root,
        &CLAUDE_ADAPTER_RUNTIME,
        &["src/main.ts"],
        &[],
        &["@types/bun", "typescript"],
    )?;
    require_file(
        &package_root.join(TUI_RUNTIME.destination).join("build.ts"),
        "packaged TUI build entry point",
    )?;
    verify_tui_layout(package_root, target)?;
    for identity in hya_bundle::FIRST_PARTY_BUNDLES {
        let name = hya_bundle::first_party_package_name(identity)
            .with_context(|| format!("no package name for {identity}"))?;
        require_file(
            &package_root.join("bundles").join(name),
            "packaged first-party bundle",
        )?;
    }
    Ok(())
}

/// Verify the staged TUI and WebUI host: sources, production dependencies
/// (including the target's OpenTUI native package), and no dev-only packages.
fn verify_tui_layout(package_root: &Path, target: &str) -> Result<()> {
    let native = opentui_native_package(target)?;
    let mut tui_dependencies = TUI_DEPENDENCIES.to_vec();
    tui_dependencies.push(native);
    verify_staged_program(
        package_root,
        &TUI_RUNTIME,
        &TUI_RUNTIME_FILES,
        &tui_dependencies,
        &TUI_DEV_ONLY,
    )?;
    verify_staged_program(
        package_root,
        &TUI_WEB_RUNTIME,
        &TUI_WEB_RUNTIME_FILES,
        &TUI_WEB_DEPENDENCIES,
        &TUI_WEB_DEV_ONLY,
    )?;
    verify_staged_program(
        package_root,
        &TUI_SDK_RUNTIME,
        &TUI_SDK_RUNTIME_FILES,
        &TUI_SDK_DEPENDENCIES,
        &TUI_SDK_DEV_ONLY,
    )
}

/// Require one staged Bun program's files and dependencies, and reject its
/// development-only packages.
fn verify_staged_program(
    package_root: &Path,
    runtime: &BunRuntime,
    runtime_files: &[&str],
    dependencies: &[&str],
    dev_only: &[&str],
) -> Result<()> {
    let directory = package_root.join(runtime.destination);
    let label = runtime.destination;
    for file in runtime.files.iter().chain(runtime_files) {
        require_file(&directory.join(file), &format!("packaged {label} file"))?;
    }
    for dependency in dependencies {
        require_file(
            &directory
                .join("node_modules")
                .join(dependency)
                .join("package.json"),
            &format!("packaged {label} dependency {dependency}"),
        )?;
    }
    for dependency in dev_only {
        let path = directory.join("node_modules").join(dependency);
        ensure!(
            fs::symlink_metadata(&path).is_err(),
            "packaged {label} contains development-only package {dependency} at {}",
            path.display()
        );
    }
    Ok(())
}

/// Verify the tar listing after checksum validation and before extraction smoke.
fn verify_archive_listing(
    root: &Path,
    archive: &Path,
    package_name: &str,
    scratch: &ScratchDirectory,
) -> Result<()> {
    let output = run_checked(
        OsStr::new("tar"),
        &["-tzf".to_owned(), archive.display().to_string()],
        root,
        &[],
        &[],
    )
    .context("list release tar archive")?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let listing_path = scratch.path().join("archive.txt");
    fs::write(&listing_path, listing.as_bytes())
        .with_context(|| format!("write archive listing {}", listing_path.display()))?;
    for path in [
        "bin/hya",
        PACKAGED_BUN,
        "lib/hya/bun-adapter/package.json",
        "lib/hya/bun-adapter/bun.lock",
        "lib/hya/bun-adapter/src/main.ts",
        "lib/hya/claude-adapter/package.json",
        "lib/hya/claude-adapter/bun.lock",
        "lib/hya/claude-adapter/src/main.ts",
        "lib/hya/tui/frontend-version.ts",
        "lib/hya/tui/dist/app.js",
        "lib/hya/tui-web/web/index.html",
        "lib/hya/tui-sdk/src/main.ts",
        ARGUS_PACKAGE,
    ] {
        require_listing_line(
            &listing,
            &format!("{package_name}/{path}"),
            "release tar listing",
        )?;
    }
    for identity in hya_bundle::FIRST_PARTY_BUNDLES {
        let name = hya_bundle::first_party_package_name(identity)
            .with_context(|| format!("no package name for {identity}"))?;
        require_listing_line(
            &listing,
            &format!("{package_name}/bundles/{name}"),
            "release tar listing",
        )?;
    }
    ensure!(
        !listing.contains(&format!("{package_name}/bundles/examples/argus-example")),
        "release tar contains the example source-tree prefix"
    );
    Ok(())
}

/// Smoke packaged binaries, adapters, and TUI/WebUI runtimes.
fn smoke_packaged_release(
    package_root: &Path,
    scratch: &ScratchDirectory,
    version: &str,
) -> Result<()> {
    let backend = package_root.join("bin/hya");
    let version_output = run_checked(
        backend.as_os_str(),
        &arg_list(&["--version"]),
        scratch.path(),
        &[],
        &[],
    )?;
    ensure!(
        combined_output(&version_output).contains(version),
        "packaged hya --version did not report {version}"
    );
    run_checked(
        backend.as_os_str(),
        &arg_list(&["--help"]),
        scratch.path(),
        &[],
        &[],
    )
    .context("smoke packaged hya --help")?;
    smoke_first_party_bundles(&backend, scratch, version)?;
    let bun = package_root.join(PACKAGED_BUN);
    let bun_version = run_checked(
        bun.as_os_str(),
        &arg_list(&["--version"]),
        scratch.path(),
        &[],
        &[],
    )
    .context("smoke the packaged Bun")?;
    ensure!(
        String::from_utf8_lossy(&bun_version.stdout).trim() == BUN_VERSION,
        "packaged Bun must report version {BUN_VERSION}"
    );

    smoke_bun_adapter(&package_root.join("lib/hya/bun-adapter"), scratch)?;
    smoke_tui_runtime(package_root, scratch)
}

/// Require the packaged backend to load every first-party bundle at `version`.
fn smoke_first_party_bundles(
    backend: &Path,
    scratch: &ScratchDirectory,
    version: &str,
) -> Result<()> {
    let home = scratch.path().join("smoke-home");
    fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    let envs = [
        ("HOME", home.as_os_str().to_os_string()),
        ("XDG_CONFIG_HOME", home.join("config").into_os_string()),
        ("XDG_DATA_HOME", home.join("data").into_os_string()),
        ("XDG_STATE_HOME", home.join("state").into_os_string()),
        ("XDG_CACHE_HOME", home.join("cache").into_os_string()),
    ];
    let output = run_checked(
        backend.as_os_str(),
        &arg_list(&["bundle", "list"]),
        scratch.path(),
        &envs,
        &[],
    )
    .context("list first-party bundles with the packaged backend")?;
    let listing = String::from_utf8_lossy(&output.stdout);
    for identity in hya_bundle::FIRST_PARTY_BUNDLES {
        let row = format!("{identity} {version} ");
        ensure!(
            listing.lines().any(|line| line.starts_with(&row)),
            "packaged bundle list lacks `{identity}` at {version}"
        );
    }
    Ok(())
}

/// Install the unified archive with `scripts/hya-install.sh` from a `file://`
/// `v<version>` release tree like the workflow, then require bare `hya update`
/// to find the installed release current.
fn smoke_release_installer(
    root: &Path,
    dist: &Path,
    archive_name: &str,
    target: &str,
    version: &str,
    scratch: &ScratchDirectory,
) -> Result<()> {
    let releases = scratch.path().join("releases");
    let sums = dist.join("SHA256SUMS");
    let tag_dir = releases.join(format!("download/v{version}"));
    copy_file(&sums, &tag_dir.join("SHA256SUMS"))?;
    copy_file(&sums, &releases.join("latest/download/SHA256SUMS"))?;
    copy_file(&dist.join(archive_name), &tag_dir.join(archive_name))?;
    let home = scratch.path().join("installer-home");
    let installed = scratch.path().join("installed");
    let envs = [
        ("HOME", home.as_os_str().to_os_string()),
        ("XDG_CONFIG_HOME", home.join("config").into_os_string()),
        ("XDG_DATA_HOME", home.join("data").into_os_string()),
        ("XDG_STATE_HOME", home.join("state").into_os_string()),
        ("XDG_CACHE_HOME", home.join("cache").into_os_string()),
        (
            "HYA_RELEASES_URL",
            OsString::from(format!("file://{}", releases.display())),
        ),
    ];
    run_checked(
        OsStr::new("sh"),
        &[
            root.join("scripts/hya-install.sh").display().to_string(),
            "--prefix".to_owned(),
            installed.display().to_string(),
        ],
        scratch.path(),
        &envs,
        &[],
    )
    .context("install the unified release archive")?;
    verify_package_layout(&installed, target)?;
    let backend = installed.join("bin/hya");
    let version_output = run_checked(
        backend.as_os_str(),
        &arg_list(&["--version"]),
        scratch.path(),
        &envs,
        &[],
    )?;
    ensure!(
        combined_output(&version_output).trim() == format!("hya {version}"),
        "installed hya --version did not report {version}"
    );
    let update = run_checked(
        backend.as_os_str(),
        &arg_list(&["update"]),
        scratch.path(),
        &envs,
        &[],
    )
    .context("run hya update against the installed release")?;
    ensure!(
        combined_output(&update).contains(&format!("hya {version} is already installed")),
        "hya update did not find the installed {version} current"
    );
    smoke_claude_adapter(&backend, scratch, &home, &envs)
}

/// Exercise the configured runtime and import CLI through the installed adapter resolver.
fn smoke_claude_adapter(
    backend: &Path,
    scratch: &ScratchDirectory,
    home: &Path,
    envs: &[(&str, OsString)],
) -> Result<()> {
    let plugin = scratch.path().join("claude-smoke");
    fs::create_dir_all(plugin.join("hooks"))?;
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"claude-smoke","version":"1.0.0"}"#,
    )?;
    let marker = scratch.path().join("claude-smoke-marker");
    let marker_argument = marker.to_string_lossy().replace('\'', "'\"'\"'");
    let hooks = serde_json::json!({
        "hooks": { "SessionStart": [{ "hooks": [{
            "type": "command",
            "command": format!("printf ready > '{marker_argument}'"),
        }] }] },
    });
    fs::write(plugin.join("hooks/hooks.json"), serde_json::to_vec(&hooks)?)?;
    let config_dir = home.join("config/hya");
    fs::create_dir_all(&config_dir)?;
    let plugin_dir = serde_json::to_string(&plugin)?;
    fs::write(
        config_dir.join("config.yaml"),
        format!("plugins:\n  claude-smoke:\n    kind: claude\n    plugin_dir: {plugin_dir}\n"),
    )?;
    let removed_env = ["HYA_CLAUDE_ADAPTER_DIR", "BUN"];
    run_checked(
        backend.as_os_str(),
        &arg_list(&[
            "--model",
            "hya/offline",
            "exec",
            "--time-budget",
            "30",
            "Claude smoke",
        ]),
        scratch.path(),
        envs,
        &removed_env,
    )
    .context("run a configured kind: claude plugin from the installed release")?;
    ensure!(
        fs::read_to_string(&marker).context("read Claude SessionStart marker")? == "ready",
        "the installed Claude adapter did not run the SessionStart hook"
    );
    fs::remove_file(&marker)?;
    let mut args = arg_list(&["bundle", "install", "-y", "--claude"]);
    args.push(plugin.display().to_string());
    let install = run_checked(
        backend.as_os_str(),
        &args,
        scratch.path(),
        envs,
        &removed_env,
    )
    .context("import a Claude plugin from the installed release")?;
    ensure!(
        combined_output(&install).contains("installed claude/claude-smoke 1.0.0"),
        "the installed hya did not report the imported Claude bundle"
    );
    println!("release-rehearsal: installed Claude plugin hook and bundle import passed");
    Ok(())
}

/// Run the Bun adapter initialize/shutdown handshake and verify its bounded output.
fn smoke_bun_adapter(bun_adapter: &Path, scratch: &ScratchDirectory) -> Result<()> {
    let probe = scratch.path().join("bun-adapter-probe");
    let output_path = scratch.path().join("bun-adapter-output");
    fs::create_dir_all(&probe).with_context(|| format!("create {}", probe.display()))?;
    let script = bun_adapter.join("src/main.ts");
    let args = vec!["run".to_owned(), script.display().to_string()];
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocol_version\":1,\"host\":{\"name\":\"hya\",\"version\":\"release\"}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n";
    let output = run_process_with_input(
        OsStr::new("bun"),
        &args,
        &probe,
        &[],
        &["HYA_BUN_ADAPTER_DIR"],
        input,
    )
    .context("run Bun adapter handshake")?;
    ensure!(
        output.status.success(),
        "Bun adapter handshake failed with status {}",
        status_label(&output)
    );
    fs::write(&output_path, &output.stdout)
        .with_context(|| format!("write adapter output {}", output_path.display()))?;
    let body = String::from_utf8_lossy(&output.stdout);
    for marker in [
        "\"protocol_version\":1",
        "\"hooks\":[]",
        "\"tools\":[]",
        "\"id\":2,\"result\":{}",
    ] {
        ensure!(body.contains(marker), "adapter output lacks `{marker}`");
    }
    Ok(())
}

/// Environment overrides that would point `hya` away from the staged TUI.
const TUI_OVERRIDES: [&str; 2] = ["HYA_TUI_DIR", "HYA_TUI_WEB_DIR"];

/// Run the staged TUI and WebUI host from outside the repository.
///
/// Both `--help` entry points must start, and the WebUI host must serve its
/// page together with the bundled client script and xterm stylesheet.
fn smoke_tui_runtime(package_root: &Path, scratch: &ScratchDirectory) -> Result<()> {
    let probe = scratch.path().join("tui-probe");
    fs::create_dir_all(&probe).with_context(|| format!("create {}", probe.display()))?;
    for runtime in [&TUI_RUNTIME, &TUI_WEB_RUNTIME] {
        let script = package_root.join(runtime.destination).join("src/main.ts");
        let output = run_checked(
            OsStr::new("bun"),
            &[script.display().to_string(), "--help".to_owned()],
            &probe,
            &[],
            &TUI_OVERRIDES,
        )
        .with_context(|| format!("smoke packaged {} --help", runtime.destination))?;
        ensure!(
            String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "packaged {} --help printed no usage",
            runtime.destination
        );
    }
    smoke_web_host(&package_root.join(TUI_WEB_RUNTIME.destination), &probe)
}

/// Stops the WebUI host child process when the smoke check returns.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    /// Kill and reap the host so the rehearsal never leaks a listener.
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the staged WebUI host on a free port and fetch its page and assets.
fn smoke_web_host(tui_web: &Path, probe: &Path) -> Result<()> {
    use std::io::{BufRead as _, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;

    let script = tui_web.join("src/main.ts");
    let mut command = build_command(
        OsStr::new("bun"),
        &[
            script.display().to_string(),
            "--port".to_owned(),
            "0".to_owned(),
            "--".to_owned(),
            "true".to_owned(),
        ],
        probe,
        &[],
        &TUI_OVERRIDES,
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ChildGuard(command.spawn().context("spawn packaged WebUI host")?);
    let stdout = child.0.stdout.take().context("open WebUI host stdout")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(address) = parse_listen_address(&line) {
                let _ = sender.send(address);
                break;
            }
        }
    });
    let address = receiver
        .recv_timeout(Duration::from_secs(30))
        .context("packaged WebUI host did not report its listen address within 30s")?;

    let page = http_get(&address, "/").context("fetch the WebUI page")?;
    ensure!(
        page.contains("id=\"terminal\""),
        "packaged WebUI page lacks its terminal element"
    );
    let assets = page_asset_paths(&page);
    ensure!(
        assets.iter().any(|path| path.ends_with(".js"))
            && assets.iter().any(|path| path.ends_with(".css")),
        "packaged WebUI page references no bundled script and stylesheet: {assets:?}"
    );
    for path in &assets {
        let body = http_get(&address, path).with_context(|| format!("fetch WebUI asset {path}"))?;
        if path.ends_with(".css") {
            ensure!(
                body.contains(".xterm"),
                "packaged WebUI stylesheet {path} lacks the xterm styles"
            );
        }
    }
    drop(child);
    Ok(())
}

/// Return `host:port` from the WebUI host's `listening on http://…/` line.
fn parse_listen_address(line: &str) -> Option<String> {
    let url = line.split_once("listening on http://")?.1.trim();
    let address = url.split('/').next()?;
    (!address.is_empty()).then(|| address.to_owned())
}

/// Collect root-relative `src="/…"` and `href="/…"` references from a page.
fn page_asset_paths(page: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for attribute in ["src=\"/", "href=\"/"] {
        let mut rest = page;
        while let Some(start) = rest.find(attribute) {
            let value = &rest[start + attribute.len() - 1..];
            let Some(end) = value.find('"') else { break };
            paths.push(value[..end].to_owned());
            rest = &value[end..];
        }
    }
    paths
}

/// Fetch one path over plain HTTP and return the body of a `200` response.
fn http_get(address: &str, path: &str) -> Result<String> {
    use std::io::Read as _;
    use std::net::TcpStream;
    use std::time::Duration;

    let mut stream =
        TcpStream::connect(address).with_context(|| format!("connect to {address}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .context("set HTTP read timeout")?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .context("send HTTP request")?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .context("read HTTP response")?;
    parse_http_response(&String::from_utf8_lossy(&response))
}

/// Split an HTTP/1.1 response, require status `200`, and decode a chunked body.
fn parse_http_response(response: &str) -> Result<String> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("HTTP response has no header terminator")?;
    let status = head.lines().next().unwrap_or_default();
    ensure!(
        status.split_whitespace().nth(1) == Some("200"),
        "HTTP response status is `{status}`"
    );
    let chunked = head.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
        })
    });
    if !chunked {
        return Ok(body.to_owned());
    }
    let mut decoded = String::new();
    let mut rest = body;
    loop {
        let (size, after) = rest
            .split_once("\r\n")
            .context("chunked HTTP body lacks a size line")?;
        let size = usize::from_str_radix(size.split(';').next().unwrap_or_default().trim(), 16)
            .context("chunked HTTP body has an invalid chunk size")?;
        if size == 0 {
            return Ok(decoded);
        }
        let chunk = after
            .get(..size)
            .context("chunked HTTP body is truncated")?;
        decoded.push_str(chunk);
        rest = after[size..].trim_start_matches("\r\n");
    }
}

/// Require every import of the staged TUI and WebUI host to resolve inside
/// its own directory.
///
/// `--help` never loads the lazily imported app, so this bundles each entry
/// point with Bun, which follows every static and dynamic import and fails on
/// one that does not resolve — such as a relative path that leaves the
/// package. OpenTUI's native packages for other platforms stay external.
fn verify_tui_imports_resolve(package_root: &Path, scratch: &ScratchDirectory) -> Result<()> {
    let probe = scratch.path().join("tui-resolve");
    fs::create_dir_all(&probe).with_context(|| format!("create {}", probe.display()))?;
    for (runtime, externals) in [
        (&TUI_RUNTIME, &["@opentui/core-*"][..]),
        (&TUI_WEB_RUNTIME, &[]),
    ] {
        let script = package_root.join(runtime.destination).join("src/main.ts");
        let mut args = arg_list(&["build", "--target=bun"]);
        for external in externals {
            args.extend(["--external".to_owned(), (*external).to_owned()]);
        }
        let outdir = probe.join(runtime.destination.replace('/', "-"));
        args.extend([
            "--outdir".to_owned(),
            outdir.display().to_string(),
            script.display().to_string(),
        ]);
        let output = run_process(OsStr::new("bun"), &args, &probe, &[], &TUI_OVERRIDES)
            .with_context(|| {
                format!(
                    "bundle packaged {} to resolve its imports",
                    runtime.destination
                )
            })?;
        ensure!(
            output.status.success(),
            "packaged {} has imports that do not resolve inside it:\n{}",
            runtime.destination,
            unresolved_imports(&combined_output(&output))
        );
    }
    Ok(())
}

/// Keep only Bun's `Could not resolve` diagnostics and their locations.
fn unresolved_imports(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut report = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.contains("Could not resolve") {
            report.push(line.trim());
            if let Some(location) = lines
                .get(index + 1)
                .filter(|next| next.trim().starts_with("at "))
            {
                report.push(location.trim());
            }
        }
    }
    if report.is_empty() {
        "bun build failed without an unresolved-import diagnostic".to_owned()
    } else {
        report.join("\n")
    }
}

/// Require a regular file at one release path.
fn require_file(path: &Path, label: &str) -> Result<()> {
    ensure!(path.is_file(), "{label} is missing at {}", path.display());
    Ok(())
}

/// Require one exact line in a command listing or captured output.
fn require_listing_line(listing: &str, expected: &str, label: &str) -> Result<()> {
    ensure!(
        listing.lines().any(|line| line == expected),
        "{label} lacks expected line `{expected}`"
    );
    Ok(())
}

/// Build a command argument vector from static string slices.
fn arg_list(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect()
}

/// Spawn one command with inherited environment plus selected overrides.
fn build_command(
    program: &OsStr,
    args: &[String],
    cwd: &Path,
    envs: &[(&str, OsString)],
    removed_env: &[&str],
) -> Command {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    for (key, value) in envs {
        command.env(key, value);
    }
    for key in removed_env {
        command.env_remove(key);
    }
    command
}

/// Capture one process result without exposing child output in an error.
fn run_process(
    program: &OsStr,
    args: &[String],
    cwd: &Path,
    envs: &[(&str, OsString)],
    removed_env: &[&str],
) -> Result<Output> {
    build_command(program, args, cwd, envs, removed_env)
        .output()
        .with_context(|| format!("spawn {}", program.to_string_lossy()))
}

/// Capture one process result after feeding bounded input to stdin.
fn run_process_with_input(
    program: &OsStr,
    args: &[String],
    cwd: &Path,
    envs: &[(&str, OsString)],
    removed_env: &[&str],
    input: &[u8],
) -> Result<Output> {
    let mut command = build_command(program, args, cwd, envs, removed_env);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", program.to_string_lossy()))?;
    let mut stdin = child
        .stdin
        .take()
        .context("open child stdin for release smoke input")?;
    stdin
        .write_all(input)
        .context("write release smoke input")?;
    drop(stdin);
    child
        .wait_with_output()
        .with_context(|| format!("wait for {}", program.to_string_lossy()))
}

/// Run one process and require a successful exit status.
fn run_checked(
    program: &OsStr,
    args: &[String],
    cwd: &Path,
    envs: &[(&str, OsString)],
    removed_env: &[&str],
) -> Result<Output> {
    let output = run_process(program, args, cwd, envs, removed_env)?;
    ensure!(
        output.status.success(),
        "{} failed with status {}",
        program.to_string_lossy(),
        status_label(&output)
    );
    Ok(output)
}

/// Format one process exit status without including potentially sensitive output.
fn status_label(output: &Output) -> String {
    output
        .status
        .code()
        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
}

/// Combine child stdout and stderr for bounded, local marker checks.
fn combined_output(output: &Output) -> String {
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use anyhow::{Context, Result};

    /// Reject a release target the rehearsal does not build.
    #[test]
    fn release_metadata_rejects_an_unknown_target() -> Result<()> {
        let root = repo_root()?;
        let error =
            validate_release_metadata(&root, hya_version::VERSION, "x86_64-pc-windows-msvc")
                .expect_err("unlisted target accepted");
        assert!(error.to_string().contains("release target"), "{error:#}");
        Ok(())
    }

    /// Canonical release tags strictly use `vx.x.x` with an optional `-<prerelease>` suffix.
    #[test]
    fn release_tag_grammar_requires_v_prefix_and_pure_semver() {
        for valid in ["v0.47.0", "v0.47.0-dev.1", "v1.2.3-rc.2"] {
            assert!(is_release_tag(valid), "expected valid tag `{valid}`");
        }
        for invalid in [
            "0.47.0",
            "0.45.10-dev.1",
            "backend/0.45.11",
            "frontend/0.44.22",
            "frontend/0.44.23-dev.5",
            "phase1",
            "v0.47.0+build.1",
            "v1.2",
        ] {
            assert!(!is_release_tag(invalid), "expected invalid tag `{invalid}`");
        }
    }

    /// The pinned Bun must be able to read every packaged program's lockfile.
    #[test]
    fn packaged_lockfiles_are_readable_by_the_pinned_bun() -> Result<()> {
        validate_bun_lockfile(&repo_root()?)?;
        let error = parse_bun_lockfile_version("{\n  \"lockfileVersion\": 3,\n}")
            .and_then(|version| require_supported_bun_lockfile(TUI_RUNTIME.source, version))
            .expect_err("a lockfile from a newer Bun was accepted");
        assert!(error.to_string().contains(BUN_VERSION), "{error:#}");
        assert!(error.to_string().contains(TUI_RUNTIME.source), "{error:#}");
        Ok(())
    }

    /// Every release target maps to the OpenTUI native package its runner installs.
    #[test]
    fn every_release_target_has_an_opentui_native_package() -> Result<()> {
        for target in RELEASE_TARGETS {
            let package = opentui_native_package(target)?;
            ensure!(package.starts_with("@opentui/core-"), "{package}");
            ensure!(
                !package.contains("musl"),
                "{target} is a glibc target: {package}"
            );
        }
        assert_eq!(
            opentui_native_package("aarch64-apple-darwin")?,
            "@opentui/core-darwin-arm64"
        );
        assert!(opentui_native_package("x86_64-apple-darwin").is_err());
        assert!(opentui_native_package("x86_64-pc-windows-msvc").is_err());
        Ok(())
    }

    /// The release metadata uses one unified `hya` aggregate version and placeholder references.
    #[test]
    fn version_contract_uses_unified_hya_aggregate() -> Result<()> {
        let root = repo_root()?;
        let (version, minimum_backend_version) = aggregate_versions(&root)?;
        assert_eq!(version, hya_version::VERSION);
        assert_eq!(
            minimum_backend_version,
            hya_version::MINIMUM_BACKEND_VERSION
        );
        assert!(release_version_at_least(
            &version,
            &minimum_backend_version
        )?);

        let backend = read_text(&root, "crates/hya-backend/Cargo.toml")?;
        assert!(backend.contains("version.workspace = true"));
        let component = read_text(&root, "crates/hya-provider/Cargo.toml")?;
        assert!(component.contains("version = \"0.0.0\""));
        assert!(component.contains("version-reference = \"hya\""));

        let source = read_text(&root, "packages/hya-tui/frontend-version.ts")?;
        ensure!(
            source.contains(&format!("export const frontendVersion = \"{version}\"")),
            "packages/hya-tui/frontend-version.ts does not carry unified version {version}"
        );
        ensure!(
            source.contains(&format!(
                "export const minimumBackendVersion = \"{minimum_backend_version}\""
            )),
            "packages/hya-tui/frontend-version.ts does not carry minimum backend version {minimum_backend_version}"
        );
        validate_newest_changelog(&read_text(&root, "CHANGELOG.md")?, "CHANGELOG.md", &version)?;

        for path in [
            "packages/hya-code-mode/package.json",
            "packages/hya-tui/package.json",
            "packages/hya-tui-web/package.json",
            "crates/hya-plugin-bun/adapter/package.json",
            "plugins/disk-inspector/package.json",
        ] {
            let package: serde_json::Value = serde_json::from_str(&read_text(&root, path)?)?;
            ensure!(
                package.get("version").and_then(serde_json::Value::as_str) == Some("0.0.0"),
                "{path} must use placeholder version 0.0.0"
            );
            ensure!(
                package["hya"]["version-reference"] == "hya",
                "{path} must reference the unified hya aggregate"
            );
        }
        Ok(())
    }

    /// Each packaged program's copied files and directories exist in the workspace.
    #[test]
    fn packaged_runtime_sources_exist() -> Result<()> {
        let root = repo_root()?;
        for runtime in PACKAGED_RUNTIMES {
            for file in runtime.files {
                require_file(&root.join(runtime.source).join(file), "runtime source file")?;
            }
            for directory in runtime.directories {
                let path = root.join(runtime.source).join(directory);
                ensure!(
                    path.is_dir(),
                    "runtime source directory {} is missing",
                    path.display()
                );
            }
        }
        Ok(())
    }

    /// Write a minimal staged TUI/WebUI tree that passes [`verify_tui_layout`].
    fn write_tui_fixture(package_root: &Path, target: &str) -> Result<()> {
        let native = opentui_native_package(target)?;
        copy_or_write(&package_root.join("lib/hya/tui/frontend-version.ts"))?;
        let tui = package_root.join(TUI_RUNTIME.destination);
        for file in TUI_RUNTIME.files.iter().chain(&TUI_RUNTIME_FILES) {
            copy_or_write(&tui.join(file))?;
        }
        for dependency in TUI_DEPENDENCIES.iter().chain([&native]) {
            copy_or_write(
                &tui.join("node_modules")
                    .join(dependency)
                    .join("package.json"),
            )?;
        }
        let web = package_root.join(TUI_WEB_RUNTIME.destination);
        for file in TUI_WEB_RUNTIME.files.iter().chain(&TUI_WEB_RUNTIME_FILES) {
            copy_or_write(&web.join(file))?;
        }
        for dependency in TUI_WEB_DEPENDENCIES {
            copy_or_write(
                &web.join("node_modules")
                    .join(dependency)
                    .join("package.json"),
            )?;
        }
        let sdk = package_root.join(TUI_SDK_RUNTIME.destination);
        for file in TUI_SDK_RUNTIME.files.iter().chain(&TUI_SDK_RUNTIME_FILES) {
            copy_or_write(&sdk.join(file))?;
        }
        for dependency in TUI_SDK_DEPENDENCIES {
            copy_or_write(
                &sdk.join("node_modules")
                    .join(dependency)
                    .join("package.json"),
            )?;
        }
        Ok(())
    }

    /// Create one empty fixture file and its parents.
    fn copy_or_write(path: &Path) -> Result<()> {
        fs::create_dir_all(path.parent().context("fixture path has no parent")?)?;
        fs::write(path, b"{}").with_context(|| format!("write {}", path.display()))
    }

    /// The layout check requires the host's native package and rejects dev-only packages.
    #[test]
    fn tui_layout_requires_native_package_and_rejects_dev_dependencies() -> Result<()> {
        let target = "aarch64-apple-darwin";
        let scratch = ScratchDirectory::create()?;
        let root = scratch.path();
        write_tui_fixture(root, target)?;
        verify_tui_layout(root, target)?;

        let error = verify_tui_layout(root, "x86_64-unknown-linux-gnu")
            .expect_err("a tree without the target's native package was accepted");
        assert!(
            error.to_string().contains("@opentui/core-linux-x64"),
            "{error:#}"
        );

        let playwright = root.join("lib/hya/tui-web/node_modules/@playwright/test");
        fs::create_dir_all(&playwright)?;
        let error = verify_tui_layout(root, target).expect_err("Playwright was accepted");
        assert!(error.to_string().contains("@playwright/test"), "{error:#}");
        fs::remove_dir_all(&playwright)?;

        let quickjs = root.join("lib/hya/tui-sdk/node_modules/@jitl/quickjs-wasmfile-release-sync");
        fs::remove_dir_all(&quickjs)?;
        let error = verify_tui_layout(root, target)
            .expect_err("an extension host without the QuickJS VM was accepted");
        assert!(
            error.to_string().contains("quickjs-wasmfile-release-sync"),
            "{error:#}"
        );
        copy_or_write(&quickjs.join("package.json"))?;

        fs::remove_file(root.join("lib/hya/tui-web/web/index.html"))?;
        let error = verify_tui_layout(root, target).expect_err("a missing page was accepted");
        assert!(error.to_string().contains("index.html"), "{error:#}");
        Ok(())
    }

    /// The listen line and the page's bundled asset references parse as served by Bun.
    #[test]
    fn web_host_listen_line_and_page_assets_parse() {
        assert_eq!(
            parse_listen_address("hya-tui-web listening on http://127.0.0.1:49292/").as_deref(),
            Some("127.0.0.1:49292")
        );
        assert_eq!(parse_listen_address("starting"), None);
        let page = "<link rel=\"stylesheet\" crossorigin href=\"/chunk-a.css\"><script type=\"module\" crossorigin src=\"/chunk-b.js\"></script>";
        assert_eq!(page_asset_paths(page), vec!["/chunk-b.js", "/chunk-a.css"]);
    }

    /// HTTP bodies are returned for `200` only and chunked encoding is decoded.
    #[test]
    fn http_responses_decode_chunked_bodies_and_reject_errors() -> Result<()> {
        let plain = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_http_response(plain)?, "hello");
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n.xte\r\n2\r\nrm\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(chunked)?, ".xterm");
        assert!(parse_http_response("HTTP/1.1 404 Not Found\r\n\r\n").is_err());
        Ok(())
    }

    /// Unresolved-import reports keep Bun's diagnostic and its location only.
    #[test]
    fn unresolved_import_report_keeps_diagnostics() {
        let output = "2 | import openapi from \"../../../docs/protocol/openapi.json\"\n    ^\nerror: Could not resolve: \"../../../docs/protocol/openapi.json\"\n    at /x/lib/hya/tui/src/api.ts:2:21\n";
        assert_eq!(
            unresolved_imports(output),
            "error: Could not resolve: \"../../../docs/protocol/openapi.json\"\nat /x/lib/hya/tui/src/api.ts:2:21"
        );
    }

    /// A rehearsal builds and runs natively, so the target must be the host.
    #[test]
    fn rehearsal_target_must_match_the_host() {
        assert!(require_host_target("aarch64-apple-darwin", "aarch64-apple-darwin").is_ok());
        let error = require_host_target("x86_64-unknown-linux-gnu", "aarch64-apple-darwin")
            .expect_err("foreign target accepted");
        assert!(error.to_string().contains("host"), "{error:#}");
    }
}
