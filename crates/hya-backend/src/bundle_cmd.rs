use std::collections::BTreeSet;
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
        /// Claude Code plugin source: a local directory or
        /// `<marketplace-root>#<entry>` translated into a standard bundle.
        #[arg(long, value_name = "SOURCE")]
        claude: Option<PathBuf>,
        /// Accept a namespace conflict (replacing the incumbent bundle) and
        /// same-bundle downgrades instead of failing.
        #[arg(long)]
        overwrite: bool,
    },
    /// List available bundles.
    List,
    /// Search bundles by a case-insensitive substring over bundle ids,
    /// agent ids, and skill ids.
    Search {
        /// Substring matched against bundle ids, agent ids, and skill ids
        /// (case-insensitive).
        #[arg(value_name = "QUERY", value_parser = parse_search_query)]
        query: String,
    },
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
        } => install_dispatch(package, claude, overwrite).await,
        BundleCommand::List => list().await,
        BundleCommand::Search { query } => search(&query).await,
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

async fn install(package: PathBuf, overwrite: bool) -> anyhow::Result<()> {
    validate_package_path(&package)?;
    let inspection = inspect_package(&package)?;
    if let PackageInspection::Public(public) = &inspection {
        let incoming = public
            .prepared
            .bundles()
            .first()
            .context("installed public package contains no bundle")?;
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
        let mut first_party =
            hya_app::first_party_catalogs().context("load first-party bundles")?;
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

fn installed_shadow_keys(records: &[BundleRegistryRecord]) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut ids = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    for record in records {
        ids.insert(record.bundle_id.clone());
        if let Ok(bundle) = decode_installed_bundle(record) {
            namespaces.insert(bundle.namespace().to_string());
        }
    }
    (ids, namespaces)
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
    hya_core::builtin_agents()
        .iter()
        .map(|agent| agent.id)
        .collect()
}

async fn list() -> anyhow::Result<()> {
    let first_party = hya_app::first_party_catalogs().context("load first-party bundles")?;
    let installed = installed_records_if_exists().await?;
    let (shadowed_ids, shadowed_namespaces) = installed_shadow_keys(&installed);
    let mut rows = Vec::new();
    for preset in hya_app::trusted_preset_inventory().context("decode trusted presets")? {
        rows.push((
            preset.id,
            preset.version,
            preset.agent_ids.join(","),
            "active".to_string(),
            preset.kind,
            "-".to_string(),
        ));
    }
    for catalog in &first_party {
        for bundle in catalog.bundles() {
            if shadowed_ids.contains(&bundle.identity().id)
                || shadowed_namespaces.contains(bundle.namespace())
            {
                continue;
            }
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

    print_list_rows(rows.iter());
    Ok(())
}

/// Present one prepared bundle as an owned, sortable CLI list row.
fn bundle_list_row(bundle: &PreparedInstallableBundle, state: &str) -> BundleListRow {
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

/// One `bundle list` row: NAME VERSION AGENT STATE KIND WORKFLOW.
type BundleListRow = (String, String, String, String, String, String);

/// One searchable catalog entry: the lowercased metadata a query matches
/// against and the `bundle list` row printed when it matches.
struct SearchEntry {
    haystack: String,
    row: BundleListRow,
}

/// Print the `bundle list` header plus one line per row.
fn print_list_rows<'a>(rows: impl Iterator<Item = &'a BundleListRow>) {
    println!("NAME VERSION AGENT STATE KIND WORKFLOW");
    for (bundle_id, version, agents, state, kind, workflow) in rows {
        println!("{bundle_id} {version} {agents} {state} {kind} {workflow}");
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

/// Search the merged first-party and installed catalog: a case-insensitive
/// substring query over bundle ids, agent ids, and skill ids prints matching
/// bundles as `bundle list` rows. When no metadata matches — a query naming a
/// subcommand like `schemas` rather than bundle metadata — every bundle
/// prints instead and the fallback is explained on stderr.
async fn search(query: &str) -> anyhow::Result<()> {
    let needle = query.trim().to_lowercase();
    anyhow::ensure!(!needle.is_empty(), "bundle search requires a query");
    let first_party = hya_app::first_party_catalogs().context("load first-party bundles")?;
    let installed = installed_records_if_exists().await?;
    let (shadowed_ids, shadowed_namespaces) = installed_shadow_keys(&installed);
    let mut entries = Vec::new();
    for preset in hya_app::trusted_preset_inventory().context("decode trusted presets")? {
        let mut haystack = preset.id.to_lowercase();
        for id in preset.agent_ids.iter().chain(preset.resource_ids.iter()) {
            haystack.push('\n');
            haystack.push_str(&id.to_lowercase());
        }
        entries.push(SearchEntry {
            haystack,
            row: (
                preset.id,
                preset.version,
                preset.agent_ids.join(","),
                "active".to_string(),
                preset.kind,
                "-".to_string(),
            ),
        });
    }
    for catalog in &first_party {
        for bundle in catalog.bundles() {
            if shadowed_ids.contains(&bundle.identity().id)
                || shadowed_namespaces.contains(bundle.namespace())
            {
                continue;
            }
            entries.push(SearchEntry {
                haystack: bundle_search_haystack(bundle),
                row: bundle_list_row(bundle, "active"),
            });
        }
    }
    for record in &installed {
        match decode_installed_bundle(record) {
            Ok(bundle) => entries.push(SearchEntry {
                haystack: bundle_search_haystack(&bundle),
                row: bundle_list_row(&bundle, "active"),
            }),
            // Written by a different binary version: the bundle id is the
            // only searchable metadata left, and the degraded row matches
            // what `bundle list` prints for the same record.
            Err(_) => entries.push(SearchEntry {
                haystack: record.bundle_id.to_lowercase(),
                row: (
                    record.bundle_id.clone(),
                    record.version.clone(),
                    "-".to_string(),
                    "unreadable (reinstall)".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                ),
            }),
        }
    }
    entries.sort_by(|left, right| left.row.0.as_bytes().cmp(right.row.0.as_bytes()));

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

async fn info(bundle_id: &str) -> anyhow::Result<()> {
    if let Some(preset) = hya_app::trusted_preset_inventory()
        .context("decode trusted presets")?
        .into_iter()
        .find(|preset| preset.id == bundle_id)
    {
        println!("name={}", preset.id);
        println!("version={}", preset.version);
        println!("origin=preset");
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
    if let Some(record) = installed_records_if_exists()
        .await?
        .into_iter()
        .find(|record| record.bundle_id == bundle_id)
    {
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
    Err(StoreError::BundleNotFound {
        bundle_id: bundle_id.to_string(),
    }
    .into())
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
        return Ok(());
    }
    Err(StoreError::BundleNotFound {
        bundle_id: bundle_id.to_string(),
    }
    .into())
}

async fn uninstall(bundle_id: &str) -> anyhow::Result<()> {
    let has_installed_override = installed_records_if_exists()
        .await?
        .iter()
        .any(|record| record.bundle_id == bundle_id);
    anyhow::ensure!(
        !hya_app::trusted_preset_inventory()
            .context("decode trusted presets")?
            .iter()
            .any(|preset| preset.id == bundle_id),
        "immutable trusted preset `{bundle_id}` cannot be uninstalled"
    );
    anyhow::ensure!(
        has_installed_override
            || !hya_app::first_party_catalogs()
                .context("load first-party bundles")?
                .iter()
                .any(|catalog| catalog
                    .bundles()
                    .iter()
                    .any(|bundle| bundle.identity().id == bundle_id)),
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
    let first_party = hya_app::first_party_catalogs().context("load first-party bundles")?;
    let installed = installed_records_if_exists().await?;
    let (shadowed_ids, shadowed_namespaces) = installed_shadow_keys(&installed);
    let mut rows = Vec::new();
    for catalog in &first_party {
        let shadowed = catalog.bundles().first().is_some_and(|bundle| {
            shadowed_ids.contains(&bundle.identity().id)
                || shadowed_namespaces.contains(bundle.namespace())
        });
        if !shadowed {
            rows.extend(catalog_schema_rows(catalog));
        }
    }
    for record in installed {
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
