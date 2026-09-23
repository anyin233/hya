use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand};
use hya_app::project_bundles::{
    ProjectBundle, ProjectBundleError, find_project_bundle, install_project_bundle,
    plan_project_install, project_bundles, project_bundles_dir, remove_project_bundle,
};
use hya_bundle::{
    BundleCatalog, PackageInspection, PreparedCatalog, PreparedInstallableBundle,
    PrivatePackageAuthentication, PrivatePackagePayload, PublicPackageInspection,
    cleanup_orphaned_staging, stage_package,
};
use hya_store::{
    BundleInstallAction, BundleInstallCandidate, BundleInstallOutcome, BundleInstallPlan,
    BundleRegistry, BundleRegistryRecord, BundleUninstallOutcome, NamespaceInstallPolicy,
    StoreError,
};

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
    /// Check that a package would install into a scope, without installing it.
    Verify {
        /// Bundle package (`.hyabundle`).
        #[arg(value_name = "PACKAGE")]
        package: PathBuf,
        /// Check as `install --overwrite` would.
        #[arg(long)]
        overwrite: bool,
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

pub(crate) async fn run(command: BundleCommand) -> anyhow::Result<()> {
    match command {
        BundleCommand::Install {
            package,
            claude,
            overwrite,
            scope,
            yes,
        } => install_dispatch(package, claude, policy(overwrite), scope.target(), yes).await,
        BundleCommand::List { scope } => list(scope.filter()).await,
        BundleCommand::Search { query, scope } => search(&query, scope.filter()).await,
        BundleCommand::Remove { name, scope, yes } => remove(&name, scope.target(), yes).await,
        BundleCommand::Verify {
            package,
            overwrite,
            scope,
        } => verify(&package, policy(overwrite), scope.target()).await,
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
) -> anyhow::Result<()> {
    match (package, claude) {
        (Some(package), None) => install(&package, policy, scope, yes).await,
        (None, Some(source)) => {
            let staged = stage_claude_source(&source).await?;
            let result = install(&staged, policy, scope, yes).await;
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
/// standard `Plugin`/`AgentSetBundle` manifest and its translated files; they are prepared through
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

/// Inspect a package for install into `scope`: it must be a public package
/// whose bundle neither overrides an immutable trusted preset nor breaks the
/// first-party catalog.
fn inspect_installable(package: &Path) -> anyhow::Result<PublicPackageInspection> {
    validate_package_path(package)?;
    let public = match inspect_package(package)? {
        PackageInspection::Public(public) => public,
        PackageInspection::Private(_) => {
            return Err(StoreError::PrivateActivationUnsupported.into());
        }
    };
    let [incoming] = public.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle");
    };
    if let Some(preset) = hya_app::trusted_preset_inventory()
        .context("decode trusted presets")?
        .iter()
        .find(|preset| preset.id == incoming.identity().id)
    {
        anyhow::bail!(
            "immutable trusted preset `{}` cannot be installed or overridden",
            preset.id
        );
    }
    let mut first_party = hya_app::first_party_catalogs().context("load first-party bundles")?;
    first_party.retain(|catalog| {
        catalog.bundles().first().is_none_or(|bundle| {
            bundle.identity().id != incoming.identity().id
                && bundle.namespace() != incoming.namespace()
        })
    });
    let mut catalogs = first_party.iter().collect::<Vec<_>>();
    catalogs.push(&public.prepared);
    BundleCatalog::from_verified_catalogs(&catalogs)
        .context("validate package against immutable first-party catalog")?;
    Ok(public)
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
    let reserved = reserved_agent_ids();
    match scope {
        Scope::User => {
            let registry_path = hya_app::bundle_registry_path();
            let plan = if let Some(registry) = registry {
                registry
                    .plan_install(&reserved, policy, &install_candidate(public))
                    .await
                    .map_err(explain_store_error)?
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
                    return Err(StoreError::BundleAgentIdReserved {
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
                .map_err(explain_project_error)?;
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

fn install_candidate(public: &PublicPackageInspection) -> BundleInstallCandidate {
    BundleInstallCandidate {
        source_digest: public.source_digest,
        prepared_digest: public.prepared.digest().to_owned(),
        prepared_bytes: public.prepared.bytes().to_vec(),
        installed_at: hya_proto::now_millis(),
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
    if yes {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{summary}Proceed? [y/N] ").context("write confirmation prompt")?;
    stderr.flush().context("flush confirmation prompt")?;
    drop(stderr);
    let mut answer = String::new();
    let read = std::io::stdin()
        .read_line(&mut answer)
        .context("read confirmation")?;
    if read == 0 {
        eprintln!();
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
) -> anyhow::Result<()> {
    let public = inspect_installable(package)?;
    let [bundle] = public.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle");
    };
    let identity = bundle.identity().clone();
    // One registry handle for plan and install: connecting again right after
    // dropping a handle can race the old pool's close and find the DB locked.
    let registry = match scope {
        Scope::User => Some(open_registry().await?),
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
        return Ok(());
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

    match scope {
        Scope::User => {
            let registry = registry.context("user registry handle missing")?;
            let outcome = registry
                .install(&reserved_agent_ids(), policy, install_candidate(&public))
                .await
                .map_err(explain_store_error)?;
            let (action, generation) = match outcome {
                BundleInstallOutcome::Installed { generation } => ("installed", generation),
                BundleInstallOutcome::Replaced { generation } => ("replaced", generation),
                BundleInstallOutcome::Unchanged { generation } => ("unchanged", generation),
            };
            println!(
                "{action} {} {} scope=user generation={generation}",
                identity.id, identity.version
            );
        }
        Scope::Project => {
            let plan = install_project_bundle(
                &project_dir()?,
                public.files,
                &reserved_agent_ids(),
                policy,
            )
            .map_err(explain_project_error)?;
            let action = match plan.action {
                BundleInstallAction::Install => "installed",
                BundleInstallAction::Replace { .. } => "replaced",
                BundleInstallAction::Unchanged => "unchanged",
            };
            println!(
                "{action} {} {} scope=project path={}",
                identity.id,
                identity.version,
                plan.target.display()
            );
        }
    }
    Ok(())
}

/// Check that `package` would install into `scope`, printing what install
/// would do. Writes nothing: no registry, no project directory.
async fn verify(
    package: &Path,
    policy: NamespaceInstallPolicy,
    scope: Scope,
) -> anyhow::Result<()> {
    let public = inspect_installable(package)?;
    let [bundle] = public.prepared.bundles() else {
        anyhow::bail!("public package must contain exactly one bundle");
    };
    let registry = match scope {
        Scope::User => existing_registry().await?,
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
    Ok(())
}

/// Turn the registry's overwrite-able conflicts into actionable messages.
fn explain_store_error(error: StoreError) -> anyhow::Error {
    match error {
        StoreError::NamespaceConflict {
            namespace,
            existing_bundle_id,
            incoming_bundle_id,
        } => anyhow::anyhow!(
            "NAMESPACE_CONFLICT: namespace {namespace} is owned by {existing_bundle_id}; \
             rerun with --overwrite to replace it with {incoming_bundle_id}"
        ),
        StoreError::BundleDowngradeRequired {
            bundle_id,
            installed_version,
            incoming_version,
        } => anyhow::anyhow!(
            "BUNDLE_DOWNGRADE_REQUIRED: {bundle_id} is installed at {installed_version}; \
             rerun with --overwrite to install {incoming_version}"
        ),
        error => error.into(),
    }
}

fn explain_project_error(error: ProjectBundleError) -> anyhow::Error {
    match error {
        ProjectBundleError::Store(StoreError::BundleContentConflict { bundle_id, version }) => {
            anyhow::anyhow!(
                "BUNDLE_CONTENT_CONFLICT: project bundle {bundle_id} {version} has different \
                 content; bump the version or rerun with --overwrite"
            )
        }
        ProjectBundleError::Store(error) => explain_store_error(error),
        error => error.into(),
    }
}

/// Project scope root: `.hya/bundles` under the current directory, the same
/// directory the runtime loads project bundles from.
fn project_dir() -> anyhow::Result<PathBuf> {
    project_bundles_dir().context("resolve the current directory for --project")
}

fn info_file(package: &Path) -> anyhow::Result<()> {
    validate_package_path(package)?;
    match inspect_package(package)? {
        PackageInspection::Public(inspection) => {
            let [bundle] = inspection.prepared.bundles() else {
                anyhow::bail!("public package must contain exactly one bundle")
            };
            let identity = bundle.identity();
            println!("format: public-v1");
            println!("name: {}", identity.id);
            println!("version: {}", identity.version);
            println!("publisher: {}", identity.publisher);
            println!("origin: package");
            println!("state: inspected");
            println!("immutable: false");
            println!("source_digest: {}", hex_digest(&inspection.source_digest));
            println!("prepared_digest: {}", inspection.prepared.digest());
            print_static_info(
                bundle,
                ": ",
                inspection.prepared.bundle_schemas(&identity.id),
                inspection.prepared.bundle_process(&identity.id),
                inspection.prepared.bundle_views(&identity.id),
            );
            Ok(())
        }
        PackageInspection::Private(inspection) => {
            let authentication = match inspection.authentication {
                PrivatePackageAuthentication::Unverified => "unverified",
            };
            let payload = match inspection.payload {
                PrivatePackagePayload::Opaque => "opaque",
            };

            println!("format: private-v1");
            println!("target: {}", inspection.target);
            println!("protocol_minimum: {}", inspection.protocol_minimum);
            println!("protocol_maximum: {}", inspection.protocol_maximum);
            println!("ciphertext_length: {}", inspection.ciphertext_length);
            println!(
                "ciphertext_digest: {}",
                hex_digest(&inspection.ciphertext_digest)
            );
            println!("authentication: {authentication}");
            println!("payload: {payload}");
            println!("activation: unsupported-in-{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn validate_package_path(package: &Path) -> anyhow::Result<()> {
    let has_exact_suffix = package
        .file_name()
        .and_then(|filename| filename.to_str())
        .is_some_and(|filename| filename.ends_with(".hyabundle"));
    anyhow::ensure!(has_exact_suffix, "exact lowercase .hyabundle suffix");
    Ok(())
}

/// Built-in agent ids an installed bundle must not claim.
fn reserved_agent_ids() -> Vec<&'static str> {
    hya_core::builtin_agents()
        .iter()
        .map(|agent| agent.id)
        .collect()
}

/// Every bundle across scopes as a `bundle list` row plus its search
/// metadata, mirroring the runtime layering: project bundles shadow
/// user-installed ones (id or namespace), and first-party bundles hide behind
/// any active user or project bundle. Shared by `list` and `search` so both
/// always cover the same bundles.
async fn catalog_entries() -> anyhow::Result<Vec<SearchEntry>> {
    let first_party = hya_app::first_party_catalogs().context("load first-party bundles")?;
    let installed = installed_records_if_exists().await?;
    let project = project_bundles(&project_dir()?);
    let project_ids = project
        .iter()
        .map(|bundle| bundle.bundle_id().to_string())
        .collect::<BTreeSet<_>>();
    let project_namespaces = project
        .iter()
        .map(|bundle| bundle.bundle().namespace().to_string())
        .collect::<BTreeSet<_>>();

    let mut rows = Vec::new();
    for preset in hya_app::trusted_preset_inventory().context("decode trusted presets")? {
        let mut haystack = preset.id.to_lowercase();
        for id in preset.agent_ids.iter().chain(preset.resource_ids.iter()) {
            haystack.push('\n');
            haystack.push_str(&id.to_lowercase());
        }
        rows.push(SearchEntry {
            haystack,
            row: BundleListRow {
                name: preset.id,
                version: preset.version,
                agents: preset.agent_ids.join(","),
                state: "active".to_string(),
                kind: preset.kind,
                workflow: "-".to_string(),
                scope: BUILTIN_SCOPE,
            },
        });
    }
    let mut higher_ids = project_ids.clone();
    let mut higher_namespaces = project_namespaces.clone();
    for record in &installed {
        match decode_installed_bundle(record) {
            Ok(bundle) => {
                let shadowed = project_ids.contains(&record.bundle_id)
                    || project_namespaces.contains(bundle.namespace());
                if !shadowed {
                    higher_ids.insert(record.bundle_id.clone());
                    higher_namespaces.insert(bundle.namespace().to_string());
                }
                let state = if shadowed { "shadowed" } else { "active" };
                rows.push(bundle_entry(&bundle, state, Scope::User.as_str()));
            }
            // Written by a different binary version: name the row and tell the
            // operator what to do, rather than failing the whole list. The
            // bundle id is the only searchable metadata left.
            Err(_) => rows.push(SearchEntry {
                haystack: record.bundle_id.to_lowercase(),
                row: BundleListRow {
                    name: record.bundle_id.clone(),
                    version: record.version.clone(),
                    agents: "-".to_string(),
                    state: "unreadable (reinstall)".to_string(),
                    kind: "-".to_string(),
                    workflow: "-".to_string(),
                    scope: Scope::User.as_str(),
                },
            }),
        }
    }
    for bundle in &project {
        rows.push(bundle_entry(
            bundle.bundle(),
            "active",
            Scope::Project.as_str(),
        ));
    }
    for catalog in &first_party {
        for bundle in catalog.bundles() {
            if higher_ids.contains(&bundle.identity().id)
                || higher_namespaces.contains(bundle.namespace())
            {
                continue;
            }
            rows.push(bundle_entry(bundle, "active", BUILTIN_SCOPE));
        }
    }
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

/// One prepared bundle as a list row plus its search metadata.
fn bundle_entry(
    bundle: &PreparedInstallableBundle,
    state: &str,
    scope: &'static str,
) -> SearchEntry {
    SearchEntry {
        haystack: bundle_search_haystack(bundle),
        row: bundle_list_row(bundle, state, scope),
    }
}

/// Scope label for the bundles shipped with hya (trusted presets and
/// first-party bundles).
const BUILTIN_SCOPE: &str = "builtin";

/// Present one prepared bundle as an owned, sortable CLI list row.
fn bundle_list_row(
    bundle: &PreparedInstallableBundle,
    state: &str,
    scope: &'static str,
) -> BundleListRow {
    BundleListRow {
        name: bundle.identity().id.clone(),
        version: bundle.identity().version.clone(),
        agents: bundle
            .agents()
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        state: state.to_string(),
        kind: bundle.kind().as_str().to_string(),
        workflow: bundle
            .workflow()
            .map_or_else(|| "-".to_string(), |workflow| workflow.id.clone()),
        scope,
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
    println!("NAME VERSION AGENT STATE KIND WORKFLOW SCOPE");
    for row in rows {
        println!(
            "{} {} {} {} {} {} {}",
            row.name, row.version, row.agents, row.state, row.kind, row.workflow, row.scope
        );
    }
}

/// Lowercased metadata one bundle contributes to `bundle search`: its bundle
/// id plus every agent id and skill id (local and stable) it declares.
fn bundle_search_haystack(bundle: &PreparedInstallableBundle) -> String {
    let mut haystack = bundle.identity().id.to_lowercase();
    for agent in bundle.agents() {
        haystack.push('\n');
        haystack.push_str(&agent.id.as_str().to_lowercase());
    }
    for skill in bundle.skills() {
        haystack.push('\n');
        haystack.push_str(&skill.local_id.to_lowercase());
        haystack.push('\n');
        haystack.push_str(&skill.stable_id.to_lowercase());
    }
    haystack
}

/// Search every scope (or one, with `--user`/`--project`): a
/// case-insensitive substring query over bundle ids, agent ids, and skill ids
/// prints matching bundles as `bundle list` rows, with the same scope and
/// state `list` reports. When no metadata matches — a query naming a
/// subcommand like `schema` rather than bundle metadata — every bundle in the
/// searched scope prints instead and the fallback is explained on stderr.
async fn search(query: &str, filter: Option<Scope>) -> anyhow::Result<()> {
    let needle = query.trim().to_lowercase();
    anyhow::ensure!(!needle.is_empty(), "bundle search requires a query");
    let entries = scoped_entries(filter).await?;
    let matched = entries
        .iter()
        .filter(|entry| entry.haystack.contains(needle.as_str()))
        .collect::<Vec<_>>();
    if matched.is_empty() {
        eprintln!("hya: no bundle metadata matched `{query}`; listing every bundle instead");
        print_list_rows(entries.iter().map(|entry| &entry.row));
        return Ok(());
    }
    print_list_rows(matched.iter().map(|entry| &entry.row));
    Ok(())
}

async fn info(bundle_id: &str, filter: Option<Scope>) -> anyhow::Result<()> {
    let not_found = || -> anyhow::Error {
        StoreError::BundleNotFound {
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
            let bundle =
                find_project_bundle(&project_dir()?, bundle_id).map_err(explain_project_error)?;
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
    let Some(record) = installed_records_if_exists()
        .await?
        .into_iter()
        .find(|record| record.bundle_id == bundle_id)
    else {
        return Ok(false);
    };
    let prepared = decode_installed_catalog(&record)?;
    let [bundle] = prepared.bundles() else {
        anyhow::bail!("installed catalog must contain exactly one bundle")
    };

    let identity = bundle.identity();
    println!("name={}", identity.id);
    println!("version={}", identity.version);
    println!("publisher={}", identity.publisher);
    println!("origin=installed");
    println!("scope=user");
    println!("format=public-v1");
    println!("state=active");
    println!("immutable=false");
    println!("source_digest={}", hex_digest(&record.source_digest));
    println!("prepared_digest={}", record.prepared_digest);
    print_static_info(
        bundle,
        "=",
        prepared.bundle_schemas(bundle_id),
        prepared.bundle_process(bundle_id),
        prepared.bundle_views(bundle_id),
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
        prepared.bundle_views(&identity.id),
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
        println!("format=prepared-v2");
        println!("state=active");
        println!("immutable=true");
        println!("prepared_digest={}", prepared.digest());
        print_static_info(
            bundle,
            "=",
            prepared.bundle_schemas(&identity.id),
            prepared.bundle_process(&identity.id),
            prepared.bundle_views(&identity.id),
        );
        return Ok(());
    }
    Err(StoreError::BundleNotFound {
        bundle_id: bundle_id.to_string(),
    }
    .into())
}

async fn remove(bundle_id: &str, scope: Scope, yes: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !hya_app::trusted_preset_inventory()
            .context("decode trusted presets")?
            .iter()
            .any(|preset| preset.id == bundle_id),
        "immutable trusted preset `{bundle_id}` cannot be removed"
    );
    match scope {
        Scope::User => {
            let registry = existing_registry().await?;
            let installed = match &registry {
                Some(registry) => registry
                    .snapshot()
                    .await?
                    .bundles
                    .into_iter()
                    .find(|record| record.bundle_id == bundle_id),
                None => None,
            };
            let Some(record) = installed else {
                let first_party = hya_app::first_party_catalogs()
                    .context("load first-party bundles")?
                    .iter()
                    .any(|catalog| {
                        catalog
                            .bundles()
                            .iter()
                            .any(|bundle| bundle.identity().id == bundle_id)
                    });
                anyhow::ensure!(
                    !first_party,
                    "immutable first-party bundle `{bundle_id}` cannot be removed"
                );
                return Err(StoreError::BundleNotFound {
                    bundle_id: bundle_id.to_string(),
                }
                .into());
            };
            let registry_path = hya_app::bundle_registry_path();
            confirm(
                &format!(
                    "Remove {bundle_id} {} from user scope\n  registry: {}\n",
                    record.version,
                    registry_path.display()
                ),
                "remove",
                yes,
            )?;
            let registry = registry.context("user registry handle missing")?;
            let BundleUninstallOutcome::Removed { generation } =
                registry.uninstall(bundle_id).await?;
            println!("removed {bundle_id} scope=user generation={generation}");
        }
        Scope::Project => {
            let dir = project_dir()?;
            let bundle = find_project_bundle(&dir, bundle_id).map_err(explain_project_error)?;
            confirm(
                &format!(
                    "Remove {bundle_id} {} from project scope\n  deletes: {}\n",
                    bundle.version(),
                    bundle.dir().display()
                ),
                "remove",
                yes,
            )?;
            let removed = remove_project_bundle(&dir, bundle_id).map_err(explain_project_error)?;
            println!(
                "removed {bundle_id} scope=project path={}",
                removed.dir().display()
            );
        }
    }
    Ok(())
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
        validate_package_path(as_file)?;
        let PackageInspection::Public(public) = inspect_package(as_file)? else {
            anyhow::bail!("private packages do not expose their schema declarations");
        };
        let [bundle] = public.prepared.bundles() else {
            anyhow::bail!("public package must contain exactly one bundle");
        };
        print_schema_rows(public.prepared.bundle_schemas(&bundle.identity().id));
        return Ok(());
    }
    let not_found = || -> anyhow::Error {
        StoreError::BundleNotFound {
            bundle_id: name.to_string(),
        }
        .into()
    };
    let project = || -> anyhow::Result<Option<ProjectBundle>> {
        Ok(find_project_bundle(&project_dir()?, name).ok())
    };
    let installed =
        || async {
            let Some(record) = installed_records_if_exists()
                .await?
                .into_iter()
                .find(|record| record.bundle_id == name)
            else {
                return anyhow::Ok(None);
            };
            decode_installed_catalog(&record).map(Some).with_context(|| {
            format!("installed bundle {name} is unreadable; reinstall it with `hya bundle install`")
        })
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
    println!("SCHEME TOOL WRITABLE");
    for schema in schemas {
        println!("{} {} {}", schema.scheme, schema.tool, schema.writable);
    }
}

/// Decode one installed record's full prepared catalog, verifying identity.
fn decode_installed_catalog(record: &BundleRegistryRecord) -> anyhow::Result<PreparedCatalog> {
    let corrupt = || StoreError::BundleRegistryCorrupt {
        bundle_id: record.bundle_id.clone(),
    };
    let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
        .map_err(|_| corrupt())?;
    let [bundle] = prepared.bundles() else {
        return Err(corrupt().into());
    };
    let identity = bundle.identity();
    if identity.id.as_str() != record.bundle_id.as_str()
        || identity.version.as_str() != record.version.as_str()
        || identity.publisher.as_str() != record.publisher.as_str()
    {
        return Err(corrupt().into());
    }
    Ok(prepared)
}

fn inspect_package(package: &Path) -> anyhow::Result<PackageInspection> {
    let registry_path = hya_app::bundle_registry_path();
    let registry_parent = registry_path
        .parent()
        .context("bundle registry path has no parent")?;
    let staging_root = registry_parent.join("staging");
    cleanup_orphaned_staging(&staging_root).context("clean bundle staging directory")?;
    stage_package(package, &staging_root)
        .with_context(|| format!("stage bundle package {}", package.display()))?
        .inspect()
        .with_context(|| format!("inspect bundle package {}", package.display()))
}

async fn open_registry() -> anyhow::Result<BundleRegistry> {
    let path = hya_app::bundle_registry_path();
    let parent = path
        .parent()
        .context("bundle registry path has no parent")?
        .to_path_buf();
    fs::create_dir_all(&parent)
        .with_context(|| format!("create bundle registry directory {}", parent.display()))?;
    let path = path
        .to_str()
        .context("bundle registry path is not valid UTF-8")?;
    let registry = BundleRegistry::connect(path)
        .await
        .context("open bundle registry")?;
    Ok(registry)
}

/// Open the user registry only when it already exists, so read-only
/// commands never create it.
async fn existing_registry() -> anyhow::Result<Option<BundleRegistry>> {
    let path = hya_app::bundle_registry_path();
    if !path
        .try_exists()
        .with_context(|| format!("inspect bundle registry path {}", path.display()))?
    {
        return Ok(None);
    }
    let path = path
        .to_str()
        .context("bundle registry path is not valid UTF-8")?;
    let registry = BundleRegistry::connect(path)
        .await
        .context("open bundle registry")?;
    Ok(Some(registry))
}

async fn installed_records_if_exists() -> anyhow::Result<Vec<BundleRegistryRecord>> {
    match existing_registry().await? {
        Some(registry) => Ok(registry.snapshot().await?.bundles),
        None => Ok(Vec::new()),
    }
}

/// Print the static metadata of one prepared bundle, including its declared
/// schemas, optional `extensions.process` declaration, read-only views, and
/// mcp entries. The declaration lines print only when non-empty.
fn print_static_info(
    bundle: &PreparedInstallableBundle,
    separator: &str,
    schemas: &[hya_bundle::PreparedSchema],
    process: Option<&hya_bundle::PreparedProcessExtension>,
    views: &[hya_bundle::PreparedView],
) {
    println!("kind{separator}{}", bundle.kind().as_str());
    if let Some(workflow) = bundle.workflow() {
        println!("workflow{separator}{}", workflow.id);
    }
    for agent in bundle.agents() {
        println!("agent{separator}{}", agent.id);
    }
    for skill in bundle.skills() {
        println!("skill{separator}{}", skill.stable_id);
    }
    for tool in bundle.tools() {
        println!("tool{separator}{}", tool.stable_id);
    }
    for mcp in bundle.mcp() {
        println!("mcp{separator}{}", mcp.stable_id);
    }
    for hook in bundle.hooks() {
        println!("hook{separator}{}", hook.stable_id);
    }
    for extension in bundle.extensions() {
        println!("extension{separator}{}", extension.stable_id);
    }
    for schema in schemas {
        println!(
            "schema{separator}{scheme} tool={tool} writable={writable}",
            scheme = schema.scheme,
            tool = schema.tool,
            writable = schema.writable
        );
    }
    if let Some(process) = process {
        println!(
            "process{separator}{kind} command={command}",
            kind = process.kind.as_str(),
            command = process.command.join(" ")
        );
    }
    for view in views {
        if view.description.is_empty() {
            println!("view{separator}{}", view.id);
        } else {
            println!(
                "view{separator}{id} description={description}",
                id = view.id,
                description = view.description
            );
        }
    }
}

fn decode_installed_bundle(
    record: &BundleRegistryRecord,
) -> anyhow::Result<PreparedInstallableBundle> {
    let corrupt = || StoreError::BundleRegistryCorrupt {
        bundle_id: record.bundle_id.clone(),
    };
    let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
        .map_err(|_| corrupt())?;
    let [bundle] = prepared.bundles() else {
        return Err(corrupt().into());
    };
    let identity = bundle.identity();
    if identity.id.as_str() != record.bundle_id.as_str()
        || identity.version.as_str() != record.version.as_str()
        || identity.publisher.as_str() != record.publisher.as_str()
    {
        return Err(corrupt().into());
    }
    Ok(bundle.clone())
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
