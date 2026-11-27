use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use hya_app::bundle_admin::{self, InstallTarget, Removed, SelfCheck};
use hya_app::bundle_registry::{
    BundleInstallAction, BundleInstallPlan, BundleRegistry, BundleRegistryError,
    LoadedBundleRegistryRecord, NamespaceInstallPolicy,
};
use hya_app::project_bundles::{ProjectBundle, find_project_bundle, plan_project_install};
use hya_bundle::{
    Bundle, BundleSource, PreparedCatalog, PreparedProcessKind, PublicPackageInspection,
    SourceFile, inspect_public_package, prepare_package, write_public_package,
};
use hya_server::BundleListing;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

/// Where a bundle command reads or writes: `--user` (the installed-bundle
/// registry, the default for install/remove/verify) or `--project`
/// (`.hya/bundles` under the current directory). `list` and `info` show every
/// scope unless one is named.
#[derive(Args, Clone, Copy, Debug, Default)]
pub(crate) struct ScopeArgs {
    /// Project scope: bundle source directories under `./.hya/bundles`.
    #[arg(long, conflicts_with = "user")]
    project: bool,
    /// User scope: the installed-bundle registry (default).
    #[arg(long)]
    user: bool,
}

/// One concrete bundle scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    User,
    Project,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl ScopeArgs {
    /// Scope a write (or verify) targets: user unless `--project`.
    fn target(self) -> Scope {
        if self.project {
            Scope::Project
        } else {
            Scope::User
        }
    }

    /// Scope a read is narrowed to, or `None` for every scope.
    fn filter(self) -> Option<Scope> {
        if self.project {
            Some(Scope::Project)
        } else if self.user {
            Some(Scope::User)
        } else {
            None
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum BundleCommand {
    /// Install a bundle package or a Claude Code plugin source. Asks for
    /// confirmation unless `-y` is given.
    Install {
        /// Bundle package (`.hyabundle`); conflicts with `--claude`.
        #[arg(value_name = "PACKAGE")]
        package: Option<PathBuf>,
        /// Claude Code plugin source: a local directory or
        /// `<marketplace-root>#<entry>` translated into a standard bundle.
        #[arg(long, value_name = "SOURCE")]
        claude: Option<PathBuf>,
        /// Accept a namespace conflict (replacing the incumbent bundle) and
        /// same-bundle downgrades instead of failing.
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        scope: ScopeArgs,
        /// Install without asking for confirmation.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Enable a bundle globally.
    Enable { name: String },
    /// Disable a bundle globally; disabled bundles publish no components.
    Disable { name: String },
    /// Refresh edited bundles and config in the running backend.
    Refresh,
    /// List bundles with their scope (builtin, user, project).
    List {
        #[command(flatten)]
        scope: ScopeArgs,
    },
    /// Search bundles in every scope by a case-insensitive substring over
    /// bundle ids, agent ids, and skill ids.
    Search {
        /// Substring matched against bundle ids, agent ids, and skill ids
        /// (case-insensitive).
        #[arg(value_name = "QUERY", value_parser = parse_search_query)]
        query: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    /// Remove an installed bundle. Asks for confirmation unless `-y` is given.
    #[command(visible_alias = "uninstall")]
    Remove {
        /// Bundle id, for example `acme/tools`.
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
        /// Remove without asking for confirmation.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Deterministically pack a bundle source directory into a `.hyabundle` archive.
    Pack {
        /// Bundle source directory containing `bundle.yaml` or `bundle.hya.md`.
        #[arg(value_name = "SOURCE_DIR")]
        source: PathBuf,
        /// Output `.hyabundle` package path.
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,
    },
    /// Check that a package (`.hyabundle`) or source directory is valid and
    /// would install into a scope, without installing it.
    Verify {
        /// Bundle package (`.hyabundle`) or source directory.
        #[arg(value_name = "PACKAGE")]
        package: PathBuf,
        /// Check as `install --overwrite` would.
        #[arg(long)]
        overwrite: bool,
        /// Validate strict schema, descriptors, determinism, and secret hygiene.
        #[arg(long)]
        strict: bool,
        /// Spawn a process bundle in an isolated copy and verify JSON-RPC 2.0
        /// `initialize` (matching declared tools/hooks/apis) and `shutdown`.
        #[arg(long)]
        protocol_smoke: bool,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    /// Show bundle metadata by bundle id or from a package file.
    Info {
        /// Bundle id, or a path to a `.hyabundle` package file.
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        name: Option<String>,
        /// Package file to read.
        #[arg(
            short = 'f',
            long,
            value_name = "FILE",
            required_unless_present = "name",
            conflicts_with = "name"
        )]
        file: Option<PathBuf>,
        #[command(flatten)]
        scope: ScopeArgs,
    },
    /// Show the URI-scheme extensions one bundle declares.
    Schema {
        /// Bundle id, or a path to a `.hyabundle` package file.
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

/// clap value parser for `bundle search <QUERY>`: a query of only whitespace
/// cannot meaningfully substring-match, so reject it at parse time.
fn parse_search_query(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("QUERY must contain at least one non-whitespace character".to_string())
    } else {
        Ok(value.to_string())
    }
}

/// `db`: the backend database whose running daemon `install` activates the
/// bundle in.
pub(crate) async fn run(command: BundleCommand, db: String) -> anyhow::Result<()> {
    match command {
        BundleCommand::Install {
            package,
            claude,
            overwrite,
            scope,
            yes,
        } => install_dispatch(package, claude, policy(overwrite), scope.target(), yes, &db).await,
        BundleCommand::Enable { name } => set_enabled(&name, true, &db).await,
        BundleCommand::Disable { name } => set_enabled(&name, false, &db).await,
        BundleCommand::Refresh => refresh_daemon(&db, None).await,
        BundleCommand::List { scope } => list(scope.filter()).await,
        BundleCommand::Search { query, scope } => search(&query, scope.filter()).await,
        BundleCommand::Remove { name, scope, yes } => remove(&name, scope.target(), yes, &db).await,
        BundleCommand::Pack { source, output } => pack(&source, &output),
        BundleCommand::Verify {
            package,
            overwrite,
            strict,
            protocol_smoke,
            scope,
        } => {
            verify(
                &package,
                policy(overwrite),
                scope.target(),
                strict,
                protocol_smoke,
            )
            .await
        }
        BundleCommand::Info {
            name: Some(name),
            file: None,
            scope,
        } => {
            let as_file = Path::new(&name);
            if name.ends_with(".hyabundle") && as_file.is_file() {
                info_file(as_file)
            } else {
                info(&name, scope.filter()).await
            }
        }
        BundleCommand::Info {
            file: Some(package),
            ..
        } => info_file(&package),
        BundleCommand::Info { .. } => anyhow::bail!("bundle info requires a bundle name"),
        BundleCommand::Schema { name, scope } => schema(&name, scope.filter()).await,
    }
}

async fn set_enabled(bundle_id: &str, enabled: bool, db: &str) -> anyhow::Result<()> {
    bundle_admin::set_enabled(bundle_id, enabled).await?;
    println!(
        "{} {bundle_id}",
        if enabled { "enabled" } else { "disabled" }
    );
    refresh_daemon(db, Some(bundle_id)).await
}

/// Ask the running backend to refresh the base and the current Project now.
async fn refresh_daemon(db: &str, bundle_id: Option<&str>) -> anyhow::Result<()> {
    if let Some(found) = crate::daemon::running(db).await {
        let refreshed = reqwest::Client::new()
            .post(format!("{}/v1/bundles:refresh", found.url))
            .json(&serde_json::json!({ "directory": std::env::current_dir()
                .context("read the current directory")?.to_string_lossy() }))
            .send()
            .await
            .context("ask the backend to refresh its bundles")?
            .error_for_status()
            .context("backend rejected the bundle refresh")?
            .json::<hya_api::v1::RefreshBundlesResponse>()
            .await
            .context("read the backend bundle refresh response")?;
        report_refresh_errors(bundle_id, &refreshed.errors)?;
        if bundle_id.is_none() {
            println!("refreshed bundles: generation={}", refreshed.generation);
        }
    } else if bundle_id.is_none() {
        println!("no backend runs for {db}; nothing to refresh");
    }
    Ok(())
}

fn report_refresh_errors(
    id: Option<&str>,
    errors: &[hya_api::v1::BundleRefreshError],
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for error in errors {
        let message = format!("{}: {}", error.bundle_id, error.message);
        if id.is_none_or(|id| error.bundle_id == id) {
            failures.push(message);
        } else {
            eprintln!("warning: {message}");
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "backend bundle refresh failed:\n  {}",
        failures.join("\n  ")
    );
    Ok(())
}

fn policy(overwrite: bool) -> NamespaceInstallPolicy {
    if overwrite {
        NamespaceInstallPolicy::OverwriteConflicts
    } else {
        NamespaceInstallPolicy::DenyConflicts
    }
}

/// Route `bundle install` to the package or Claude plugin source path.
async fn install_dispatch(
    package: Option<PathBuf>,
    claude: Option<PathBuf>,
    policy: NamespaceInstallPolicy,
    scope: Scope,
    yes: bool,
    db: &str,
) -> anyhow::Result<()> {
    match (package, claude) {
        (Some(package), None) => install(&package, policy, scope, yes, db).await,
        (None, Some(source)) => {
            let staged = stage_claude_source(&source).await?;
            let result = install(&staged, policy, scope, yes, db).await;
            if let Err(error) = fs::remove_file(&staged) {
                eprintln!(
                    "hya: could not remove staged claude package {} ({error})",
                    staged.display()
                );
            }
            result
        }
        (Some(_), Some(_)) => anyhow::bail!(
            "bundle install accepts either a package or `--claude <source>`, not both"
        ),
        (None, None) => {
            anyhow::bail!("bundle install requires a bundle package or `--claude <source>`")
        }
    }
}

/// Translate a local Claude Code plugin directory or marketplace reference into a staged
/// `.hyabundle` package via the bundled Claude adapter.
///
/// The adapter prints one JSON envelope (`hya-plugin-claude::emit`) with the
/// standard `Plugin`/`Bundle` manifest and its translated files; they are prepared through
/// the canonical `hya_bundle` pipeline, so namespace and digest validation is
/// shared with every other install path.
async fn stage_claude_source(source: &Path) -> anyhow::Result<PathBuf> {
    let source_text = source.to_string_lossy();
    let marketplace = if source.is_dir() {
        None
    } else {
        source_text
            .rsplit_once('#')
            .filter(|(root, entry)| !root.is_empty() && !entry.is_empty())
    };
    anyhow::ensure!(
        source.is_dir() || marketplace.is_some(),
        "--claude source must be a local plugin directory or <marketplace-root>#<entry>: {}",
        source.display()
    );
    let Some(bun) = hya_app::plugins::find_bun() else {
        anyhow::bail!(
            "BUN_REQUIRED: installing `{}` needs Bun on PATH (or `BUN`) to run the Claude adapter",
            source.display()
        );
    };
    let adapter_main = hya_app::plugins::claude_adapter_dir().join("src/main.ts");
    anyhow::ensure!(
        adapter_main.is_file(),
        "claude adapter not found at {} (set HYA_CLAUDE_ADAPTER_DIR)",
        adapter_main.display()
    );
    let mut command = tokio::process::Command::new(&bun);
    command
        .arg("run")
        .arg(&adapter_main)
        .arg("--emit-bundle-manifest");
    if let Some((root, entry)) = marketplace {
        command
            .arg("--marketplace")
            .arg(root)
            .arg("--entry")
            .arg(entry);
    } else {
        command.arg("--plugin-dir").arg(source);
    }
    let output = command
        .output()
        .await
        .with_context(|| format!("run Claude adapter for {}", source.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "claude adapter failed for {} ({}): {}",
            source.display(),
            output.status,
            stderr.trim()
        );
    }
    let emit =
        hya_plugin_claude::emit::parse_manifest_emit(&String::from_utf8_lossy(&output.stdout))
            .map_err(|error| anyhow::anyhow!("parse claude adapter manifest envelope: {error}"))?;
    let mut files = Vec::with_capacity(emit.files.len() + 1);
    files.push(hya_bundle::SourceFile::new(
        "bundle.yaml",
        emit.manifest.clone().into_bytes(),
    ));
    for file in &emit.files {
        files.push(hya_bundle::SourceFile::new(
            file.path.clone(),
            file.content.clone().into_bytes(),
        ));
    }
    let bundle_source = hya_bundle::BundleSource::new(source.display().to_string(), files);
    // `write_public_package` runs the canonical prepare pipeline, so namespace,
    // digest, and identity validation is shared with every other install path.
    let bytes =
        hya_bundle::write_public_package(&bundle_source).context("prepare claude plugin bundle")?;

    let registry_path = hya_app::bundle_registry_path();
    let registry_parent = registry_path
        .parent()
        .context("bundle registry path has no parent")?;
    let staging_root = registry_parent.join("staging");
    fs::create_dir_all(&staging_root)
        .with_context(|| format!("create staging directory {}", staging_root.display()))?;
    let staged = staging_root.join(format!(
        "claude-install-{}.hyabundle",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    fs::write(&staged, bytes)
        .with_context(|| format!("write staged package {}", staged.display()))?;
    Ok(staged)
}

/// Run the package's declared self-check and say what it did.
async fn self_check(public: &PublicPackageInspection) -> anyhow::Result<()> {
    match bundle_admin::run_self_check(public).await? {
        SelfCheck::None => println!("self-check: none declared"),
        SelfCheck::Passed(shown) => println!("self-check: passed (`{shown}`)"),
    }
    Ok(())
}

/// After an install: ask the running backend of `db` to refresh its bundle
/// catalog now and require the installed bundle (`id` at `digest`) to be
/// published. Without a running backend it loads at the next start.
async fn prove_activation(db: &str, scope: Scope, id: &str, digest: &str) -> anyhow::Result<()> {
    let disabled = match bundle_admin::existing_registry().await? {
        Some(registry) => registry.disabled_bundle_ids().await?.contains(id),
        None => false,
    };
    let Some(found) = crate::daemon::running(db).await else {
        if disabled {
            println!("activation: installed, disabled");
        } else {
            println!("activation: no backend runs for {db}; it loads when one starts");
        }
        return Ok(());
    };
    let directory = match scope {
        Scope::User => String::new(),
        Scope::Project => std::env::current_dir()
            .context("read the current directory")?
            .to_string_lossy()
            .into_owned(),
    };
    let response = reqwest::Client::new()
        .post(format!("{}/v1/bundles:refresh", found.url))
        .json(&serde_json::json!({ "directory": directory }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .with_context(|| format!("ask the backend at {} to refresh its bundles", found.url))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .context("read the bundle refresh error")?;
        anyhow::bail!("installed but not activated: the backend answered {status}: {body}");
    }
    let body = response
        .json::<hya_api::v1::RefreshBundlesResponse>()
        .await
        .context("read the bundle refresh response")?;
    report_refresh_errors(Some(id), &body.errors)
        .map_err(|error| anyhow::anyhow!("installed but not activated: {error}"))?;
    if disabled {
        println!("activation: installed, disabled");
        return Ok(());
    }
    let published = body
        .bundles
        .iter()
        .any(|bundle| bundle.id == id && bundle.prepared_digest == digest);
    if published {
        println!(
            "activation: active in the backend (pid {}); the next turn uses it",
            found.pid
        );
        return Ok(());
    }
    if scope == Scope::Project && body.scope == "directory" {
        println!(
            "activation: {directory} is not inside a registered Project, so no session loads its \
             `.hya/bundles`; add it to a Project (TUI `/project`) to activate the bundle"
        );
        return Ok(());
    }
    anyhow::bail!(
        "installed but not activated: the backend (pid {}) does not publish {id} at the installed digest",
        found.pid
    )
}

/// What installing one package into a scope would do.
struct InstallPreview {
    target: String,
    action: BundleInstallAction,
    displaced: Vec<String>,
}

/// Plan an install of `public` into `scope` without writing anything.
///
/// `registry` is the user registry handle when one exists; `None` plans
/// against an empty registry without creating it.
async fn preview_install(
    public: &PublicPackageInspection,
    policy: NamespaceInstallPolicy,
    scope: Scope,
    registry: Option<&BundleRegistry>,
) -> anyhow::Result<InstallPreview> {
    let reserved = bundle_admin::reserved_agent_ids();
    match scope {
        Scope::User => {
            let registry_path = hya_app::bundle_registry_path();
            let plan = if let Some(registry) = registry {
                registry
                    .plan_install(&reserved, policy, &bundle_admin::install_candidate(public))
                    .await
                    .map_err(bundle_admin::store_error)?
            } else {
                // Nothing installed yet: only the candidate's own rules apply,
                // and verify must not create the registry to learn that.
                let [incoming] = public.prepared.bundles() else {
                    anyhow::bail!("public package must contain exactly one bundle");
                };
                if let Some(agent) = incoming
                    .agents()
                    .iter()
                    .find(|agent| reserved.contains(&agent.id.as_str()))
                {
                    return Err(BundleRegistryError::BundleAgentIdReserved {
                        bundle_id: incoming.identity().id.clone(),
                        agent_id: agent.id.as_str().to_string(),
                    }
                    .into());
                }
                BundleInstallPlan {
                    action: BundleInstallAction::Install,
                    displaced: Vec::new(),
                }
            };
            Ok(InstallPreview {
                target: registry_path.display().to_string(),
                action: plan.action,
                displaced: plan.displaced,
            })
        }
        Scope::Project => {
            let dir = project_dir()?;
            let plan = plan_project_install(&dir, &public.files, &reserved, policy)
                .map_err(bundle_admin::project_error)?;
            Ok(InstallPreview {
                target: plan.target.display().to_string(),
                action: plan.action,
                displaced: plan
                    .displaced
                    .iter()
                    .map(|bundle| bundle.bundle_id().to_string())
                    .collect(),
            })
        }
    }
}

fn action_label(action: &BundleInstallAction) -> String {
    match action {
        BundleInstallAction::Install => "install".to_string(),
        BundleInstallAction::Replace { installed_version } => {
            format!("replace from={installed_version}")
        }
        BundleInstallAction::Unchanged => "unchanged".to_string(),
    }
}

/// Ask `question` on stderr and read one answer line from stdin. Only `y` or
/// `yes` (any case) proceeds; anything else, including closed stdin, cancels.
fn confirm(summary: &str, verb: &str, yes: bool) -> anyhow::Result<()> {
    confirm_from(
        summary,
        verb,
        yes,
        std::io::stdin().lock(),
        std::io::stderr().lock(),
    )
}

fn confirm_from(
    summary: &str,
    verb: &str,
    yes: bool,
    mut reader: impl std::io::BufRead,
    mut writer: impl std::io::Write,
) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    write!(writer, "{summary}Proceed? [y/N] ").context("write confirmation prompt")?;
    writer.flush().context("flush confirmation prompt")?;
    let mut answer = String::new();
    let read = reader.read_line(&mut answer).context("read confirmation")?;
    if read == 0 {
        let _ = writeln!(writer);
        anyhow::bail!("bundle {verb} cancelled: no answer on stdin (pass -y to skip the prompt)");
    }
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        anyhow::bail!("bundle {verb} cancelled")
    }
}

async fn install(
    package: &Path,
    policy: NamespaceInstallPolicy,
    scope: Scope,
    yes: bool,
    db: &str,
) -> anyhow::Result<()> {
    let public = bundle_admin::inspect_installable(package)?;
    self_check(&public).await?;
    let [bundle] = public.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle");
    };
    let identity = bundle.identity().clone();
    let digest = bundle.digest().to_string();
    // One registry handle for plan and install: connecting again right after
    // dropping a handle can race the old pool's close and find the DB locked.
    let registry = match scope {
        Scope::User => Some(bundle_admin::open_registry().await?),
        Scope::Project => None,
    };
    let preview = preview_install(&public, policy, scope, registry.as_ref()).await?;
    if preview.action == BundleInstallAction::Unchanged {
        println!(
            "unchanged {} {} scope={}",
            identity.id,
            identity.version,
            scope.as_str()
        );
        return prove_activation(db, scope, &identity.id, &digest).await;
    }
    let mut summary = format!(
        "Install {} {} ({}) into {} scope\n  target: {}\n  action: {}\n",
        identity.id,
        identity.version,
        bundle.kind().as_str(),
        scope.as_str(),
        preview.target,
        action_label(&preview.action),
    );
    let agents = bundle
        .agents()
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<Vec<_>>();
    if !agents.is_empty() {
        summary.push_str(&format!("  agents: {}\n", agents.join(",")));
    }
    for displaced in &preview.displaced {
        summary.push_str(&format!("  removes: {displaced} (namespace takeover)\n"));
    }
    confirm(&summary, "install", yes)?;

    let project = match scope {
        Scope::User => None,
        Scope::Project => Some(project_dir()?),
    };
    let target = match (&registry, &project) {
        (Some(registry), _) => InstallTarget::User(registry),
        (None, Some(dir)) => InstallTarget::Project(dir),
        (None, None) => anyhow::bail!("user registry handle missing"),
    };
    let installed = bundle_admin::install(public, policy, target).await?;
    let action = match installed.action {
        BundleInstallAction::Install => "installed",
        BundleInstallAction::Replace { .. } => "replaced",
        BundleInstallAction::Unchanged => "unchanged",
    };
    match (installed.generation, installed.path) {
        (Some(generation), _) => println!(
            "{action} {} {} scope=user generation={generation}",
            identity.id, identity.version
        ),
        (None, Some(path)) => println!(
            "{action} {} {} scope=project path={}",
            identity.id,
            identity.version,
            path.display()
        ),
        (None, None) => println!("{action} {} {}", identity.id, identity.version),
    }
    prove_activation(db, scope, &identity.id, &digest).await
}

/// Deterministically pack a bundle source directory into a public `.hyabundle` archive.
fn pack(source: &Path, output: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.is_dir(),
        "bundle pack source must be a directory: {}",
        source.display()
    );
    bundle_admin::validate_package_path(output)?;
    let bundle_source = BundleSource::read_directory(source)
        .with_context(|| format!("read bundle source directory {}", source.display()))?;
    let bytes = write_public_package(&bundle_source)
        .with_context(|| format!("pack bundle source {}", source.display()))?;
    let inspected = inspect_public_package(&bytes).context("verify packed bundle archive")?;
    let [bundle] = inspected.bundles() else {
        anyhow::bail!("packed package must contain exactly one bundle");
    };
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create package output directory {}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bundle.hyabundle"),
        std::process::id()
    ));
    fs::write(&temporary, &bytes)
        .with_context(|| format!("write temporary package {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, output) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("activate package output {}", output.display()));
    }
    let identity = bundle.identity();
    println!("packed {} {}", identity.id, identity.version);
    println!("output={}", output.display());
    println!("bytes={}", bytes.len());
    println!("prepared_digest={}", inspected.digest());
    Ok(())
}

/// Check that `package` (a `.hyabundle` file or bundle source directory) is valid
/// and would install into `scope`, without installing it.
async fn verify(
    package: &Path,
    policy: NamespaceInstallPolicy,
    scope: Scope,
    strict: bool,
    protocol_smoke: bool,
) -> anyhow::Result<()> {
    if package.is_dir() {
        return verify_directory(package, policy, scope, strict, protocol_smoke).await;
    }
    let public = bundle_admin::inspect_installable(package)?;
    self_check(&public).await?;
    let [bundle] = public.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle");
    };
    if strict {
        verify_strict_bundle(bundle, None, &public.files)?;
    }
    let smoke_status = if protocol_smoke {
        Some(run_protocol_smoke(&public.prepared, bundle, None, Some(&public.files)).await?)
    } else {
        None
    };
    let registry = match scope {
        Scope::User => bundle_admin::existing_registry().await?,
        Scope::Project => None,
    };
    let preview = preview_install(&public, policy, scope, registry.as_ref()).await?;
    let identity = bundle.identity();
    println!("verified {} {}", identity.id, identity.version);
    println!("format=public-v1");
    println!("kind={}", bundle.kind().as_str());
    println!("source_digest={}", hex_digest(&public.source_digest));
    println!("prepared_digest={}", public.prepared.digest());
    println!("scope={}", scope.as_str());
    println!("target={}", preview.target);
    println!("action={}", action_label(&preview.action));
    for displaced in preview.displaced {
        println!("removes={displaced}");
    }
    if strict {
        println!("strict=passed");
    }
    if let Some(status) = smoke_status {
        println!("protocol_smoke={status}");
    }
    Ok(())
}

async fn verify_directory(
    dir: &Path,
    policy: NamespaceInstallPolicy,
    scope: Scope,
    strict: bool,
    protocol_smoke: bool,
) -> anyhow::Result<()> {
    let source = BundleSource::read_directory(dir)
        .with_context(|| format!("read bundle source directory {}", dir.display()))?;
    let prepared = prepare_package(source.clone())
        .with_context(|| format!("prepare bundle source directory {}", dir.display()))?;
    let [bundle] = prepared.bundles() else {
        anyhow::bail!("bundle source must contain exactly one bundle");
    };
    run_directory_self_check(dir, bundle).await?;

    let archive_a = write_public_package(&source)
        .with_context(|| format!("pack bundle source directory {}", dir.display()))?;
    let round_trip = inspect_public_package(&archive_a)
        .with_context(|| format!("verify packed archive for {}", dir.display()))?;
    anyhow::ensure!(
        round_trip.digest() == prepared.digest(),
        "packed archive prepared digest differs from directory source"
    );
    if strict {
        let archive_b = write_public_package(&source)
            .with_context(|| format!("re-pack bundle source directory {}", dir.display()))?;
        anyhow::ensure!(
            archive_a == archive_b,
            "strict check failed: packing `{}` is not deterministic",
            dir.display()
        );
        verify_strict_bundle(bundle, Some(dir), &[])?;
    }
    let smoke_status = if protocol_smoke {
        Some(run_protocol_smoke(&prepared, bundle, Some(dir), None).await?)
    } else {
        None
    };

    let identity = bundle.identity();
    let is_preset = hya_app::trusted_preset_inventory()
        .context("decode trusted presets")?
        .iter()
        .any(|preset| preset.id == identity.id);

    println!("verified {} {}", identity.id, identity.version);
    println!("format=source");
    println!("kind={}", bundle.kind().as_str());
    println!("prepared_digest={}", prepared.digest());
    println!("scope={}", scope.as_str());
    if is_preset {
        println!("action=builtin-source");
    } else {
        let staged = tempfile::tempdir().context("create temp dir for directory verify preview")?;
        let pkg_path = staged.path().join("verify.hyabundle");
        fs::write(&pkg_path, &archive_a).context("write temp package for preview")?;
        let public = bundle_admin::inspect_package(&pkg_path)?;
        let registry = match scope {
            Scope::User => bundle_admin::existing_registry().await?,
            Scope::Project => None,
        };
        let preview = preview_install(&public, policy, scope, registry.as_ref()).await?;
        println!("target={}", preview.target);
        println!("action={}", action_label(&preview.action));
        for displaced in preview.displaced {
            println!("removes={displaced}");
        }
    }
    if strict {
        println!("strict=passed");
    }
    if let Some(status) = smoke_status {
        println!("protocol_smoke={status}");
    }
    Ok(())
}

async fn run_directory_self_check(dir: &Path, bundle: &Bundle) -> anyhow::Result<()> {
    let Some(check) = bundle.check() else {
        println!("self-check: none declared");
        return Ok(());
    };
    let (program, args) = check
        .command
        .split_first()
        .context("check.command is empty")?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env("HYA_BUNDLE_ID", &bundle.identity().id)
        .env("HYA_BUNDLE_VERSION", &bundle.identity().version)
        .env("HYA_BUNDLE_ROOT", dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let shown = check.command.join(" ");
    let output = tokio::time::timeout(Duration::from_secs(check.timeout_secs), command.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "self-check `{shown}` timed out after {} s",
                check.timeout_secs
            )
        })?
        .with_context(|| format!("run self-check `{shown}`"))?;
    if !output.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(40)..].join("\n");
        anyhow::bail!("self-check `{shown}` failed ({})\n{tail}", output.status);
    }
    println!("self-check: passed (`{shown}`)");
    Ok(())
}

/// Strict static checks: SemVer, non-empty publisher, structured v2 descriptors,
/// MCP placeholder whitelist, skill/command bounds, and secret hygiene scan.
fn verify_strict_bundle(
    bundle: &Bundle,
    source_dir: Option<&Path>,
    packaged_files: &[SourceFile],
) -> anyhow::Result<()> {
    let identity = bundle.identity();
    anyhow::ensure!(
        !identity.publisher.trim().is_empty(),
        "strict check failed: identity.publisher must not be empty"
    );
    semver::Version::parse(&identity.version).with_context(|| {
        format!(
            "strict check failed: identity.version `{}` is not valid SemVer",
            identity.version
        )
    })?;

    // Check if the manifest declares schema_version: 2 so we can forbid empty `{}` placeholders.
    let manifest_text = if let Some(dir) = source_dir {
        let yaml = dir.join("bundle.yaml");
        if yaml.is_file() {
            fs::read_to_string(&yaml).ok()
        } else {
            None
        }
    } else {
        packaged_files
            .iter()
            .find(|f| f.path() == "bundle.yaml")
            .and_then(|f| std::str::from_utf8(f.bytes()).ok().map(String::from))
    };
    let is_schema_v2 = manifest_text.as_deref().is_some_and(|text| {
        text.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == "schema_version: 2" || trimmed.starts_with("schema_version: 2 ")
        })
    });

    if is_schema_v2 {
        for tool in bundle.tools() {
            if is_empty_json_object(&tool.content) {
                anyhow::bail!(
                    "strict check failed: schema_version 2 tool descriptor `{}` must not be an empty `{{}}` placeholder",
                    tool.source_path
                );
            }
        }
        for hook in bundle.hooks() {
            if is_empty_json_object(&hook.content) {
                anyhow::bail!(
                    "strict check failed: schema_version 2 hook descriptor `{}` must not be an empty `{{}}` placeholder",
                    hook.source_path
                );
            }
        }
    }

    for skill in bundle.skills() {
        anyhow::ensure!(
            !skill.content.trim().is_empty(),
            "strict check failed: skill `{}` must not be empty",
            skill.source_path
        );
        anyhow::ensure!(
            skill.content.len() <= 256 * 1024,
            "strict check failed: skill `{}` exceeds 256 KiB",
            skill.source_path
        );
    }

    for command in bundle.commands() {
        anyhow::ensure!(
            !command.content.trim().is_empty(),
            "strict check failed: command `{}` must not be empty",
            command.source_path
        );
        anyhow::ensure!(
            command.content.len() <= 256 * 1024,
            "strict check failed: command `{}` exceeds 256 KiB",
            command.source_path
        );
    }

    for mcp in bundle.mcp() {
        validate_mcp_strict(&mcp.source_path, &mcp.content)?;
    }

    // Secret hygiene scan across all text files in the bundle.
    let mut scan_targets: Vec<(String, String)> = Vec::new();
    if let Some(text) = manifest_text {
        scan_targets.push(("bundle.yaml".to_string(), text));
    }
    if let Some(dir) = source_dir {
        for example in ["config.example.yml", "config.example.yaml"] {
            let path = dir.join(example);
            if path.is_file()
                && let Ok(text) = fs::read_to_string(&path)
            {
                scan_targets.push((example.to_string(), text));
            }
        }
    }
    for agent in bundle.agents() {
        if let Some(prompt) = &agent.prompt {
            let path = agent
                .prompt_source
                .clone()
                .unwrap_or_else(|| format!("agent:{}", agent.id));
            scan_targets.push((path, prompt.clone()));
        }
    }
    for resource in bundle
        .tools()
        .iter()
        .chain(bundle.skills())
        .chain(bundle.commands())
        .chain(bundle.mcp())
        .chain(bundle.hooks())
        .chain(bundle.extensions())
    {
        if resource.binary_base64.is_none() {
            scan_targets.push((resource.source_path.clone(), resource.content.clone()));
        }
    }
    for (path, text) in &scan_targets {
        if let Some(reason) = find_hardcoded_secret(text) {
            anyhow::bail!("strict check failed: potential secret in `{path}` ({reason})");
        }
    }
    Ok(())
}

fn is_empty_json_object(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| value.as_object().map(|map| map.is_empty()))
        .unwrap_or(false)
}

fn validate_mcp_strict(path: &str, content: &str) -> anyhow::Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(content).with_context(|| format!("parse MCP `{path}`"))?;
    let Some(obj) = value.as_object() else {
        anyhow::bail!("strict check failed: MCP `{path}` must be a JSON object");
    };
    if let Some(command) = obj.get("command").and_then(serde_json::Value::as_array) {
        for arg in command.iter().filter_map(serde_json::Value::as_str) {
            validate_placeholders(path, arg)?;
        }
    }
    if let Some(url) = obj.get("url").and_then(serde_json::Value::as_str) {
        validate_placeholders(path, url)?;
    }
    if let Some(env) = obj.get("env").and_then(serde_json::Value::as_object) {
        for (key, val) in env {
            if let Some(text) = val.as_str() {
                validate_placeholders(path, text)?;
                if is_sensitive_key(key) && is_hardcoded_secret_value(text) {
                    anyhow::bail!(
                        "strict check failed: MCP `{path}` env `{key}` appears to contain a hardcoded secret; use `${{env:...}}`"
                    );
                }
            }
        }
    }
    if let Some(headers) = obj.get("headers").and_then(serde_json::Value::as_object) {
        for (key, val) in headers {
            if let Some(text) = val.as_str() {
                validate_placeholders(path, text)?;
                if is_sensitive_key(key) && is_hardcoded_secret_value(text) {
                    anyhow::bail!(
                        "strict check failed: MCP `{path}` header `{key}` appears to contain a hardcoded secret; use `${{env:...}}`"
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_placeholders(path: &str, text: &str) -> anyhow::Result<()> {
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            anyhow::bail!("strict check failed: unclosed placeholder in `{path}`: {text:?}");
        };
        let token = &after[..end];
        let allowed = matches!(
            token,
            "BUNDLE_ROOT" | "BUNDLE_CONFIG_FILE" | "BUNDLE_CONFIG_DIR" | "CLAUDE_PLUGIN_ROOT"
        ) || token.strip_prefix("env:").is_some_and(|var| {
            !var.is_empty()
                && var
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
        if !allowed {
            anyhow::bail!(
                "strict check failed: unsupported placeholder `${{{token}}}` in `{path}`"
            );
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "authorization"
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("api-key")
        || lower.contains("password")
}

fn is_hardcoded_secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains("${") || trimmed.starts_with('$') {
        return false;
    }
    let token = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim();
    if token.len() < 12 {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    !(lower.starts_with('<')
        || lower.contains("example")
        || lower.contains("placeholder")
        || lower.contains("your_")
        || lower.contains("your-")
        || lower.contains("changeme")
        || lower.contains("replace"))
}

fn find_hardcoded_secret(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        return Some("PEM private key header");
    }
    for (prefix, min_tail, label) in [
        ("sk-ant-", 16_usize, "Anthropic API key"),
        ("sk-proj-", 16, "OpenAI project API key"),
        ("github_pat_", 20, "GitHub fine-grained token"),
        ("ghp_", 20, "GitHub personal access token"),
        ("gho_", 20, "GitHub OAuth token"),
        ("ghu_", 20, "GitHub user token"),
        ("ghs_", 20, "GitHub server token"),
        ("xoxb-", 16, "Slack bot token"),
        ("xoxp-", 16, "Slack user token"),
        ("AIza", 35, "Google API key"),
    ] {
        let mut search = text;
        while let Some(pos) = search.find(prefix) {
            let after = &search[pos + prefix.len()..];
            let tail_len = after
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                .count();
            if tail_len >= min_tail {
                let tail = &after[..tail_len];
                let lower = tail.to_ascii_lowercase();
                if !lower.contains("example") && !lower.contains("placeholder") {
                    return Some(label);
                }
            }
            search = &search[pos + prefix.len()..];
        }
    }
    // OpenAI legacy `sk-<20+ alnum>` keys (bounded by non-identifier char before `sk-`).
    let bytes = text.as_bytes();
    let mut idx = 0;
    while idx + 3 < bytes.len() {
        if &bytes[idx..idx + 3] == b"sk-"
            && (idx == 0 || !(bytes[idx - 1].is_ascii_alphanumeric() || bytes[idx - 1] == b'_'))
        {
            let tail = &bytes[idx + 3..];
            let alnum_len = tail
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric())
                .count();
            if alnum_len >= 20 {
                let lower = std::str::from_utf8(&tail[..alnum_len])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !lower.starts_with("test")
                    && !lower.contains("example")
                    && !lower.contains("placeholder")
                {
                    return Some("API secret key");
                }
            }
        }
        idx += 1;
    }
    // AWS Access Key ID: `AKIA` + 16 uppercase alphanumerics.
    let mut aws_idx = 0;
    while aws_idx + 20 <= bytes.len() {
        if &bytes[aws_idx..aws_idx + 4] == b"AKIA"
            && (aws_idx == 0 || !bytes[aws_idx - 1].is_ascii_alphanumeric())
            && bytes[aws_idx + 4..aws_idx + 20]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            && (aws_idx + 20 == bytes.len() || !bytes[aws_idx + 20].is_ascii_alphanumeric())
        {
            return Some("AWS access key ID");
        }
        aws_idx += 1;
    }
    None
}

/// Black-box JSON-RPC 2.0 protocol smoke check for process-backed bundles:
/// materializes the bundle in an isolated temporary directory, spawns its
/// configured process, sends `initialize`, verifies returned tools/hooks/apis
/// match the manifest declarations, sends `shutdown`, and confirms clean exit.
async fn run_protocol_smoke(
    prepared: &PreparedCatalog,
    bundle: &Bundle,
    source_dir: Option<&Path>,
    packaged_files: Option<&[SourceFile]>,
) -> anyhow::Result<String> {
    let bundle_id = &bundle.identity().id;
    let Some(process) = prepared.bundle_process(bundle_id) else {
        return Ok("skipped (no process runtime)".to_string());
    };

    let temp = tempfile::tempdir().context("create temp directory for protocol smoke")?;
    let root = temp.path().join("bundle");
    let config_dir = temp.path().join("config");
    let config_file = config_dir.join("config.yml");
    fs::create_dir_all(&root).context("create smoke bundle root")?;
    fs::create_dir_all(&config_dir).context("create smoke config dir")?;
    fs::write(&config_file, b"").context("write smoke config file")?;

    materialize_for_smoke(&root, bundle, source_dir, packaged_files)?;

    let mut command = expand_smoke_argv(&process.command, &root, &config_file, &config_dir);
    match process.kind {
        PreparedProcessKind::Bun => {
            if command.first().is_some_and(|program| program == "bun")
                && let Some(bun) = hya_app::plugins::find_bun()
            {
                command[0] = bun.to_string_lossy().into_owned();
            }
        }
        PreparedProcessKind::Claude => {
            let bun = hya_app::plugins::find_bun()
                .context("protocol smoke for Claude bundle requires Bun")?;
            let mut runner = vec![
                bun.to_string_lossy().into_owned(),
                "run".to_string(),
                hya_app::plugins::claude_adapter_dir()
                    .join("src/main.ts")
                    .to_string_lossy()
                    .into_owned(),
                "--plugin-id".to_string(),
                bundle.namespace().to_string(),
            ];
            runner.append(&mut command);
            command = runner;
        }
        PreparedProcessKind::Rust => {}
    }

    let (program, args) = command
        .split_first()
        .context("protocol smoke: process command is empty")?;
    let mut env = BTreeMap::new();
    env.insert(
        "HYA_BUNDLE_ROOT".to_string(),
        root.to_string_lossy().into_owned(),
    );
    env.insert(
        "CLAUDE_PLUGIN_ROOT".to_string(),
        root.to_string_lossy().into_owned(),
    );
    env.insert(
        "HYA_BUNDLE_CONFIG_FILE".to_string(),
        config_file.to_string_lossy().into_owned(),
    );
    env.insert(
        "HYA_BUNDLE_CONFIG_DIR".to_string(),
        config_dir.to_string_lossy().into_owned(),
    );
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    if let Ok(home) = std::env::var("HOME") {
        env.insert("HOME".to_string(), home);
    }

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(&root)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("protocol smoke: spawn `{}`", command.join(" ")))?;

    let mut stdin = child
        .stdin
        .take()
        .context("protocol smoke: child stdin missing")?;
    let stdout = child
        .stdout
        .take()
        .context("protocol smoke: child stdout missing")?;
    let mut stderr = child
        .stderr
        .take()
        .context("protocol smoke: child stderr missing")?;

    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        if buf.len() > 8192 {
            buf.drain(..buf.len() - 8192);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let mut reader = BufReader::new(stdout);
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocol_version": 1,
            "host": {
                "name": "hya",
                "version": hya_version::VERSION,
            }
        }
    });
    let mut init_line = serde_json::to_string(&init_request)?;
    init_line.push('\n');
    stdin
        .write_all(init_line.as_bytes())
        .await
        .context("protocol smoke: write initialize request")?;
    stdin
        .flush()
        .await
        .context("protocol smoke: flush initialize request")?;

    let mut reply_line = String::new();
    let read_result =
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut reply_line))
            .await
            .map_err(|_| {
                anyhow::anyhow!("protocol smoke: timed out waiting for initialize response")
            })?
            .context("protocol smoke: read initialize response")?;
    if read_result == 0 {
        let _ = child.start_kill();
        let _ = child.wait().await;
        let stderr_text = stderr_task.await.unwrap_or_default();
        anyhow::bail!(
            "protocol smoke: process exited before answering initialize\nstderr: {}",
            stderr_text.trim()
        );
    }

    let reply: serde_json::Value = serde_json::from_str(reply_line.trim()).with_context(|| {
        format!(
            "protocol smoke: initialize response is not valid JSON: {:?}",
            reply_line.trim()
        )
    })?;
    if reply.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || reply.get("id").and_then(serde_json::Value::as_u64) != Some(1)
    {
        anyhow::bail!("protocol smoke: invalid JSON-RPC 2.0 initialize envelope: {reply}");
    }
    if let Some(error) = reply.get("error") {
        anyhow::bail!("protocol smoke: initialize returned error: {error}");
    }
    let result = reply
        .get("result")
        .and_then(serde_json::Value::as_object)
        .context("protocol smoke: initialize response missing `result` object")?;
    if result
        .get("protocol_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        anyhow::bail!(
            "protocol smoke: expected protocol_version 1, got {:?}",
            result.get("protocol_version")
        );
    }
    let plugin_id = result
        .get("plugin")
        .and_then(|p| p.get("id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    anyhow::ensure!(
        !plugin_id.trim().is_empty(),
        "protocol smoke: initialize result missing non-empty `plugin.id`"
    );

    let mut actual_tools = BTreeSet::new();
    if let Some(tools) = result.get("tools").and_then(serde_json::Value::as_array) {
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .context("protocol smoke: tool entry missing non-empty `name`")?;
            let schema_type = tool
                .get("inputSchema")
                .and_then(|schema| schema.get("type"))
                .and_then(serde_json::Value::as_str);
            anyhow::ensure!(
                schema_type == Some("object"),
                "protocol smoke: tool `{name}` inputSchema.type must be `object`"
            );
            actual_tools.insert(name.to_string());
        }
    }
    let actual_hooks: BTreeSet<String> = result
        .get("hooks")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hook| hook.get("name").and_then(serde_json::Value::as_str))
        .map(String::from)
        .collect();
    let actual_apis: BTreeSet<String> = result
        .get("apis")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|api| api.get("name").and_then(serde_json::Value::as_str))
        .map(String::from)
        .collect();

    let declared_tools: BTreeSet<String> = bundle
        .tools()
        .iter()
        .map(|tool| tool.local_id.clone())
        .collect();
    let declared_hooks: BTreeSet<String> = bundle
        .hooks()
        .iter()
        .map(|hook| hook.local_id.clone())
        .collect();
    let declared_apis: BTreeSet<String> = prepared
        .bundle_apis(bundle_id)
        .iter()
        .map(|api| api.id.clone())
        .collect();

    anyhow::ensure!(
        declared_tools == actual_tools,
        "protocol smoke: tools mismatch (manifest={declared_tools:?}, runtime={actual_tools:?})"
    );
    anyhow::ensure!(
        declared_hooks == actual_hooks,
        "protocol smoke: hooks mismatch (manifest={declared_hooks:?}, runtime={actual_hooks:?})"
    );
    anyhow::ensure!(
        declared_apis == actual_apis,
        "protocol smoke: apis mismatch (manifest={declared_apis:?}, runtime={actual_apis:?})"
    );

    // Send graceful shutdown and verify clean exit.
    let shutdown_request = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n";
    let _ = stdin.write_all(shutdown_request.as_bytes()).await;
    let _ = stdin.flush().await;
    let mut shutdown_line = String::new();
    let _ =
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut shutdown_line)).await;
    drop(stdin);

    let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            anyhow::bail!("protocol smoke: wait for child exit failed: {error}");
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            anyhow::bail!("protocol smoke: process did not exit within 2s after shutdown");
        }
    };
    let stderr_text = stderr_task.await.unwrap_or_default();
    anyhow::ensure!(
        status.success(),
        "protocol smoke: process exited with non-zero status {status}\nstderr: {}",
        stderr_text.trim()
    );

    Ok(format!(
        "passed (tools={} hooks={} apis={})",
        declared_tools.len(),
        declared_hooks.len(),
        declared_apis.len()
    ))
}

fn materialize_for_smoke(
    root: &Path,
    bundle: &Bundle,
    source_dir: Option<&Path>,
    packaged_files: Option<&[SourceFile]>,
) -> anyhow::Result<()> {
    let executable_paths: BTreeSet<&str> = bundle
        .extensions()
        .iter()
        .filter(|res| res.binary_base64.is_some() && !res.stable_id.contains("/library/"))
        .map(|res| res.source_path.as_str())
        .collect();
    if let Some(files) = packaged_files {
        for file in files {
            let target = root.join(file.path());
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, file.bytes())?;
            if executable_paths.contains(file.path()) {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
            }
        }
        return Ok(());
    }
    if let Some(dir) = source_dir {
        for name in ["bundle.yaml", "bundle.hya.md"] {
            let src = dir.join(name);
            if src.is_file() {
                fs::write(root.join(name), fs::read(&src)?)?;
            }
        }
    }
    for resource in bundle
        .tools()
        .iter()
        .chain(bundle.skills())
        .chain(bundle.commands())
        .chain(bundle.mcp())
        .chain(bundle.hooks())
        .chain(bundle.extensions())
    {
        let target = root.join(&resource.source_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = resource
            .source_bytes()
            .context("decode prepared resource bytes")?;
        fs::write(&target, bytes)?;
        if executable_paths.contains(resource.source_path.as_str()) {
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn expand_smoke_argv(
    command: &[String],
    root: &Path,
    config_file: &Path,
    config_dir: &Path,
) -> Vec<String> {
    let root_str = root.to_string_lossy();
    let cfg_file_str = config_file.to_string_lossy();
    let cfg_dir_str = config_dir.to_string_lossy();
    command
        .iter()
        .map(|arg| {
            let expanded = arg
                .replace("${BUNDLE_ROOT}", &root_str)
                .replace("${CLAUDE_PLUGIN_ROOT}", &root_str)
                .replace("${BUNDLE_CONFIG_FILE}", &cfg_file_str)
                .replace("${BUNDLE_CONFIG_DIR}", &cfg_dir_str);
            let path = Path::new(&expanded);
            if path.is_relative() && root.join(path).is_file() {
                root.join(path).to_string_lossy().into_owned()
            } else {
                expanded
            }
        })
        .collect()
}

/// Project bundle directory of the current working directory:
/// `$CWD/.hya/bundles`. `hya bundle install|remove --project` legitimately
/// uses the caller's cwd directly (unlike the runtime, which loads project
/// bundles per registered Project from every Project root — see
/// `hya_app::ProjectScopeRefresh`).
fn project_bundles_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".hya/bundles"))
}

/// Project scope root: `.hya/bundles` under the current directory, the same
/// directory the runtime loads project bundles from.
fn project_dir() -> anyhow::Result<PathBuf> {
    project_bundles_dir().context("resolve the current directory for --project")
}

fn info_file(package: &Path) -> anyhow::Result<()> {
    print!("{}", format_info_file(package)?);
    Ok(())
}

fn format_info_file(package: &Path) -> anyhow::Result<String> {
    use std::fmt::Write as _;
    bundle_admin::validate_package_path(package)?;
    let inspection = bundle_admin::inspect_package(package)?;
    let [bundle] = inspection.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle")
    };
    let identity = bundle.identity();
    let mut out = String::new();
    let _ = writeln!(out, "format: public-v1");
    let _ = writeln!(out, "name: {}", identity.id);
    let _ = writeln!(out, "version: {}", identity.version);
    let _ = writeln!(out, "publisher: {}", identity.publisher);
    let _ = writeln!(out, "origin: package");
    let _ = writeln!(out, "state: inspected");
    let _ = writeln!(out, "immutable: false");
    let _ = writeln!(
        out,
        "source_digest: {}",
        hex_digest(&inspection.source_digest)
    );
    let _ = writeln!(out, "prepared_digest: {}", inspection.prepared.digest());
    write_static_info(
        &mut out,
        bundle,
        ": ",
        inspection.prepared.bundle_schemas(&identity.id),
        inspection.prepared.bundle_process(&identity.id),
        inspection.prepared.bundle_apis(&identity.id),
        inspection.prepared.bundle_permission_modes(&identity.id),
    );
    Ok(out)
}

/// Every bundle across scopes as a `bundle list` row plus its search
/// metadata ([`bundle_admin::listings`]: project bundles shadow
/// user-installed ones by id or namespace, and first-party bundles hide
/// behind any active user or project bundle). Shared by `list` and `search`
/// so both always cover the same bundles.
async fn catalog_entries() -> anyhow::Result<Vec<SearchEntry>> {
    let mut rows = bundle_admin::listings(project_bundles_dir().as_deref())
        .await?
        .into_iter()
        .map(search_entry)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (left.row.name.as_bytes(), left.row.scope)
            .cmp(&(right.row.name.as_bytes(), right.row.scope))
    });
    Ok(rows)
}

/// Catalog entries narrowed to one scope, or all of them.
async fn scoped_entries(filter: Option<Scope>) -> anyhow::Result<Vec<SearchEntry>> {
    let mut entries = catalog_entries().await?;
    entries.retain(|entry| filter.is_none_or(|scope| entry.row.scope == scope.as_str()));
    Ok(entries)
}

async fn list(filter: Option<Scope>) -> anyhow::Result<()> {
    let entries = scoped_entries(filter).await?;
    print_list_rows(entries.iter().map(|entry| &entry.row));
    Ok(())
}

/// Scope label for the bundles shipped with hya (trusted presets and
/// first-party bundles).
const BUILTIN_SCOPE: &str = "builtin";

/// One listed bundle as a CLI row plus its lowercased search metadata: the
/// bundle id and every agent, skill, and tool id it declares. An unreadable
/// registry row keeps only its id and version.
fn search_entry(listing: BundleListing) -> SearchEntry {
    let components = &listing.components;
    let mut haystack = listing.id.to_lowercase();
    for id in components
        .agents
        .iter()
        .chain(&components.skills)
        .chain(&components.tools)
    {
        haystack.push('\n');
        haystack.push_str(&id.to_lowercase());
    }
    let unreadable = listing.state == "unreadable";
    let dash = || "-".to_string();
    SearchEntry {
        haystack,
        row: BundleListRow {
            agents: if unreadable {
                dash()
            } else {
                components.agents.join(",")
            },
            workflow: components.workflows.first().cloned().unwrap_or_else(dash),
            kind: if unreadable { dash() } else { listing.kind },
            state: if unreadable {
                if listing.enabled {
                    "unreadable (reinstall)"
                } else {
                    "disabled, unreadable (reinstall)"
                }
                .to_string()
            } else {
                listing.state
            },
            scope: match listing.scope.as_str() {
                bundle_admin::SCOPE_USER => Scope::User.as_str(),
                bundle_admin::SCOPE_PROJECT => Scope::Project.as_str(),
                _ => BUILTIN_SCOPE,
            },
            name: listing.id,
            version: listing.version,
        },
    }
}

/// One `bundle list` row: NAME VERSION AGENT STATE KIND WORKFLOW SCOPE.
struct BundleListRow {
    name: String,
    version: String,
    agents: String,
    state: String,
    kind: String,
    workflow: String,
    scope: &'static str,
}

/// One searchable catalog entry: the lowercased metadata a query matches
/// against and the `bundle list` row printed when it matches.
struct SearchEntry {
    haystack: String,
    row: BundleListRow,
}

/// Print the `bundle list` header plus one line per row.
fn print_list_rows<'a>(rows: impl Iterator<Item = &'a BundleListRow>) {
    print!("{}", format_list_rows(rows));
}

fn format_list_rows<'a>(rows: impl Iterator<Item = &'a BundleListRow>) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("NAME VERSION AGENT STATE KIND WORKFLOW SCOPE\n");
    for row in rows {
        let _ = writeln!(
            out,
            "{} {} {} {} {} {} {}",
            row.name, row.version, row.agents, row.state, row.kind, row.workflow, row.scope
        );
    }
    out
}

/// Filter `entries` by case-insensitive substring; returns `(rows, fallback_used)`.
fn filter_search_entries<'a>(
    entries: &'a [SearchEntry],
    query: &str,
) -> anyhow::Result<(Vec<&'a BundleListRow>, bool)> {
    let needle = query.trim().to_lowercase();
    anyhow::ensure!(!needle.is_empty(), "bundle search requires a query");
    let matched: Vec<&BundleListRow> = entries
        .iter()
        .filter(|entry| entry.haystack.contains(needle.as_str()))
        .map(|entry| &entry.row)
        .collect();
    if matched.is_empty() {
        Ok((entries.iter().map(|entry| &entry.row).collect(), true))
    } else {
        Ok((matched, false))
    }
}

/// Search every scope (or one, with `--user`/`--project`): a
/// case-insensitive substring query over bundle ids, agent ids, and skill ids
/// prints matching bundles as `bundle list` rows, with the same scope and
/// state `list` reports. When no metadata matches — a query naming a
/// subcommand like `schema` rather than bundle metadata — every bundle in the
/// searched scope prints instead and the fallback is explained on stderr.
async fn search(query: &str, filter: Option<Scope>) -> anyhow::Result<()> {
    let entries = scoped_entries(filter).await?;
    let (rows, fallback) = filter_search_entries(&entries, query)?;
    if fallback {
        eprintln!("hya: no bundle metadata matched `{query}`; listing every bundle instead");
    }
    print_list_rows(rows.into_iter());
    Ok(())
}

async fn info(bundle_id: &str, filter: Option<Scope>) -> anyhow::Result<()> {
    let not_found = || -> anyhow::Error {
        BundleRegistryError::BundleNotFound {
            bundle_id: bundle_id.to_string(),
        }
        .into()
    };
    match filter {
        Some(Scope::User) => {
            return if info_installed(bundle_id).await? {
                Ok(())
            } else {
                Err(not_found())
            };
        }
        Some(Scope::Project) => {
            let bundle = bundle_admin::project_bundle(&project_dir()?, bundle_id)?;
            info_project(&bundle);
            return Ok(());
        }
        None => {}
    }
    if let Some(preset) = hya_app::trusted_preset_inventory()
        .context("decode trusted presets")?
        .into_iter()
        .find(|preset| preset.id == bundle_id)
    {
        println!("name={}", preset.id);
        println!("version={}", preset.version);
        println!("origin=preset");
        println!("scope={BUILTIN_SCOPE}");
        println!("state=active");
        println!("immutable={}", preset.immutable);
        println!("installable={}", preset.installable);
        println!("prepared_digest={}", preset.digest);
        println!("kind={}", preset.kind);
        for agent in preset.agent_ids {
            println!("agent={agent}");
        }
        for resource in preset.resource_ids {
            println!("resource={resource}");
        }
        return Ok(());
    }
    if let Ok(bundle) = find_project_bundle(&project_dir()?, bundle_id) {
        info_project(&bundle);
        return Ok(());
    }
    if info_installed(bundle_id).await? {
        return Ok(());
    }
    if hya_app::first_party_catalogs()
        .context("load first-party bundles")?
        .iter()
        .any(|catalog| {
            catalog
                .bundles()
                .iter()
                .any(|bundle| bundle.identity().id == bundle_id)
        })
    {
        return info_first_party(bundle_id);
    }
    Err(not_found())
}

/// Print a user-installed bundle; `false` when the registry has no such id.
async fn info_installed(bundle_id: &str) -> anyhow::Result<bool> {
    let Some((loaded, enabled)) = installed_record_if_exists(bundle_id).await? else {
        return Ok(false);
    };
    let record = &loaded.record;
    let prepared = &loaded.prepared;
    let bundle = &prepared.bundles()[0];

    let identity = bundle.identity();
    println!("name={}", identity.id);
    println!("version={}", identity.version);
    println!("publisher={}", identity.publisher);
    println!("origin=installed");
    println!("scope=user");
    println!("format=public-v1");
    println!("state={}", if enabled { "active" } else { "disabled" });
    println!("immutable=false");
    println!("source_digest={}", hex_digest(&record.source_digest));
    println!("prepared_digest={}", record.prepared_digest);
    print_static_info(
        bundle,
        "=",
        prepared.bundle_schemas(bundle_id),
        prepared.bundle_process(bundle_id),
        prepared.bundle_apis(bundle_id),
        prepared.bundle_permission_modes(bundle_id),
    );
    Ok(true)
}

/// Print a project bundle read from its source directory.
fn info_project(bundle: &ProjectBundle) {
    let prepared = bundle.prepared();
    let prepared_bundle = bundle.bundle();
    let identity = prepared_bundle.identity();
    println!("name={}", identity.id);
    println!("version={}", identity.version);
    println!("publisher={}", identity.publisher);
    println!("origin=project");
    println!("scope=project");
    println!("format=source");
    println!("path={}", bundle.dir().display());
    println!("state=active");
    println!("immutable=false");
    println!("prepared_digest={}", prepared.digest());
    print_static_info(
        prepared_bundle,
        "=",
        prepared.bundle_schemas(&identity.id),
        prepared.bundle_process(&identity.id),
        prepared.bundle_apis(&identity.id),
        prepared.bundle_permission_modes(&identity.id),
    );
}

/// Print metadata for the immutable first-party bundles.
fn info_first_party(bundle_id: &str) -> anyhow::Result<()> {
    let catalogs = hya_app::first_party_catalogs().context("load first-party bundles")?;
    for prepared in &catalogs {
        let [bundle] = prepared.bundles() else {
            anyhow::bail!("first-party catalog must contain exactly one bundle")
        };
        if bundle.identity().id != bundle_id {
            continue;
        }
        let identity = bundle.identity();
        println!("name={}", identity.id);
        println!("version={}", identity.version);
        println!("publisher={}", identity.publisher);
        println!("origin=first-party");
        println!("scope={BUILTIN_SCOPE}");
        println!("format=prepared-v{}", bundle.format_version);
        println!("state=active");
        println!("immutable=true");
        println!("prepared_digest={}", prepared.digest());
        print_static_info(
            bundle,
            "=",
            prepared.bundle_schemas(&identity.id),
            prepared.bundle_process(&identity.id),
            prepared.bundle_apis(&identity.id),
            prepared.bundle_permission_modes(&identity.id),
        );
        return Ok(());
    }
    Err(BundleRegistryError::BundleNotFound {
        bundle_id: bundle_id.to_string(),
    }
    .into())
}

async fn remove(bundle_id: &str, scope: Scope, yes: bool, db: &str) -> anyhow::Result<()> {
    let project = match scope {
        Scope::User => None,
        Scope::Project => Some(project_dir()?),
    };
    let summary = match &project {
        None => {
            let installed = bundle_admin::listings(None)
                .await?
                .into_iter()
                .find(|record| record.id == bundle_id && record.scope == bundle_admin::SCOPE_USER);
            let Some(record) = installed else {
                // Not installed: the shared removal names why (preset, first-party, or not found).
                bundle_admin::remove(bundle_id, None).await?;
                anyhow::bail!("bundle `{bundle_id}` is not installed");
            };
            format!(
                "Remove {bundle_id} {} from user scope\n  registry: {}\n",
                record.version,
                hya_app::bundle_registry_path().display()
            )
        }
        Some(dir) => {
            let bundle = bundle_admin::project_bundle(dir, bundle_id)?;
            format!(
                "Remove {bundle_id} {} from project scope\n  deletes: {}\n",
                bundle.version(),
                bundle.dir().display()
            )
        }
    };
    confirm(&summary, "remove", yes)?;
    match bundle_admin::remove(bundle_id, project.as_deref()).await? {
        Removed::User { generation } => {
            println!("removed {bundle_id} scope=user generation={generation}");
        }
        Removed::Project { path } => {
            println!("removed {bundle_id} scope=project path={}", path.display());
        }
    }
    refresh_daemon(db, Some(bundle_id)).await
}

/// Print the URI-scheme extensions one bundle declares, one
/// `SCHEME TOOL WRITABLE` row each (header only when it declares none).
///
/// `name` is a bundle id resolved like `info` (preset, project, user, then
/// first-party, or only the named scope), or a `.hyabundle` package file,
/// which is inspected without installing it.
async fn schema(name: &str, filter: Option<Scope>) -> anyhow::Result<()> {
    let as_file = Path::new(name);
    if name.ends_with(".hyabundle") && as_file.is_file() {
        bundle_admin::validate_package_path(as_file)?;
        let public = bundle_admin::inspect_package(as_file)?;
        let [bundle] = public.prepared.bundles() else {
            anyhow::bail!("public package must contain exactly one bundle");
        };
        print_schema_rows(public.prepared.bundle_schemas(&bundle.identity().id));
        return Ok(());
    }
    let not_found = || -> anyhow::Error {
        BundleRegistryError::BundleNotFound {
            bundle_id: name.to_string(),
        }
        .into()
    };
    let project = || -> anyhow::Result<Option<ProjectBundle>> {
        Ok(find_project_bundle(&project_dir()?, name).ok())
    };
    let installed = || async {
        Ok::<_, anyhow::Error>(
            installed_record_if_exists(name)
                .await?
                .map(|(loaded, _)| loaded.prepared),
        )
    };
    let prepared: PreparedCatalog = match filter {
        Some(Scope::Project) => {
            let bundle = project()?.ok_or_else(not_found)?;
            print_schema_rows(bundle.prepared().bundle_schemas(name));
            return Ok(());
        }
        Some(Scope::User) => installed().await?.ok_or_else(not_found)?,
        None => {
            let builtin = hya_bundle::FIRST_PARTY_BUNDLES.contains(&name);
            let preset = hya_app::trusted_preset_inventory()
                .context("decode trusted presets")?
                .iter()
                .any(|preset| preset.id == name);
            if preset {
                return print_builtin_schema(name);
            }
            if let Some(bundle) = project()? {
                print_schema_rows(bundle.prepared().bundle_schemas(name));
                return Ok(());
            }
            match installed().await? {
                Some(prepared) => prepared,
                None if builtin => return print_builtin_schema(name),
                None => return Err(not_found()),
            }
        }
    };
    print_schema_rows(prepared.bundle_schemas(name));
    Ok(())
}

/// Print a builtin (preset or first-party) bundle's declared schemas.
fn print_builtin_schema(bundle_id: &str) -> anyhow::Result<()> {
    let catalog = hya_bundle::first_party_bundle(bundle_id)
        .with_context(|| format!("load builtin bundle {bundle_id}"))?;
    print_schema_rows(catalog.bundle_schemas(bundle_id));
    Ok(())
}

fn print_schema_rows(schemas: &[hya_bundle::PreparedSchema]) {
    print!("{}", format_schema_rows(schemas));
}

fn format_schema_rows(schemas: &[hya_bundle::PreparedSchema]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("SCHEME TOOL WRITABLE\n");
    for schema in schemas {
        let _ = writeln!(out, "{} {} {}", schema.scheme, schema.tool, schema.writable);
    }
    out
}

async fn installed_record_if_exists(
    id: &str,
) -> anyhow::Result<Option<(LoadedBundleRegistryRecord, bool)>> {
    let Some(registry) = bundle_admin::existing_registry().await? else {
        return Ok(None);
    };
    let snapshot = registry
        .snapshot_with_catalogs()
        .await
        .map_err(bundle_admin::store_error)?;
    let enabled = !registry.disabled_bundle_ids().await?.contains(id);
    if let Some(row) = snapshot.unreadable.iter().find(|row| row.bundle_id == id) {
        anyhow::bail!(
            "installed bundle {id} is unreadable{}: {}",
            if enabled { "" } else { " (disabled)" },
            row.error
        );
    }
    Ok(snapshot
        .bundles
        .into_iter()
        .find(|loaded| loaded.record.bundle_id == id)
        .map(|loaded| (loaded, enabled)))
}

/// Print the static metadata of one prepared bundle, including its declared
/// schemas, optional `extensions.process` declaration, API endpoints, and
/// mcp entries. The declaration lines print only when non-empty.
fn print_static_info(
    bundle: &Bundle,
    separator: &str,
    schemas: &[hya_bundle::PreparedSchema],
    process: Option<&hya_bundle::PreparedProcessExtension>,
    apis: &[hya_bundle::PreparedApi],
    permission_modes: &[hya_bundle::PreparedPermissionMode],
) {
    let mut out = String::new();
    write_static_info(
        &mut out,
        bundle,
        separator,
        schemas,
        process,
        apis,
        permission_modes,
    );
    print!("{out}");
}

fn write_static_info(
    out: &mut String,
    bundle: &Bundle,
    separator: &str,
    schemas: &[hya_bundle::PreparedSchema],
    process: Option<&hya_bundle::PreparedProcessExtension>,
    apis: &[hya_bundle::PreparedApi],
    permission_modes: &[hya_bundle::PreparedPermissionMode],
) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "kind{separator}{}", bundle.kind().as_str());
    if let Some(workflow) = bundle.workflow() {
        let _ = writeln!(out, "workflow{separator}{}", workflow.id);
    }
    for agent in bundle.agents() {
        let _ = writeln!(out, "agent{separator}{}", agent.id);
    }
    for skill in bundle.skills() {
        let _ = writeln!(out, "skill{separator}{}", skill.stable_id);
    }
    for command in bundle.commands() {
        let _ = writeln!(out, "command{separator}{}", command.stable_id);
    }
    for tool in bundle.tools() {
        let _ = writeln!(out, "tool{separator}{}", tool.stable_id);
    }
    for mcp in bundle.mcp() {
        let _ = writeln!(out, "mcp{separator}{}", mcp.stable_id);
    }
    for hook in bundle.hooks() {
        let _ = writeln!(out, "hook{separator}{}", hook.stable_id);
    }
    for extension in bundle.extensions() {
        let _ = writeln!(out, "extension{separator}{}", extension.stable_id);
    }
    for schema in schemas {
        let _ = writeln!(
            out,
            "schema{separator}{scheme} tool={tool} writable={writable}",
            scheme = schema.scheme,
            tool = schema.tool,
            writable = schema.writable
        );
    }
    if let Some(process) = process {
        let _ = writeln!(
            out,
            "process{separator}{kind} command={command}",
            kind = process.kind.as_str(),
            command = process.command.join(" ")
        );
    }
    // One line per endpoint: `api=<METHOD> <scope> <path> id=<id>` plus the
    // optional schema paths and (last, since it may contain spaces) the
    // description.
    for api in apis {
        let mut line = format!(
            "api{separator}{method} {scope} {path} id={id}",
            method = api.method,
            scope = api.scope,
            path = api.path,
            id = api.id
        );
        if let Some(schema) = &api.request_schema {
            line.push_str(&format!(" request_schema={schema}"));
        }
        if let Some(schema) = &api.response_schema {
            line.push_str(&format!(" response_schema={schema}"));
        }
        if !api.description.is_empty() {
            line.push_str(&format!(" description={}", api.description));
        }
        let _ = writeln!(out, "{line}");
    }
    // One line per mode: `permission_mode=<bundle-id>/<id> title=<title>`
    // plus (last, since it may contain spaces) the description.
    for mode in permission_modes {
        let mut line = format!(
            "permission_mode{separator}{bundle}/{id} title={title}",
            bundle = bundle.identity().id,
            id = mode.id,
            title = mode.title
        );
        if !mode.description.is_empty() {
            line.push_str(&format!(" description={}", mode.description));
        }
        let _ = writeln!(out, "{line}");
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod refresh_tests {
    #![allow(clippy::unwrap_used)]

    fn listing(
        id: &str,
        scope: &str,
        state: &str,
        enabled: bool,
        agents: &[&str],
        skills: &[&str],
        workflows: &[&str],
    ) -> super::SearchEntry {
        use hya_server::{BundleListing, BundleListingComponents};
        let to_vec = |xs: &[&str]| xs.iter().map(|s| (*s).into()).collect();
        super::search_entry(BundleListing {
            id: id.into(),
            version: "1.0.0".into(),
            kind: "Bundle".into(),
            scope: scope.into(),
            state: state.into(),
            enabled,
            removable: true,
            components: BundleListingComponents {
                agents: to_vec(agents),
                skills: to_vec(skills),
                workflows: to_vec(workflows),
                ..BundleListingComponents::default()
            },
            ..BundleListing::default()
        })
    }

    #[tokio::test]
    async fn refresh_without_daemon_succeeds_without_starting_one() {
        let root = std::env::temp_dir().join(format!(
            "hya-cli-no-refresh-{}",
            hya_proto::SessionId::new()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("hya.db");
        super::run(super::BundleCommand::Refresh, db.to_str().unwrap().into())
            .await
            .unwrap();
        assert!(crate::daemon::running(db.to_str().unwrap()).await.is_none() && !db.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_refresh_reports_all_sources_but_targeted_changes_only_the_target() {
        let errors = vec![hya_api::v1::BundleRefreshError {
            bundle_id: "other".into(),
            message: "invalid config".into(),
        }];
        assert!(super::report_refresh_errors(None, &[]).is_ok());
        assert!(super::report_refresh_errors(Some("alpha"), &errors).is_ok());
        assert!(
            super::report_refresh_errors(None, &errors)
                .unwrap_err()
                .to_string()
                .contains("other: invalid config")
        );
    }

    #[tokio::test]
    async fn hot_reload_cli_reports_refresh_response_errors() {
        let root =
            std::env::temp_dir().join(format!("hya-cli-refresh-{}", hya_proto::SessionId::new()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("hya.db");
        let owner = hya_store::SessionStore::connect_runtime(db.to_str().unwrap())
            .await
            .unwrap();
        let paths = crate::db_lock::paths(db.to_str().unwrap()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route(
                "/v1/health",
                axum::routing::get(|| async { axum::Json(serde_json::json!({"ok":true})) }),
            )
            .route(
                "/v1/bundles:refresh",
                axum::routing::post(
                    |axum::Json(req): axum::Json<serde_json::Value>| async move {
                        assert_eq!(
                            req["directory"],
                            std::env::current_dir().unwrap().to_string_lossy().as_ref()
                        );
                        axum::Json(hya_api::v1::RefreshBundlesResponse {
                            scope: "global".into(),
                            generation: 1,
                            bundles: Vec::new(),
                            errors: vec![hya_api::v1::BundleRefreshError {
                                bundle_id: "alpha".into(),
                                message: "alpha failed to initialize".into(),
                            }],
                        })
                    },
                ),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::fs::write(
            &paths.discovery,
            serde_json::to_vec(&serde_json::json!({"url":url,"pid":std::process::id(),"version":"test","startedAt":0})).unwrap(),
        )
        .unwrap();
        let result = super::run(super::BundleCommand::Refresh, db.to_str().unwrap().into()).await;
        server.abort();
        let _ = server.await;
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("alpha failed to initialize")
        );
    }

    #[test]
    fn info_file_formats_declarations_and_rejects_bad_suffix_and_private_format() {
        let dir =
            std::env::temp_dir().join(format!("hya-bundle-info-{}", hya_proto::SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = hya_bundle::BundleSource::new(
            "decl-demo",
            vec![
                hya_bundle::SourceFile::new(
                    "bundle.yaml",
                    br#"schema_version: 2
kind: Bundle
identity: { id: hya/decl-demo, version: 1.0.0, publisher: hya }
schemas: [{ scheme: db, tool: query, writable: false }]
apis:
  - { id: usage, method: GET, scope: session, path: /usage, description: Token usage }
  - { id: put-item, method: PUT, scope: global, path: "/items/{id}", request_schema: schemas/item.json, response_schema: schemas/item.json }
permission_modes:
  - { id: careful, title: Careful, description: Approves read-only commands }
  - { id: ci, title: CI }
resources:
  tools: [{ id: query, path: extensions/runtime.js }]
  mcp: [{ id: vecdb, path: mcp/vecdb.json }]
  hooks: [{ id: permission.approve, path: hooks/permission-approve.json }]
runtime: { kind: js, mode: process, entry: extensions/runtime.js, command: [bun, run, extensions/runtime.ts] }
assets:
  - { id: item-schema, path: schemas/item.json }
agents: [{ id: decl-lead, role: main, resource_view: { allow: [query, runtime] } }]
"#,
                ),
                hya_bundle::SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
                hya_bundle::SourceFile::new("schemas/item.json", br#"{"type":"object"}"#.to_vec()),
                hya_bundle::SourceFile::new("hooks/permission-approve.json", b"{}".to_vec()),
                hya_bundle::SourceFile::new("mcp/vecdb.json", br#"{"command":["python3","vecdb.py"]}"#.to_vec()),
            ],
        );
        let pkg = dir.join("decl-demo.hyabundle");
        let bytes = hya_bundle::write_public_package(&source).unwrap();
        std::fs::write(&pkg, &bytes).unwrap();

        let info = super::format_info_file(&pkg).unwrap();
        for expected in [
            "format: public-v1",
            "name: hya/decl-demo",
            "origin: package",
            "state: inspected",
            "schema: db tool=query writable=false",
            "process: bun command=bun run extensions/runtime.ts",
            "mcp: bundle:hya/decl-demo/mcp/vecdb",
            "api: PUT global /items/{id} id=put-item request_schema=schemas/item.json response_schema=schemas/item.json",
            "api: GET session /usage id=usage description=Token usage",
            "permission_mode: hya/decl-demo/careful title=Careful description=Approves read-only commands",
            "permission_mode: hya/decl-demo/ci title=CI",
        ] {
            assert!(
                info.lines().any(|l| l == expected),
                "missing {expected:?}:\n{info}"
            );
        }
        for bad_name in ["demo.HYABUNDLE", "demo.hyabundle.extra"] {
            let bad = dir.join(bad_name);
            std::fs::write(&bad, &bytes).unwrap();
            assert!(
                super::format_info_file(&bad)
                    .unwrap_err()
                    .to_string()
                    .contains("exact lowercase .hyabundle suffix")
            );
        }
        let mut private = b"HYABNDL\0".to_vec();
        for v in [1_u16, 1, 1, 24, 12, 16] {
            private.extend_from_slice(&v.to_le_bytes());
        }
        private.extend_from_slice(&1_u64.to_le_bytes());
        private.extend_from_slice(&[0x6e; 32]);
        private.extend_from_slice(b"x86_64-unknown-linux-gnu");
        private.extend_from_slice(&[0; 29]);
        let priv_path = dir.join("private.hyabundle");
        std::fs::write(&priv_path, private).unwrap();
        assert!(
            super::format_info_file(&priv_path)
                .unwrap_err()
                .to_string()
                .contains("invalid bundle package format")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_entry_formatting_filters_and_falls_back_including_unreadable_rows() {
        let entries = vec![
            listing(
                "hya/goal-loop",
                "first_party",
                "active",
                true,
                &["hya-goal-guide"],
                &["goal-contract"],
                &[],
            ),
            listing(
                "hya/wf",
                "user",
                "active",
                true,
                &["demo-worker"],
                &[],
                &["demo"],
            ),
            listing(
                "hya/broken-on",
                "user",
                "unreadable",
                true,
                &["x"],
                &[],
                &[],
            ),
            listing(
                "hya/broken-off",
                "project",
                "unreadable",
                false,
                &["x"],
                &[],
                &[],
            ),
        ];
        let table = super::format_list_rows(entries.iter().map(|e| &e.row));
        for row in [
            "NAME VERSION AGENT STATE KIND WORKFLOW SCOPE\n",
            "hya/goal-loop 1.0.0 hya-goal-guide active Bundle - builtin\n",
            "hya/wf 1.0.0 demo-worker active Bundle demo user\n",
            "hya/broken-on 1.0.0 - unreadable (reinstall) - - user\n",
            "hya/broken-off 1.0.0 - disabled, unreadable (reinstall) - - project\n",
        ] {
            assert!(table.contains(row), "missing {row:?}:\n{table}");
        }
        let (matched, fallback) = super::filter_search_entries(&entries, "GOAL-CONTRACT").unwrap();
        assert!(!fallback && matched.len() == 1 && matched[0].name == "hya/goal-loop");
        let (all, fallback) = super::filter_search_entries(&entries, "schemas").unwrap();
        assert!(fallback && all.len() == entries.len());
        assert!(super::filter_search_entries(&entries, "   ").is_err());
        assert!(super::parse_search_query("   ").is_err());
        assert_eq!(super::parse_search_query("ok").unwrap(), "ok");
    }

    #[test]
    fn schema_rows_confirmation_prompts_and_scope_helpers() {
        assert_eq!(super::format_schema_rows(&[]), "SCHEME TOOL WRITABLE\n");
        assert_eq!(
            super::format_schema_rows(&[hya_bundle::PreparedSchema {
                scheme: "db".into(),
                tool: "query".into(),
                writable: false,
            }]),
            "SCHEME TOOL WRITABLE\ndb query false\n"
        );
        let mut out = Vec::new();
        super::confirm_from(
            "Install x\n",
            "install",
            true,
            std::io::Cursor::new(""),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        super::confirm_from(
            "Install x\n",
            "install",
            false,
            std::io::Cursor::new("YES\n"),
            &mut out,
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("[y/N]"));
        for input in ["n\n", ""] {
            let err = super::confirm_from(
                "Remove x\n",
                "remove",
                false,
                std::io::Cursor::new(input),
                &mut Vec::new(),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("cancelled"), "{input:?}: {err}");
        }
        assert_eq!(
            (
                super::ScopeArgs::default().target(),
                super::ScopeArgs::default().filter()
            ),
            (super::Scope::User, None)
        );
        assert_eq!(
            super::ScopeArgs {
                project: true,
                user: false
            }
            .target(),
            super::Scope::Project
        );
        assert_eq!(
            super::ScopeArgs {
                project: false,
                user: true
            }
            .filter(),
            Some(super::Scope::User)
        );
        assert_eq!(
            super::action_label(&super::BundleInstallAction::Replace {
                installed_version: "1.0.0".into()
            }),
            "replace from=1.0.0"
        );
    }

    #[tokio::test]
    async fn install_dispatch_rejects_missing_or_conflicting_sources() {
        let p = super::NamespaceInstallPolicy::DenyConflicts;
        let both = super::install_dispatch(
            Some("a.hyabundle".into()),
            Some("plugin".into()),
            p,
            super::Scope::User,
            true,
            "",
        )
        .await
        .unwrap_err()
        .to_string();
        let neither = super::install_dispatch(None, None, p, super::Scope::User, true, "")
            .await
            .unwrap_err()
            .to_string();
        assert!(both.contains("not both") && neither.contains("requires a bundle package"));
    }

    #[test]
    fn pack_writes_deterministic_archive_and_rejects_bad_paths() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-bundle");
        std::fs::create_dir_all(src.join("commands")).unwrap();
        std::fs::create_dir_all(src.join("agents")).unwrap();
        std::fs::write(
            src.join("bundle.yaml"),
            "kind: Bundle\nschema_version: 2\nidentity: { id: acme/pack-demo, version: 1.2.3, publisher: acme }\nruntime: { kind: static }\nresources:\n  commands: [{ id: hello, path: commands/hello.md }]\nagents:\n  - path: agents/helper.md\n",
        )
        .unwrap();
        std::fs::write(
            src.join("commands/hello.md"),
            "---\nname: hello\ndescription: Say hi\n---\nHello $ARGUMENTS\n",
        )
        .unwrap();
        std::fs::write(
            src.join("agents/helper.md"),
            "---\nid: pack-helper\nrole: subagent\ndescription: Helper\n---\nBe helpful.\n",
        )
        .unwrap();
        // Undeclared test/readme file should not prevent packing.
        std::fs::write(src.join("README.md"), "# Demo\n").unwrap();

        let out_a = dir.path().join("out/a.hyabundle");
        let out_b = dir.path().join("out/b.hyabundle");
        super::pack(&src, &out_a).unwrap();
        super::pack(&src, &out_b).unwrap();
        let bytes_a = std::fs::read(&out_a).unwrap();
        let bytes_b = std::fs::read(&out_b).unwrap();
        assert_eq!(bytes_a, bytes_b);
        let inspected = hya_bundle::inspect_public_package(&bytes_a).unwrap();
        assert_eq!(inspected.bundles()[0].identity().id, "acme/pack-demo");
        assert_eq!(inspected.bundles()[0].commands().len(), 1);

        // Reject bad output extension and non-dir source.
        assert!(super::pack(&src, &dir.path().join("bad.zip")).is_err());
        assert!(super::pack(&out_a, &dir.path().join("c.hyabundle")).is_err());
    }

    #[tokio::test]
    async fn verify_strict_validates_descriptors_and_rejects_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("strict-bundle");
        std::fs::create_dir_all(src.join("mcp")).unwrap();
        std::fs::write(
            src.join("bundle.yaml"),
            "kind: Bundle\nschema_version: 2\nidentity: { id: acme/strict-ok, version: 1.0.0, publisher: acme }\nruntime: { kind: static }\nresources:\n  mcp: [{ id: docs, path: mcp/docs.json }]\n",
        )
        .unwrap();
        std::fs::write(
            src.join("mcp/docs.json"),
            r#"{"transport":"http","url":"https://mcp.example.com/v1","headers":{"Authorization":"Bearer ${env:DOCS_TOKEN}"}}"#,
        )
        .unwrap();

        super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            false,
        )
        .await
        .unwrap();

        // Also verify packed .hyabundle with --strict.
        let pkg = dir.path().join("strict-ok.hyabundle");
        super::pack(&src, &pkg).unwrap();
        super::verify(
            &pkg,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            false,
        )
        .await
        .unwrap();

        // Hardcoded Bearer secret in MCP headers must fail --strict.
        std::fs::write(
            src.join("mcp/docs.json"),
            r#"{"transport":"http","url":"https://mcp.example.com/v1","headers":{"Authorization":"Bearer supersecretliteralvalue12345678"}}"#,
        )
        .unwrap();
        let err = super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("hardcoded secret"), "{err}");

        // Unknown placeholder in MCP must fail --strict.
        std::fs::write(
            src.join("mcp/docs.json"),
            r#"{"transport":"http","url":"https://mcp.example.com/${UNKNOWN_VAR}"}"#,
        )
        .unwrap();
        let err = super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("unsupported placeholder"), "{err}");

        // PEM private key in config.example.yml must fail --strict.
        std::fs::write(
            src.join("mcp/docs.json"),
            r#"{"transport":"http","url":"https://mcp.example.com/v1"}"#,
        )
        .unwrap();
        std::fs::write(
            src.join("config.example.yml"),
            "key: |\n  -----BEGIN PRIVATE KEY-----\n  MIIBVQIBADANBg==\n  -----END PRIVATE KEY-----\n",
        )
        .unwrap();
        let err = super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("PEM private key"), "{err}");
    }

    #[tokio::test]
    async fn verify_protocol_smoke_checks_initialize_declarations_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("smoke-bundle");
        std::fs::create_dir_all(src.join("tools")).unwrap();
        std::fs::create_dir_all(src.join("hooks")).unwrap();
        std::fs::create_dir_all(src.join("schemas")).unwrap();
        std::fs::write(
            src.join("bundle.yaml"),
            r#"kind: Bundle
schema_version: 2
identity: { id: acme/smoke, version: 1.0.0, publisher: acme }
runtime:
  kind: js
  mode: process
  command: [python3, '${BUNDLE_ROOT}/plugin.py']
resources:
  tools:
    - { id: ping, path: tools/ping.json }
  hooks:
    - { id: session.start, path: hooks/hooks.json }
apis:
  - { id: health, method: GET, scope: global, path: /health, response_schema: schemas/health.json }
assets:
  - { id: plugin, path: plugin.py }
  - { id: health-schema, path: schemas/health.json }
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("tools/ping.json"),
            r#"{"schema_version":1,"name":"ping","description":"Ping tool.","permission":"read_only","parameters":{"type":"object"}}"#,
        )
        .unwrap();
        std::fs::write(
            src.join("hooks/hooks.json"),
            r#"{"schema_version":1,"hooks":[{"event":"session.start","handler":{"kind":"rpc","method":"hook/session.start"}}]}"#,
        )
        .unwrap();
        std::fs::write(src.join("schemas/health.json"), r#"{"type":"object"}"#).unwrap();
        let matching_plugin = r#"import json, sys
for line in sys.stdin:
    req = json.loads(line)
    m = req.get("method")
    rid = req.get("id")
    if m == "initialize":
        resp = {
            "jsonrpc": "2.0",
            "id": rid,
            "result": {
                "protocol_version": 1,
                "plugin": {"id": "smoke", "version": "1.0.0", "kind": "bun"},
                "tools": [{"name": "ping", "description": "Ping tool.", "inputSchema": {"type": "object"}}],
                "hooks": [{"name": "session.start"}],
                "apis": [{"name": "health"}]
            }
        }
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    elif m == "shutdown":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {}}) + "\n")
        sys.stdout.flush()
        sys.exit(0)
"#;
        std::fs::write(src.join("plugin.py"), matching_plugin).unwrap();

        // Directory source verify with --strict --protocol-smoke succeeds.
        super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            true,
        )
        .await
        .unwrap();

        // Packed .hyabundle verify with --strict --protocol-smoke also succeeds.
        let pkg = dir.path().join("smoke.hyabundle");
        super::pack(&src, &pkg).unwrap();
        super::verify(
            &pkg,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            true,
            true,
        )
        .await
        .unwrap();

        // Mismatched tool declaration must fail --protocol-smoke.
        let mismatch_plugin = matching_plugin.replace(r#""name": "ping""#, r#""name": "pong""#);
        std::fs::write(src.join("plugin.py"), mismatch_plugin).unwrap();
        let err = super::verify(
            &src,
            super::NamespaceInstallPolicy::DenyConflicts,
            super::Scope::User,
            false,
            true,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("tools mismatch"), "{err}");
    }
}
