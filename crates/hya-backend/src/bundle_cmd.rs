use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Subcommand;
use hya_bundle::{
    BundleCatalog, PackageInspection, PreparedCatalog, PreparedInstallableBundle,
    PrivatePackageAuthentication, PrivatePackagePayload, cleanup_orphaned_staging, stage_package,
};
use hya_store::{
    BundleInstallOutcome, BundleRegistry, BundleRegistryRecord, BundleUninstallOutcome, StoreError,
};

#[derive(Subcommand)]
pub(crate) enum BundleCommand {
    /// Install a bundle package.
    Install { package: PathBuf },
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
}

pub(crate) async fn run(command: BundleCommand) -> anyhow::Result<()> {
    match command {
        BundleCommand::Install { package } => install(package).await,
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
    }
}

async fn install(package: PathBuf) -> anyhow::Result<()> {
    validate_package_path(&package)?;
    let inspection = inspect_package(&package)?;
    if let PackageInspection::Public(public) = &inspection {
        let first_party =
            hya_app::first_party_catalog().context("decode embedded first-party WorkflowBundle")?;
        BundleCatalog::from_verified_catalogs(&[&first_party, &public.prepared])
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
    let registry = open_registry().await?;
    let outcome = registry
        .install_inspection(&reserved_agent_ids(), inspection, hya_proto::now_millis())
        .await?;
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
            print_static_info(bundle, ": ");
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
        hya_app::first_party_catalog().context("decode embedded first-party WorkflowBundle")?;
    let [first_party_bundle] = first_party.bundles() else {
        anyhow::bail!("first-party catalog must contain exactly one bundle")
    };
    let installed = installed_records_if_exists().await?;
    let mut rows = vec![bundle_list_row(first_party_bundle, "active")];
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
    let bundle = decode_installed_bundle(&record)?;

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
    print_static_info(&bundle, "=");
    Ok(())
}

/// Print metadata for the immutable first-party WorkflowBundle.
fn info_first_party() -> anyhow::Result<()> {
    let prepared =
        hya_app::first_party_catalog().context("decode embedded first-party WorkflowBundle")?;
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
    print_static_info(bundle, "=");
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

fn print_static_info(bundle: &PreparedInstallableBundle, separator: &str) {
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
