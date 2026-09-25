//! Rehearse the GitHub release workflow without creating or publishing a release.
//!
//! This command deliberately keeps the workflow as the packaging contract. It
//! validates the YAML and embedded shell first, then executes the same locked
//! build and asset layout in a temporary directory. No Git tag or provider
//! request is needed for any step.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use serde_norway::Value;

const ACTIONLINT_VERSION: &str = "1.7.12";
const BUN_VERSION: &str = "1.4.2";
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
const RELEASE_JOB: &str = "release";
/// Crates whose release `cdylib` the native tool-family bundles package.
const TOOL_FAMILY_CRATES: [&str; 5] = [
    "hya-base-tools",
    "hya-extended-tools",
    "hya-network-tools",
    "hya-channel-tools",
    "hya-todo-tools",
];
const BUILD_JOB: &str = "build";
const BUN_ADAPTER: &str = "crates/hya-plugin-bun/adapter";
const ARGUS_PACKAGE_SCRIPT: &str = "scripts/package-argus-example.sh";
const WORKFLOW_BUN_SOURCE_COPY: &str =
    "cp -R crates/hya-plugin-bun/adapter/src/. \"$bun_adapter/src/\"";
const WORKFLOW_TUI_SOURCE_COPY: &str = "cp -R packages/hya-tui/src/. \"$tui/src/\"";
const WORKFLOW_TUI_INSTALL: &str =
    "(cd \"$tui\" && \"$HOME/.bun/bin/bun\" install --frozen-lockfile --production)";
const WORKFLOW_TUI_WEB_SOURCE_COPY: &str = "cp -R packages/hya-tui-web/src/. \"$tui_web/src/\"";
const WORKFLOW_TUI_WEB_PAGE_COPY: &str = "cp -R packages/hya-tui-web/web/. \"$tui_web/web/\"";
const WORKFLOW_TUI_WEB_INSTALL: &str =
    "(cd \"$tui_web\" && \"$HOME/.bun/bin/bun\" install --frozen-lockfile --production)";
const WORKFLOW_FIRST_PARTY_STAGE: &str = "cargo run --locked -p xtask -- stage-first-party-bundles --target \"$TARGET\" --version \"$version\" --library-dir \"target/$TARGET/release\" --package-root \"dist/$package_dir\" --assets dist";
const WORKFLOW_CHECKSUMS: &str =
    "(cd dist && shasum -a 256 \"$archive\" hya-*.hyabundle > \"SHA256SUMS-$TARGET\")";

/// One Bun program the archive ships under `lib/hya` with production dependencies.
struct BunRuntime {
    /// Workspace directory the program is copied from.
    source: &'static str,
    /// Package-relative directory it is installed into.
    destination: &'static str,
    /// Top-level files copied as-is.
    files: &'static [&'static str],
    /// Directories copied recursively.
    directories: &'static [&'static str],
}

/// The Bun extension adapter for `kind: bun` plugins.
const BUN_ADAPTER_RUNTIME: BunRuntime = BunRuntime {
    source: BUN_ADAPTER,
    destination: "lib/hya/bun-adapter",
    files: &["package.json", "bun.lock"],
    directories: &["src"],
};

/// The OpenTUI terminal UI bare `hya` runs.
const TUI_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-tui",
    destination: "lib/hya/tui",
    files: &["package.json", "bun.lock", "bunfig.toml", "tsconfig.json"],
    directories: &["src"],
};

/// The WebUI host that serves the TUI to the browser over a PTY.
const TUI_WEB_RUNTIME: BunRuntime = BunRuntime {
    source: "packages/hya-tui-web",
    destination: "lib/hya/tui-web",
    files: &["package.json", "bun.lock", "tsconfig.json"],
    directories: &["src", "web"],
};

/// Every Bun program in the release archive, in staging order.
const PACKAGED_RUNTIMES: [&BunRuntime; 3] = [&BUN_ADAPTER_RUNTIME, &TUI_RUNTIME, &TUI_WEB_RUNTIME];

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
/// Files the staged programs load at runtime beyond their copied sources.
const TUI_RUNTIME_FILES: [&str; 1] = ["src/main.ts"];
const TUI_WEB_RUNTIME_FILES: [&str; 6] = [
    "src/main.ts",
    "src/frames.ts",
    "web/index.html",
    "web/client.ts",
    "web/style.css",
    "node_modules/@xterm/xterm/css/xterm.css",
];

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
    workflow: PathBuf,
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
/// Returns an error when arguments, workflow structure, release metadata, or
/// any release command fails. The command never performs a publishing action.
pub fn run(args: Vec<String>) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(());
    }
    let options = parse_args(&args)?;
    let root = repo_root()?;
    let workflow_path = resolve_workflow_path(&root, &options.workflow);
    let workflow_source = fs::read_to_string(&workflow_path)
        .with_context(|| format!("read release workflow {}", workflow_path.display()))?;
    let workflow: Value = serde_norway::from_str(&workflow_source)
        .with_context(|| format!("parse release workflow {} as YAML", workflow_path.display()))?;
    let run_blocks = validate_workflow(&workflow, &options.target)?;
    validate_release_metadata(&root, &options.version, &options.target, &workflow)?;
    require_host_target(&options.target, &host_target(&root)?)?;

    run_actionlint(&workflow_path, &root)?;
    for (index, script) in run_blocks.iter().enumerate() {
        check_bash_syntax(index + 1, script)?;
    }
    prepare_and_build(&root, &options.target)?;
    rehearse_package(&root, &options.version, &options.target)?;

    println!(
        "release-rehearsal: ok — version {}, target {}, no publish",
        options.version, options.target
    );
    Ok(())
}

/// Print the command usage without touching the repository or running tools.
fn print_usage() {
    println!(
        "usage: cargo xtask release-rehearsal --workflow <path> \
         --version <semver> --target <target> --no-publish"
    );
}

/// Parse and validate the release rehearsal command-line arguments.
fn parse_args(args: &[String]) -> Result<Options> {
    let mut workflow = None;
    let mut version = None;
    let mut target = None;
    let mut no_publish = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--workflow" => {
                workflow = Some(next_argument(args, &mut index, "--workflow")?);
            }
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
    let workflow = workflow.context("release-rehearsal requires --workflow <path>")?;
    let version = version.context("release-rehearsal requires --version <semver>")?;
    let target = target.context("release-rehearsal requires --target <target>")?;
    ensure!(is_safe_target(&target), "invalid release target `{target}`");

    Ok(Options {
        workflow: PathBuf::from(workflow),
        version,
        target,
    })
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

/// Resolve a workflow path relative to the workspace root, as CI does.
fn resolve_workflow_path(root: &Path, workflow: &Path) -> PathBuf {
    if workflow.is_absolute() {
        workflow.to_path_buf()
    } else {
        root.join(workflow)
    }
}

/// Return the workspace root from this crate's manifest directory.
fn repo_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("resolve workspace root from CARGO_MANIFEST_DIR")
}

/// Validate the typed workflow shape and return every embedded `run` block.
fn validate_workflow(workflow: &Value, target: &str) -> Result<Vec<String>> {
    let root = mapping(workflow, "workflow root")?;
    let jobs = mapping(
        field(root, "jobs").context("release workflow must contain jobs")?,
        "workflow jobs",
    )?;
    ensure!(
        !jobs.is_empty(),
        "release workflow must contain at least one job"
    );

    let root_env = mapping(
        field(root, "env").context("release workflow must contain top-level env")?,
        "workflow env",
    )?;
    ensure_string_field(root_env, "BINARY_NAME", BINARY_NAME, "workflow env")?;

    for (job_name, job) in jobs {
        let job_label = key_label(job_name, "workflow job")?;
        let job_map = mapping(job, &format!("job `{job_label}`"))?;
        let steps = sequence(
            field(job_map, "steps")
                .with_context(|| format!("job `{job_label}` must contain steps"))?,
            &format!("job `{job_label}` steps"),
        )?;
        ensure!(
            !steps.is_empty(),
            "job `{job_label}` must contain at least one step"
        );
        for (step_index, step) in steps.iter().enumerate() {
            mapping(step, &format!("job `{job_label}` step {}", step_index + 1))?;
        }
    }

    let build = mapping(
        field(jobs, BUILD_JOB).context("release workflow must contain build job")?,
        "build job",
    )?;
    validate_build_matrix(build, target)?;
    let release = mapping(
        field(jobs, RELEASE_JOB).context("release workflow must contain release job")?,
        "release job",
    )?;
    ensure_string_value(
        field(release, "environment").context("release job must declare environment")?,
        "release",
        "release job environment",
    )?;

    let mut run_blocks = Vec::new();
    collect_step_contracts(workflow, "workflow", &mut run_blocks)?;
    ensure!(
        run_blocks.iter().any(
            |run| run.contains("cargo build --release --locked -p hya-backend --bins --target")
        ),
        "release workflow must keep the locked hya target build command"
    );
    ensure!(
        run_blocks.iter().any(|run| run.contains("shasum -a 256")),
        "release workflow must keep SHA256SUMS generation"
    );
    ensure!(
        run_blocks
            .iter()
            .any(|run| run.contains(ARGUS_PACKAGE_SCRIPT)),
        "release workflow must keep the existing example package script"
    );
    ensure_workflow_run_contract(
        &run_blocks,
        WORKFLOW_BUN_SOURCE_COPY,
        "recursively copy the complete Bun adapter source tree",
    )?;
    for (marker, purpose) in [
        (
            WORKFLOW_TUI_SOURCE_COPY,
            "recursively copy the complete TUI source tree",
        ),
        (
            WORKFLOW_TUI_INSTALL,
            "install the TUI's production dependencies on the target runner",
        ),
        (
            WORKFLOW_TUI_WEB_SOURCE_COPY,
            "recursively copy the complete WebUI host source tree",
        ),
        (
            WORKFLOW_TUI_WEB_PAGE_COPY,
            "recursively copy the complete WebUI page tree",
        ),
        (
            WORKFLOW_TUI_WEB_INSTALL,
            "install the WebUI host's production dependencies",
        ),
    ] {
        ensure_workflow_run_contract(&run_blocks, marker, purpose)?;
    }
    ensure_workflow_run_contract(
        &run_blocks,
        WORKFLOW_FIRST_PARTY_STAGE,
        "stage the twelve first-party bundles into the archive and as release assets",
    )?;
    ensure_workflow_run_contract(
        &run_blocks,
        WORKFLOW_CHECKSUMS,
        "checksum the archive and every first-party bundle asset",
    )?;
    Ok(run_blocks)
}

/// Require the build job to run one native job per release target.
///
/// The matrix must list exactly [`RELEASE_TARGETS`], each with a runner, and
/// the job must take `TARGET` and `runs-on` from the matrix entry.
fn validate_build_matrix(build: &serde_norway::Mapping, target: &str) -> Result<()> {
    ensure_string_value(
        field(build, "runs-on").context("build job must declare runs-on")?,
        "${{ matrix.runner }}",
        "build job runs-on",
    )?;
    let job_env = mapping(
        field(build, "env").context("build job must declare env")?,
        "build job env",
    )?;
    ensure_string_field(job_env, "TARGET", "${{ matrix.target }}", "build job env")?;
    let strategy = mapping(
        field(build, "strategy").context("build job must declare a target matrix")?,
        "build job strategy",
    )?;
    let matrix = mapping(
        field(strategy, "matrix").context("build job strategy must declare a matrix")?,
        "build job matrix",
    )?;
    let include = sequence(
        field(matrix, "include").context("build job matrix must list include entries")?,
        "build job matrix include",
    )?;
    let mut targets = Vec::with_capacity(include.len());
    for (index, entry) in include.iter().enumerate() {
        let location = format!("build job matrix include[{index}]");
        let entry = mapping(entry, &location)?;
        let entry_target = string_value(
            field(entry, "target").with_context(|| format!("{location} lacks target"))?,
            &location,
        )?;
        let runner = string_value(
            field(entry, "runner").with_context(|| format!("{location} lacks runner"))?,
            &location,
        )?;
        ensure!(!runner.is_empty(), "{location} has an empty runner");
        targets.push(entry_target.to_owned());
    }
    let mut expected = RELEASE_TARGETS.map(str::to_owned).to_vec();
    expected.sort();
    targets.sort();
    ensure!(
        targets == expected,
        "build job matrix must build exactly {expected:?}, found {targets:?}"
    );
    ensure!(
        RELEASE_TARGETS.contains(&target),
        "release target `{target}` is not in the build job matrix {expected:?}"
    );
    Ok(())
}

/// Require one exact shell marker in the parsed release workflow.
///
/// `run_blocks` contains parsed workflow scripts, `marker` is the required
/// trimmed command line, and `purpose` describes the actionable failure.
/// Returns an error when no script contains that exact command line.
fn ensure_workflow_run_contract(run_blocks: &[String], marker: &str, purpose: &str) -> Result<()> {
    ensure!(
        run_blocks
            .iter()
            .flat_map(|run| run.lines())
            .any(|line| line.trim() == marker),
        "release workflow must {purpose}: `{marker}`"
    );
    Ok(())
}

/// Recursively inspect parsed workflow maps for action pins and shell blocks.
fn collect_step_contracts(
    value: &Value,
    location: &str,
    run_blocks: &mut Vec<String>,
) -> Result<()> {
    match value {
        Value::Mapping(map) => {
            for (key, child) in map {
                let key = key_label(key, location)?;
                let child_location = format!("{location}.{key}");
                match key.as_str() {
                    "uses" => validate_action_pin(child, &child_location)?,
                    "run" => {
                        let script = string_value(child, &child_location)?;
                        run_blocks.push(script.to_owned());
                    }
                    _ => {}
                }
                collect_step_contracts(child, &child_location, run_blocks)?;
            }
        }
        Value::Sequence(sequence) => {
            for (index, child) in sequence.iter().enumerate() {
                collect_step_contracts(child, &format!("{location}[{index}]"), run_blocks)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {}
    }
    Ok(())
}

/// Require a third-party GitHub action to use a full immutable commit SHA.
fn validate_action_pin(value: &Value, location: &str) -> Result<()> {
    let action = string_value(value, location)?;
    if action.starts_with("./") {
        return Ok(());
    }
    let (name, pin) = action
        .rsplit_once('@')
        .with_context(|| format!("{location} must use an immutable commit SHA"))?;
    ensure!(!name.is_empty(), "{location} has an empty action name");
    ensure!(
        pin.len() == 40 && pin.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{location} action `{action}` is not pinned to a 40-character commit SHA"
    );
    Ok(())
}

/// Return a YAML mapping or report the expected structural location.
fn mapping<'a>(value: &'a Value, location: &str) -> Result<&'a serde_norway::Mapping> {
    match value {
        Value::Mapping(map) => Ok(map),
        _ => bail!("{location} must be a mapping"),
    }
}

/// Return a YAML sequence or report the expected structural location.
fn sequence<'a>(value: &'a Value, location: &str) -> Result<&'a Vec<Value>> {
    match value {
        Value::Sequence(sequence) => Ok(sequence),
        _ => bail!("{location} must be a sequence"),
    }
}

/// Look up one string key in a YAML mapping.
fn field<'a>(map: &'a serde_norway::Mapping, key: &str) -> Option<&'a Value> {
    map.get(key)
}

/// Convert one YAML map key to a bounded diagnostic label.
fn key_label(value: &Value, location: &str) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        _ => bail!("{location} contains a non-string key"),
    }
}

/// Return a YAML string value with a structural error context.
fn string_value<'a>(value: &'a Value, location: &str) -> Result<&'a str> {
    match value {
        Value::String(value) => Ok(value),
        _ => bail!("{location} must be a string"),
    }
}

/// Require one mapping field to equal an expected string.
fn ensure_string_field(
    map: &serde_norway::Mapping,
    key: &str,
    expected: &str,
    location: &str,
) -> Result<()> {
    let value = field(map, key).with_context(|| format!("{location} must contain {key}"))?;
    ensure_string_value(value, expected, &format!("{location}.{key}"))
}

/// Require one YAML value to equal an expected string.
fn ensure_string_value(value: &Value, expected: &str, location: &str) -> Result<()> {
    let actual = string_value(value, location)?;
    ensure!(
        actual == expected,
        "{location} must be `{expected}`, found `{actual}`"
    );
    Ok(())
}

/// Run the pinned actionlint executable from `PATH` against the workflow.
fn run_actionlint(workflow: &Path, root: &Path) -> Result<()> {
    let version = run_process(
        OsStr::new("actionlint"),
        &arg_list(&["-version"]),
        root,
        &[],
        &[],
    )
    .context("run actionlint -version from PATH")?;
    ensure!(
        version.status.success(),
        "actionlint -version failed with status {}",
        status_label(&version)
    );
    let version_text = combined_output(&version);
    ensure!(
        version_text.contains(ACTIONLINT_VERSION),
        "actionlint from PATH must report version {ACTIONLINT_VERSION}"
    );

    let output = run_process(
        OsStr::new("actionlint"),
        &[workflow.display().to_string()],
        root,
        &[],
        &[],
    )
    .with_context(|| format!("run actionlint on {}", workflow.display()))?;
    ensure!(
        output.status.success(),
        "actionlint rejected {} with status {}",
        workflow.display(),
        status_label(&output)
    );
    Ok(())
}

/// Syntax-check one embedded workflow shell block with `bash -n`.
fn check_bash_syntax(index: usize, script: &str) -> Result<()> {
    let output = run_process_with_input(
        OsStr::new("bash"),
        &arg_list(&["-n"]),
        Path::new("."),
        &[],
        &[],
        script.as_bytes(),
    )
    .with_context(|| format!("syntax-check workflow run block {index} with bash -n"))?;
    ensure!(
        output.status.success(),
        "workflow run block {index} failed bash -n"
    );
    Ok(())
}

/// Validate the semver, version files, and newest-only changelog contract.
fn validate_release_metadata(
    root: &Path,
    version: &str,
    target: &str,
    workflow: &Value,
) -> Result<()> {
    ensure!(
        is_semver(version),
        "release version `{version}` is not semver-shaped"
    );
    let representative_tag = format!("v{version}");
    ensure!(
        representative_tag.strip_prefix('v') == Some(version),
        "representative release tag does not match version `{version}`"
    );
    validate_release_tag_trigger(workflow, &representative_tag)?;
    ensure!(
        RELEASE_TARGETS.contains(&target),
        "release target `{target}` is not in the build job matrix {RELEASE_TARGETS:?}"
    );

    let manifest_path = root.join("Cargo.toml");
    let manifest_source = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&manifest_source)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
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

    let readme = read_text(root, "README.md")?;
    ensure!(
        readme.contains(&format!("workspace version `{version}`")),
        "README.md does not report workspace version `{version}`"
    );

    let lockfile = read_text(root, "Cargo.lock")?;
    validate_lockfile_versions(&lockfile, version)?;
    validate_bun_lockfile(root)?;

    let changelog = read_text(root, "CHANGELOG.md")?;
    let first_heading = changelog.lines().find(|line| line.starts_with("# "));
    let expected_heading = format!("# {version}");
    ensure!(
        first_heading == Some(expected_heading.as_str()),
        "CHANGELOG.md first heading must be `{expected_heading}`"
    );
    if let Some(heading) = changelog
        .lines()
        .filter(|line| line.starts_with("# "))
        .nth(1)
    {
        bail!("CHANGELOG.md must be newest-only; found extra heading `{heading}`");
    }
    Ok(())
}

/// Require the workflow push trigger to admit the representative release tag.
fn validate_release_tag_trigger(workflow: &Value, tag: &str) -> Result<()> {
    let root = mapping(workflow, "workflow root")?;
    let triggers = mapping(
        field(root, "on").context("release workflow must contain an on trigger")?,
        "workflow on trigger",
    )?;
    let push = mapping(
        field(triggers, "push").context("workflow on trigger must contain push")?,
        "workflow on.push trigger",
    )?;
    let tags = sequence(
        field(push, "tags").context("workflow on.push trigger must contain tags")?,
        "workflow on.push.tags",
    )?;
    let mut admitted = false;
    for (index, pattern) in tags.iter().enumerate() {
        let pattern = string_value(pattern, &format!("workflow on.push.tags[{index}"))?;
        if tag_pattern_matches(pattern, tag) {
            admitted = true;
            break;
        }
    }
    ensure!(
        admitted,
        "workflow on.push.tags does not admit representative release tag `{tag}`"
    );
    Ok(())
}

/// Match the small `*` glob syntax used by GitHub tag filters.
fn tag_pattern_matches(pattern: &str, tag: &str) -> bool {
    let mut remainder = tag;
    let mut first_literal = true;
    for literal in pattern.split('*').filter(|literal| !literal.is_empty()) {
        if first_literal {
            if !remainder.starts_with(literal) {
                return false;
            }
            remainder = &remainder[literal.len()..];
            first_literal = false;
        } else if let Some(index) = remainder.find(literal) {
            remainder = &remainder[index + literal.len()..];
        } else {
            return false;
        }
    }
    pattern.ends_with('*') || remainder.is_empty()
}

/// Validate that every hya workspace package in the lockfile uses one version.
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
        ensure!(
            package_version == version,
            "Cargo.lock package {name} has version `{package_version}`, expected `{version}`"
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

/// Check the pinned Bun (used by the Bun adapter packaging) and run the
/// exact locked target release build.
fn prepare_and_build(root: &Path, target: &str) -> Result<()> {
    let bun_version = run_checked(OsStr::new("bun"), &arg_list(&["--version"]), root, &[], &[])?;
    ensure!(
        String::from_utf8_lossy(&bun_version.stdout).trim() == BUN_VERSION,
        "Bun from PATH must report version {BUN_VERSION}"
    );

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
    for family in TOOL_FAMILY_CRATES {
        libraries.extend(["-p".to_owned(), family.to_owned()]);
    }
    libraries.extend(["--lib".to_owned(), "--target".to_owned(), target.to_owned()]);
    run_checked(OsStr::new("cargo"), &libraries, root, &[], &[])
        .context("run locked release build for the native tool libraries")?;
    Ok(())
}

/// Reproduce the workflow archive, checksum, extraction, and smoke checks.
fn rehearse_package(root: &Path, version: &str, target: &str) -> Result<()> {
    let scratch = ScratchDirectory::create()?;
    let dist = scratch.path().join("dist");
    fs::create_dir_all(&dist).with_context(|| format!("create {}", dist.display()))?;
    let package_name = format!("{BINARY_NAME}-{version}-{target}");
    let package_root = dist.join(&package_name);
    let bin = package_root.join("bin");
    fs::create_dir_all(&bin).with_context(|| format!("create {}", bin.display()))?;

    let backend_source = root.join("target").join(target).join("release").join("hya");
    let backend_destination = bin.join("hya");
    copy_file(&backend_source, &backend_destination)?;
    set_executable(&backend_destination)?;
    copy_file(&root.join("README.md"), &package_root.join("README.md"))?;

    for runtime in PACKAGED_RUNTIMES {
        let destination = package_root.join(runtime.destination);
        stage_bun_runtime(root, runtime, &destination)?;
        install_runtime_dependencies(runtime, &destination)?;
    }

    let example = package_root.join("examples/hya-argus-example.hyabundle");
    fs::create_dir_all(example.parent().context("example archive has no parent")?)
        .context("create example archive directory")?;
    package_argus_example(root, &example)?;
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

    let staged = crate::first_party_release::stage(&crate::first_party_release::StageOptions {
        version: version.to_owned(),
        library_dir: root.join("target").join(target).join("release"),
        package_root: package_root.clone(),
        target: Some(target.to_owned()),
        assets: Some(dist.clone()),
    })
    .context("stage first-party bundles like the release workflow")?;
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
    verify_tui_imports_resolve(&extracted, &scratch)?;
    Ok(())
}

/// Copy one Bun program's manifest files and source trees into the package.
fn stage_bun_runtime(root: &Path, runtime: &BunRuntime, destination: &Path) -> Result<()> {
    let source = root.join(runtime.source);
    fs::create_dir_all(destination)
        .with_context(|| format!("create packaged runtime {}", destination.display()))?;
    for file in runtime.files {
        copy_file(&source.join(file), &destination.join(file))?;
    }
    for directory in runtime.directories {
        copy_directory_contents(&source.join(directory), &destination.join(directory))?;
    }
    Ok(())
}

/// Install production dependencies in one packaged Bun program.
fn install_runtime_dependencies(runtime: &BunRuntime, destination: &Path) -> Result<()> {
    run_checked(
        OsStr::new("bun"),
        &arg_list(&["install", "--frozen-lockfile", "--production"]),
        destination,
        &[],
        &[],
    )
    .with_context(|| format!("install {} production dependencies", runtime.destination))?;
    Ok(())
}

/// Use the repository's package writer for the release's example bundle.
fn package_argus_example(root: &Path, output: &Path) -> Result<()> {
    let script = root.join(ARGUS_PACKAGE_SCRIPT);
    let source = root.join("bundles/examples/argus-example");
    let args = vec![
        script.display().to_string(),
        source.display().to_string(),
        output.display().to_string(),
    ];
    run_checked(OsStr::new("bash"), &args, root, &[], &[])
        .context("package Argus example with the release package writer")?;
    Ok(())
}

/// Write `SHA256SUMS-<target>` for the archive and bundle assets, then verify it like CI.
fn write_and_verify_checksums(
    dist: &Path,
    target: &str,
    archive_name: &str,
    assets: &[String],
) -> Result<()> {
    let mut files = arg_list(&["-a", "256"]);
    files.push(archive_name.to_owned());
    files.extend(assets.iter().cloned());
    let checksum = run_checked(OsStr::new("shasum"), &files, dist, &[], &[])?;
    let sums_name = format!("SHA256SUMS-{target}");
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

    let bun_adapter = package_root.join("lib/hya/bun-adapter");
    for path in ["package.json", "bun.lock", "src/main.ts"] {
        require_file(&bun_adapter.join(path), "packaged Bun adapter file")?;
    }
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
        "lib/hya/bun-adapter/package.json",
        "lib/hya/bun-adapter/bun.lock",
        "lib/hya/bun-adapter/src/main.ts",
        "lib/hya/tui/package.json",
        "lib/hya/tui/bun.lock",
        "lib/hya/tui/src/main.ts",
        "lib/hya/tui-web/package.json",
        "lib/hya/tui-web/bun.lock",
        "lib/hya/tui-web/src/main.ts",
        "lib/hya/tui-web/web/index.html",
        "examples/hya-argus-example.hyabundle",
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
    for prefix in [
        "lib/hya/tui/node_modules/@opentui/core/",
        "lib/hya/tui-web/node_modules/@xterm/xterm/",
    ] {
        let prefix = format!("{package_name}/{prefix}");
        ensure!(
            listing.lines().any(|line| line.starts_with(&prefix)),
            "release tar listing lacks entries under `{prefix}`"
        );
    }
    ensure!(
        !listing.contains(&format!("{package_name}/bundles/examples/argus-example")),
        "release tar contains the example source-tree prefix"
    );
    Ok(())
}

/// Smoke packaged binaries and the pure adapter.
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

    smoke_bun_adapter(&package_root.join("lib/hya/bun-adapter"), scratch)?;
    smoke_tui_runtime(package_root, scratch)?;
    Ok(())
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

/// Run the Bun adapter initialize/shutdown handshake and verify its bounded output.
fn smoke_bun_adapter(bun_adapter: &Path, scratch: &ScratchDirectory) -> Result<()> {
    let probe = scratch.path().join("bun-adapter-probe");
    let output_path = scratch.path().join("bun-adapter-output");
    fs::create_dir_all(&probe).with_context(|| format!("create {}", probe.display()))?;
    let script = bun_adapter.join("src/main.ts");
    let args = vec!["run".to_owned(), script.display().to_string()];
    let envs = [
        ("HYA_DIRECTORY", probe.as_os_str().to_os_string()),
        ("HYA_WORKTREE", probe.as_os_str().to_os_string()),
    ];
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocol_version\":1,\"host\":{\"name\":\"hya\",\"version\":\"release\"}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n";
    let output = run_process_with_input(
        OsStr::new("bun"),
        &args,
        &probe,
        &envs,
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

/// Copy one source file and preserve a path-specific failure context.
fn copy_file(source: &Path, destination: &Path) -> Result<()> {
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

    /// Exact-line packaging contracts that must stay in the checked-in workflow.
    const WORKFLOW_CONTRACTS: &[&str] = &[
        WORKFLOW_BUN_SOURCE_COPY,
        WORKFLOW_TUI_SOURCE_COPY,
        WORKFLOW_TUI_INSTALL,
        WORKFLOW_TUI_WEB_SOURCE_COPY,
        WORKFLOW_TUI_WEB_PAGE_COPY,
        WORKFLOW_TUI_WEB_INSTALL,
        WORKFLOW_FIRST_PARTY_STAGE,
        WORKFLOW_CHECKSUMS,
    ];

    /// Require the checked-in workflow to satisfy every release package contract.
    #[test]
    fn validate_checked_in_workflow_release_contracts() -> Result<()> {
        let source = canonical_workflow_source()?;
        let workflow: Value = serde_norway::from_str(&source).context("parse release workflow")?;
        for target in RELEASE_TARGETS {
            validate_workflow(&workflow, target)?;
        }
        Ok(())
    }

    /// Reject a target the release matrix does not build.
    #[test]
    fn validate_workflow_rejects_a_target_outside_the_matrix() -> Result<()> {
        let workflow: Value =
            serde_norway::from_str(&canonical_workflow_source()?).context("parse workflow")?;
        let error = validate_workflow(&workflow, "x86_64-apple-darwin")
            .expect_err("unlisted target accepted");
        assert!(error.to_string().contains("matrix"), "{error:#}");
        Ok(())
    }

    /// The matrix must build exactly the targets the rehearsal supports.
    #[test]
    fn validate_workflow_rejects_a_matrix_missing_a_release_target() -> Result<()> {
        let source = canonical_workflow_source()?;
        let modified = source.replacen(
            "aarch64-unknown-linux-gnu",
            "riscv64gc-unknown-linux-gnu",
            1,
        );
        let workflow: Value = serde_norway::from_str(&modified).context("parse workflow")?;
        let error = validate_workflow(&workflow, "x86_64-unknown-linux-gnu")
            .expect_err("incomplete matrix accepted");
        assert!(error.to_string().contains("matrix"), "{error:#}");
        Ok(())
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
        assert!(opentui_native_package("x86_64-pc-windows-msvc").is_err());
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

    /// Require each independent release contract omission to fail closed.
    #[test]
    fn validate_workflow_rejects_each_missing_release_contract() -> Result<()> {
        let source = canonical_workflow_source()?;
        for &marker in WORKFLOW_CONTRACTS {
            let modified = source.replacen(marker, "", 1);
            ensure!(
                modified != source,
                "workflow fixture did not contain contract marker `{marker}`"
            );
            let workflow: Value = serde_norway::from_str(&modified)
                .with_context(|| format!("parse workflow fixture without `{marker}`"))?;
            let error = validate_workflow(&workflow, RELEASE_TARGETS[0])
                .expect_err("workflow validation accepted a missing release contract");
            assert!(
                error.to_string().contains(marker),
                "missing `{marker}` produced an unrelated error: {error:#}"
            );
        }
        Ok(())
    }

    /// Read the canonical workflow and require every exact contract marker.
    fn canonical_workflow_source() -> Result<String> {
        let source = fs::read_to_string(repo_root()?.join(".github/workflows/release.yml"))
            .context("read release workflow fixture")?;
        for &marker in WORKFLOW_CONTRACTS {
            ensure!(
                source.contains(marker),
                "checked-in release workflow is missing `{marker}`"
            );
        }
        Ok(source)
    }
}
