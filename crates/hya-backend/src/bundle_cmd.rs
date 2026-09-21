use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Subcommand;
use hya_bundle::{
    BundleCatalog, PackageInspection, PreparedCatalog, PreparedInstallableBundle,
    PrivatePackageAuthentication, PrivatePackagePayload, cleanup_orphaned_staging, stage_package,
};
use hya_store::{
    BundleInstallOutcome, BundleRegistry, BundleRegistryRecord, BundleUninstallOutcome,
    NamespaceInstallPolicy, StoreError,
};

#[derive(Subcommand)]
pub(crate) enum BundleCommand {
    /// Install a bundle package or a Claude Code plugin source.
    Install {
        /// Bundle package (`.hyabundle`); conflicts with `--claude`.
        #[arg(value_name = "PACKAGE")]
        package: Option<PathBuf>,
        /// Claude Code plugin source: a local plugin directory translated
        /// offline by the bundled Claude adapter into an AgentBundle before
        /// installation (marketplace refs are a later milestone).
        #[arg(long, value_name = "SOURCE")]
        claude: Option<PathBuf>,
        /// Accept a namespace conflict (replacing the incumbent bundle) and
        /// same-bundle downgrades instead of failing.
        #[arg(long)]
        overwrite: bool,
    },
    /// List available bundles.
    List,
    /// Uninstall an installed bundle.
    Uninstall { name: String },
    /// Show bundle information by installed name or package file.
    Info {
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        name: Option<String>,
        #[arg(
            short = 'f',
            long,
            value_name = "FILE",
            required_unless_present = "name",
            conflicts_with = "name"
        )]
        file: Option<PathBuf>,
    },
    /// List URI-scheme extensions declared by installed bundles.
    Schemas,
}

pub(crate) async fn run(command: BundleCommand) -> anyhow::Result<()> {
    match command {
        BundleCommand::Install {
            package,
            claude,
            overwrite,
        } => install_dispatch(package, claude, overwrite).await,
        BundleCommand::List => list().await,
        BundleCommand::Uninstall { name } => uninstall(&name).await,
        BundleCommand::Info {
            name: Some(name),
            file: None,
        } => info(&name).await,
        BundleCommand::Info {
            file: Some(package),
            ..
        } => info_file(&package),
        BundleCommand::Info { .. } => anyhow::bail!("bundle info requires a bundle name"),
        BundleCommand::Schemas => schemas().await,
    }
}

/// Route `bundle install` to the package or Claude plugin source path.
async fn install_dispatch(
    package: Option<PathBuf>,
    claude: Option<PathBuf>,
    overwrite: bool,
) -> anyhow::Result<()> {
    match (package, claude) {
        (Some(package), None) => install(package, overwrite).await,
        (None, Some(source)) => {
            let staged = stage_claude_source(&source).await?;
            let result = install(staged.clone(), overwrite).await;
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

/// Translate a local Claude Code plugin directory offline into a staged
/// `.hyabundle` package via the bundled Claude adapter.
///
/// The adapter prints one JSON envelope (`hya-plugin-claude::emit`) with the
/// `AgentBundle` manifest and its translated files; they are prepared through
/// the canonical `hya_bundle` pipeline, so namespace and digest validation is
/// shared with every other install path.
async fn stage_claude_source(source: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        source.is_dir(),
        "--claude source must be a local plugin directory: {}",
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
    let output = tokio::process::Command::new(&bun)
        .arg("run")
        .arg(&adapter_main)
        .arg("--emit-bundle-manifest")
        .arg("--plugin-dir")
        .arg(source)
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

async fn install(package: PathBuf, overwrite: bool) -> anyhow::Result<()> {
    validate_package_path(&package)?;
    let inspection = inspect_package(&package)?;
    if let PackageInspection::Public(public) = &inspection {
        let first_party =
            hya_app::first_party_catalogs().context("decode embedded first-party bundles")?;
        let mut catalogs = first_party.iter().collect::<Vec<_>>();
        catalogs.push(&public.prepared);
        BundleCatalog::from_verified_catalogs(&catalogs)
            .context("validate package against immutable first-party catalog")?;
    }
    let identity = match &inspection {
        PackageInspection::Public(public) => public
            .prepared
            .bundles()
            .first()
            .map(PreparedInstallableBundle::identity)
            .cloned()
            .context("installed public package contains no bundle")?,
        PackageInspection::Private(_) => {
            return Err(StoreError::PrivateActivationUnsupported.into());
        }
    };
    let policy = if overwrite {
        NamespaceInstallPolicy::OverwriteConflicts
    } else {
        NamespaceInstallPolicy::DenyConflicts
    };
    let registry = open_registry().await?;
    let outcome = match registry
        .install_inspection(
            &reserved_agent_ids(),
            policy,
            inspection,
            hya_proto::now_millis(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(StoreError::NamespaceConflict {
            namespace,
            existing_bundle_id,
            incoming_bundle_id,
        }) => {
            anyhow::bail!(
                "NAMESPACE_CONFLICT: namespace {namespace} is owned by {existing_bundle_id}; \
                 rerun with --overwrite to replace it with {incoming_bundle_id}"
            );
        }
        Err(StoreError::BundleDowngradeRequired {
            bundle_id,
            installed_version,
            incoming_version,
        }) => {
            anyhow::bail!(
                "BUNDLE_DOWNGRADE_REQUIRED: {bundle_id} is installed at {installed_version}; \
                 rerun with --overwrite to install {incoming_version}"
            );
        }
        Err(error) => return Err(error.into()),
    };
    let (action, generation) = match outcome {
        BundleInstallOutcome::Installed { generation } => ("installed", generation),
        BundleInstallOutcome::Replaced { generation } => ("replaced", generation),
        BundleInstallOutcome::Unchanged { generation } => ("unchanged", generation),
    };
    println!(
        "{action} {} {} generation={generation}",
        identity.id, identity.version
    );
    Ok(())
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
    hya_core::BUILTIN_AGENTS
        .iter()
        .map(|agent| agent.id)
        .collect()
}

async fn list() -> anyhow::Result<()> {
    let first_party =
        hya_app::first_party_catalogs().context("decode embedded first-party bundles")?;
    let installed = installed_records_if_exists().await?;
    let mut rows = Vec::new();
    for catalog in &first_party {
        for bundle in catalog.bundles() {
            rows.push(bundle_list_row(bundle, "active"));
        }
    }
    rows.extend(installed.iter().map(|record| {
        match decode_installed_bundle(record) {
            Ok(bundle) => bundle_list_row(&bundle, "active"),
            // Written by a different binary version: name the row and tell the
            // operator what to do, rather than failing the whole list.
            Err(_) => (
                record.bundle_id.clone(),
                record.version.clone(),
                "-".to_string(),
                "unreadable (reinstall)".to_string(),
                "-".to_string(),
                "-".to_string(),
            ),
        }
    }));
    rows.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    println!("NAME VERSION AGENT STATE KIND WORKFLOW");
    for (bundle_id, version, agents, state, kind, workflow) in rows {
        println!("{bundle_id} {version} {agents} {state} {kind} {workflow}");
    }
    Ok(())
}

/// Present one prepared bundle as an owned, sortable CLI list row.
fn bundle_list_row(
    bundle: &PreparedInstallableBundle,
    state: &str,
) -> (String, String, String, String, String, String) {
    (
        bundle.identity().id.clone(),
        bundle.identity().version.clone(),
        bundle
            .agents()
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        state.to_string(),
        bundle.kind().as_str().to_string(),
        bundle
            .workflow()
            .map_or_else(|| "-".to_string(), |workflow| workflow.id.clone()),
    )
}

async fn info(bundle_id: &str) -> anyhow::Result<()> {
    if bundle_id == hya_app::FIRST_PARTY_BUNDLE_ID {
        return info_first_party();
    }
    let record = installed_records_if_exists()
        .await?
        .into_iter()
        .find(|record| record.bundle_id == bundle_id)
        .ok_or_else(|| StoreError::BundleNotFound {
            bundle_id: bundle_id.to_string(),
        })?;
    let prepared = decode_installed_catalog(&record)?;
    let [bundle] = prepared.bundles() else {
        anyhow::bail!("installed catalog must contain exactly one bundle")
    };

    let identity = bundle.identity();
    println!("name={}", identity.id);
    println!("version={}", identity.version);
    println!("publisher={}", identity.publisher);
    println!("origin=installed");
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
    );
    Ok(())
}

/// Print metadata for the immutable first-party bundles.
fn info_first_party() -> anyhow::Result<()> {
    let catalogs =
        hya_app::first_party_catalogs().context("decode embedded first-party bundles")?;
    for prepared in &catalogs {
        let [bundle] = prepared.bundles() else {
            anyhow::bail!("first-party catalog must contain exactly one bundle")
        };
        let identity = bundle.identity();
        println!("name={}", identity.id);
        println!("version={}", identity.version);
        println!("publisher={}", identity.publisher);
        println!("origin=first-party");
        println!("format=prepared-v2");
        println!("state=active");
        println!("immutable=true");
        println!("prepared_digest={}", prepared.digest());
        print_static_info(
            bundle,
            "=",
            prepared.bundle_schemas(&identity.id),
            prepared.bundle_process(&identity.id),
        );
    }
    Ok(())
}

async fn uninstall(bundle_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        bundle_id != hya_app::FIRST_PARTY_BUNDLE_ID,
        "immutable first-party bundle `{bundle_id}` cannot be uninstalled"
    );
    let registry = open_registry().await?;
    let BundleUninstallOutcome::Removed { generation } = registry.uninstall(bundle_id).await?;
    println!("uninstalled {bundle_id} generation={generation}");
    Ok(())
}

/// List URI-scheme extensions across the first-party and installed bundles:
/// one `BUNDLE SCHEME TOOL WRITABLE` row per declared schema.
async fn schemas() -> anyhow::Result<()> {
    let first_party =
        hya_app::first_party_catalogs().context("decode embedded first-party bundles")?;
    let mut rows = Vec::new();
    for catalog in &first_party {
        rows.extend(catalog_schema_rows(catalog));
    }
    for record in installed_records_if_exists().await? {
        match decode_installed_catalog(&record) {
            Ok(prepared) => rows.extend(catalog_schema_rows(&prepared)),
            // Written by a different binary version: name the bundle rather
            // than failing the whole listing, matching `bundle list`.
            Err(_) => rows.push((
                record.bundle_id.clone(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            )),
        }
    }
    rows.sort_by(|left, right| {
        (left.0.as_bytes(), left.1.as_bytes()).cmp(&(right.0.as_bytes(), right.1.as_bytes()))
    });

    println!("BUNDLE SCHEME TOOL WRITABLE");
    for (bundle_id, scheme, tool, writable) in rows {
        println!("{bundle_id} {scheme} {tool} {writable}");
    }
    Ok(())
}

/// Schema rows contributed by one decoded prepared catalog.
fn catalog_schema_rows(prepared: &PreparedCatalog) -> Vec<(String, String, String, String)> {
    prepared
        .schemas()
        .iter()
        .flat_map(|row| {
            row.schemas
                .iter()
                .map(|schema| {
                    (
                        row.bundle_id.clone(),
                        schema.scheme.clone(),
                        schema.tool.clone(),
                        schema.writable.to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
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

async fn installed_records_if_exists() -> anyhow::Result<Vec<BundleRegistryRecord>> {
    let path = hya_app::bundle_registry_path();
    if !path
        .try_exists()
        .with_context(|| format!("inspect bundle registry path {}", path.display()))?
    {
        return Ok(Vec::new());
    }
    let path = path
        .to_str()
        .context("bundle registry path is not valid UTF-8")?;
    let registry = BundleRegistry::connect(path)
        .await
        .context("open bundle registry")?;
    Ok(registry.snapshot().await?.bundles)
}

/// Print the static metadata of one prepared bundle, including its declared
/// schemas, optional `extensions.process` declaration, and mcp entries. The
/// declaration lines print only when non-empty.
fn print_static_info(
    bundle: &PreparedInstallableBundle,
    separator: &str,
    schemas: &[hya_bundle::PreparedSchema],
    process: Option<&hya_bundle::PreparedProcessExtension>,
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
