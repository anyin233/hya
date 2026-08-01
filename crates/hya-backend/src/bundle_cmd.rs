use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Subcommand;
use hya_bundle::{
    BundleOrigin, PackageInspection, PreparedBundle, PreparedCatalog, PrivatePackageAuthentication,
    PrivatePackagePayload, cleanup_orphaned_staging, stage_package,
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
    let identity = match &inspection {
        PackageInspection::Public(public) => public
            .prepared
            .bundles()
            .first()
            .map(|bundle| bundle.identity.clone())
            .context("installed public package contains no bundle")?,
        PackageInspection::Private(_) => {
            return Err(StoreError::PrivateActivationUnsupported.into());
        }
    };
    let registry = open_registry().await?;
    let catalog = hya_app::builtin_catalog()?;
    let outcome = registry
        .install_inspection(catalog.bundles(), inspection, hya_proto::now_millis())
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
            println!("format: public-v1");
            println!("name: {}", bundle.identity.id);
            println!("version: {}", bundle.identity.version);
            println!("publisher: {}", bundle.identity.publisher);
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

async fn list() -> anyhow::Result<()> {
    let catalog = hya_app::builtin_catalog()?;
    let installed = installed_records_if_exists().await?;
    let mut rows = catalog
        .bundles()
        .iter()
        .map(|bundle| {
            (
                bundle.identity.id.as_str(),
                bundle.identity.version.as_str(),
                "builtin",
                "builtin-v1",
                true,
            )
        })
        .collect::<Vec<_>>();
    rows.extend(installed.iter().map(|record| {
        (
            record.bundle_id.as_str(),
            record.version.as_str(),
            "installed",
            "public-v1",
            false,
        )
    }));
    rows.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    println!("NAME VERSION ORIGIN FORMAT STATE IMMUTABLE");
    for (bundle_id, version, origin, format, immutable) in rows {
        println!("{bundle_id} {version} {origin} {format} active {immutable}");
    }
    Ok(())
}

async fn info(bundle_id: &str) -> anyhow::Result<()> {
    let catalog = hya_app::builtin_catalog()?;
    if let Some(bundle) = catalog
        .bundles()
        .iter()
        .find(|bundle| bundle.identity.id == bundle_id)
    {
        print_builtin_info(bundle);
        return Ok(());
    }

    let record = installed_records_if_exists()
        .await?
        .into_iter()
        .find(|record| record.bundle_id == bundle_id)
        .ok_or_else(|| StoreError::BundleNotFound {
            bundle_id: bundle_id.to_string(),
        })?;
    let bundle = decode_installed_bundle(&record)?;

    println!("name={}", bundle.identity.id);
    println!("version={}", bundle.identity.version);
    println!("publisher={}", bundle.identity.publisher);
    println!("origin=installed");
    println!("format=public-v1");
    println!("state=active");
    println!("immutable=false");
    println!("source_digest={}", hex_digest(&record.source_digest));
    println!("prepared_digest={}", record.prepared_digest);
    print_static_info(&bundle, "=");
    Ok(())
}

async fn uninstall(bundle_id: &str) -> anyhow::Result<()> {
    let catalog = hya_app::builtin_catalog()?;
    if catalog
        .bundles()
        .iter()
        .any(|bundle| bundle.identity.id == bundle_id)
    {
        return Err(StoreError::BundleImmutable {
            bundle_id: bundle_id.to_string(),
        }
        .into());
    }
    let registry = open_registry().await?;
    let BundleUninstallOutcome::Removed { generation } =
        registry.uninstall(catalog.bundles(), bundle_id).await?;
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

fn print_builtin_info(bundle: &PreparedBundle) {
    println!("name={}", bundle.identity.id);
    println!("version={}", bundle.identity.version);
    println!("publisher={}", bundle.identity.publisher);
    println!("origin=builtin");
    println!("format=builtin-v1");
    println!("state=active");
    println!("immutable=true");
    println!("bundle_digest={}", bundle.digest);
    print_static_info(bundle, "=");
}

fn print_static_info(bundle: &PreparedBundle, separator: &str) {
    for agent in &bundle.agents {
        println!("agent{separator}{}", agent.stable_id);
    }
    for skill in &bundle.skills {
        println!("skill{separator}{}", skill.stable_id);
    }
}

fn decode_installed_bundle(record: &BundleRegistryRecord) -> anyhow::Result<PreparedBundle> {
    let corrupt = || StoreError::BundleRegistryCorrupt {
        bundle_id: record.bundle_id.clone(),
    };
    let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
        .map_err(|_| corrupt())?;
    let [bundle] = prepared.bundles() else {
        return Err(corrupt().into());
    };
    if bundle.origin != BundleOrigin::Installed
        || bundle.immutable
        || bundle.identity.id.as_str() != record.bundle_id.as_str()
        || bundle.identity.version.as_str() != record.version.as_str()
        || bundle.identity.publisher.as_str() != record.publisher.as_str()
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
