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
const BUN_VERSION: &str = "1.3.14";
const WORKFLOW_TARGET: &str = "x86_64-unknown-linux-gnu";
const BINARY_NAME: &str = "hya";
const RELEASE_JOB: &str = "release";
const BUILD_JOB: &str = "build";
const BUN_ADAPTER: &str = "crates/hya-plugin-bun/adapter";
const ARGUS_PACKAGE_SCRIPT: &str = "scripts/package-argus-example.sh";
const WORKFLOW_BUN_SOURCE_COPY: &str =
    "cp -R crates/hya-plugin-bun/adapter/src/. \"$bun_adapter/src/\"";

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
    ensure_string_field(root_env, "TARGET", target, "workflow env")?;

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

    mapping(
        field(jobs, BUILD_JOB).context("release workflow must contain build job")?,
        "build job",
    )?;
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
        "release workflow must keep the locked hya-backend target build command"
    );
    ensure!(
        run_blocks.iter().any(|run| run.contains("sha256sum")),
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
    Ok(run_blocks)
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
        target == WORKFLOW_TARGET,
        "release target must be `{WORKFLOW_TARGET}`"
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

    let backend_source = root
        .join("target")
        .join(target)
        .join("release")
        .join("hya-backend");
    let backend_destination = bin.join("hya-backend");
    copy_file(&backend_source, &backend_destination)?;
    set_executable(&backend_destination)?;
    copy_file(&root.join("README.md"), &package_root.join("README.md"))?;

    let bun_adapter = package_root.join("lib/hya/bun-adapter");
    copy_bun_runtime(root, &bun_adapter)?;
    install_runtime_dependencies(&bun_adapter)?;

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
        &[],
        &[],
    )
    .context("create release tar.gz archive")?;

    write_and_verify_checksums(&dist, &archive_name)?;

    verify_package_layout(&package_root)?;
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
    verify_package_layout(&extracted)?;
    verify_archive_listing(root, &archive, &package_name, &scratch)?;
    smoke_packaged_release(&extracted, &scratch, version)?;
    Ok(())
}

/// Copy the Bun adapter manifest and source tree into the package.
fn copy_bun_runtime(root: &Path, bun_adapter: &Path) -> Result<()> {
    let source = root.join(BUN_ADAPTER);
    fs::create_dir_all(bun_adapter.join("src"))
        .with_context(|| format!("create Bun adapter runtime {}", bun_adapter.display()))?;
    for file in ["package.json", "bun.lock"] {
        copy_file(&source.join(file), &bun_adapter.join(file))?;
    }
    copy_directory_contents(&source.join("src"), &bun_adapter.join("src"))
}

/// Install production dependencies in the packaged JavaScript runtime.
fn install_runtime_dependencies(bun_adapter: &Path) -> Result<()> {
    run_checked(
        OsStr::new("bun"),
        &arg_list(&["install", "--frozen-lockfile", "--production"]),
        bun_adapter,
        &[],
        &[],
    )
    .context("install Bun adapter runtime dependencies")?;
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

/// Generate `SHA256SUMS` and verify it through the same `sha256sum -c` path as CI.
fn write_and_verify_checksums(dist: &Path, archive_name: &str) -> Result<()> {
    let checksum = run_checked(
        OsStr::new("sha256sum"),
        &[archive_name.to_owned()],
        dist,
        &[],
        &[],
    )?;
    let sums = dist.join("SHA256SUMS");
    fs::write(&sums, &checksum.stdout)
        .with_context(|| format!("write checksum manifest {}", sums.display()))?;
    run_checked(
        OsStr::new("sha256sum"),
        &arg_list(&["-c", "SHA256SUMS"]),
        dist,
        &[],
        &[],
    )
    .context("verify release SHA256SUMS")?;
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
fn verify_package_layout(package_root: &Path) -> Result<()> {
    require_file(
        &package_root.join("bin").join("hya-backend"),
        "packaged binary",
    )?;

    let bun_adapter = package_root.join("lib/hya/bun-adapter");
    for path in ["package.json", "bun.lock", "src/main.ts"] {
        require_file(&bun_adapter.join(path), "packaged Bun adapter file")?;
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
        "bin/hya-backend",
        "lib/hya/bun-adapter/package.json",
        "lib/hya/bun-adapter/bun.lock",
        "lib/hya/bun-adapter/src/main.ts",
        "examples/hya-argus-example.hyabundle",
    ] {
        require_listing_line(
            &listing,
            &format!("{package_name}/{path}"),
            "release tar listing",
        )?;
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
    let backend = package_root.join("bin/hya-backend");
    let version_output = run_checked(
        backend.as_os_str(),
        &arg_list(&["--version"]),
        scratch.path(),
        &[],
        &[],
    )?;
    ensure!(
        combined_output(&version_output).contains(version),
        "packaged hya-backend --version did not report {version}"
    );
    run_checked(
        backend.as_os_str(),
        &arg_list(&["--help"]),
        scratch.path(),
        &[],
        &[],
    )
    .context("smoke packaged hya-backend --help")?;

    smoke_bun_adapter(&package_root.join("lib/hya/bun-adapter"), scratch)?;
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
    const WORKFLOW_CONTRACTS: &[&str] = &[WORKFLOW_BUN_SOURCE_COPY];

    /// Require the checked-in workflow to satisfy every release package contract.
    #[test]
    fn validate_checked_in_workflow_release_contracts() -> Result<()> {
        let source = canonical_workflow_source()?;
        let workflow: Value = serde_norway::from_str(&source).context("parse release workflow")?;
        validate_workflow(&workflow, WORKFLOW_TARGET)?;
        Ok(())
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
            let error = validate_workflow(&workflow, WORKFLOW_TARGET)
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
