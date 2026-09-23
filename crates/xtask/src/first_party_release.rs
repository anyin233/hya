//! Stage hya's trusted first-party bundles for an installed layout and a release.
//!
//! Every bundle in [`FIRST_PARTY_BUNDLES`] becomes
//! `<package-root>/bundles/hya-<name>.hyabundle`, the path an installed backend
//! loads. With `--assets`, each package is also copied as a standalone release
//! asset: `hya-<name>-<version>-<target>.hyabundle` for native tool families and
//! `hya-<name>-<version>.hyabundle` for platform-independent bundles.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use hya_bundle::{FIRST_PARTY_BUNDLES, first_party_source_directory, inspect_public_package};

use crate::package_bundle::{package_directory, package_native_library};

/// Inputs for one staging run.
pub(crate) struct StageOptions {
    /// Release version every bundle identity must carry.
    pub(crate) version: String,
    /// Directory holding the built `libhya_<family>_tools` libraries.
    pub(crate) library_dir: PathBuf,
    /// Installed layout root; packages go to `<root>/bundles/`.
    pub(crate) package_root: PathBuf,
    /// Target triple for library suffixes and asset names; `None` means the host.
    pub(crate) target: Option<String>,
    /// Directory for standalone release assets, when publishing.
    pub(crate) assets: Option<PathBuf>,
}

/// One staged first-party package.
pub(crate) struct StagedBundle {
    /// Installed-layout package path.
    pub(crate) installed: PathBuf,
    /// Standalone release asset path, when assets were requested.
    pub(crate) asset: Option<PathBuf>,
}

/// Parse `stage-first-party-bundles` arguments and stage every bundle.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let usage = "usage: cargo xtask stage-first-party-bundles --library-dir <dir> \
                 --package-root <dir> [--version <semver>] [--target <triple> --assets <dir>]";
    let mut version = env!("CARGO_PKG_VERSION").to_string();
    let (mut library_dir, mut package_root, mut target, mut assets) = (None, None, None, None);
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} needs a value\n{usage}"))?;
        match flag.as_str() {
            "--version" => version = value,
            "--library-dir" => library_dir = Some(PathBuf::from(value)),
            "--package-root" => package_root = Some(PathBuf::from(value)),
            "--target" => target = Some(value),
            "--assets" => assets = Some(PathBuf::from(value)),
            _ => bail!("unknown option {flag}\n{usage}"),
        }
    }
    ensure!(
        assets.is_none() || target.is_some(),
        "--assets requires --target so native asset names carry the platform\n{usage}"
    );
    let options = StageOptions {
        version,
        library_dir: library_dir.context(usage)?,
        package_root: package_root.context(usage)?,
        target,
        assets,
    };
    for staged in stage(&options)? {
        println!("{}", staged.installed.display());
        if let Some(asset) = staged.asset {
            println!("{}", asset.display());
        }
    }
    Ok(())
}

/// Package every first-party bundle and verify its identity and version.
///
/// # Errors
/// Fails when a source or built library is missing, packaging fails, or a
/// package does not carry exactly its identity at `options.version`.
pub(crate) fn stage(options: &StageOptions) -> anyhow::Result<Vec<StagedBundle>> {
    let bundles = options.package_root.join("bundles");
    std::fs::create_dir_all(&bundles).with_context(|| format!("create {}", bundles.display()))?;
    if let Some(assets) = &options.assets {
        std::fs::create_dir_all(assets).with_context(|| format!("create {}", assets.display()))?;
    }
    let suffix = library_suffix(options.target.as_deref());
    let mut staged = Vec::with_capacity(FIRST_PARTY_BUNDLES.len());
    for identity in FIRST_PARTY_BUNDLES {
        let name = identity
            .strip_prefix("hya/")
            .with_context(|| format!("first-party identity {identity} lacks hya/"))?;
        let source = first_party_source_directory(identity)
            .with_context(|| format!("missing in-tree source for {identity}"))?;
        let installed = bundles.join(format!("hya-{name}.hyabundle"));
        let native = source.join("exposure.yaml").is_file();
        if native {
            let library = options
                .library_dir
                .join(format!("libhya_{}{suffix}", name.replace('-', "_")));
            ensure!(
                library.is_file(),
                "{identity} needs its built library at {}",
                library.display()
            );
            package_native_library(&source, &library, &installed)?;
        } else {
            package_directory(&source, &installed)?;
        }
        verify_package(&installed, identity, &options.version)?;
        let asset = match (&options.assets, &options.target) {
            (Some(assets), Some(target)) => {
                let file = if native {
                    format!("hya-{name}-{}-{target}.hyabundle", options.version)
                } else {
                    format!("hya-{name}-{}.hyabundle", options.version)
                };
                let asset = assets.join(file);
                std::fs::copy(&installed, &asset).with_context(|| {
                    format!("copy {} to {}", installed.display(), asset.display())
                })?;
                Some(asset)
            }
            _ => None,
        };
        staged.push(StagedBundle { installed, asset });
    }
    Ok(staged)
}

/// Require one package to hold exactly `identity` at `version`.
fn verify_package(package: &Path, identity: &str, version: &str) -> anyhow::Result<()> {
    let bytes = std::fs::read(package).with_context(|| format!("read {}", package.display()))?;
    let catalog = inspect_public_package(&bytes)
        .with_context(|| format!("verify staged package {}", package.display()))?;
    let [bundle] = catalog.bundles() else {
        bail!("{} must contain exactly one bundle", package.display());
    };
    ensure!(
        bundle.identity().id == identity,
        "{} contains {} instead of {identity}",
        package.display(),
        bundle.identity().id
    );
    ensure!(
        bundle.identity().version == version,
        "{identity} is version {} but the release is {version}; update its bundle.yaml",
        bundle.identity().version
    );
    Ok(())
}

/// Dynamic library suffix for `target`, or for the host when `None`.
fn library_suffix(target: Option<&str>) -> &'static str {
    match target {
        None => std::env::consts::DLL_SUFFIX,
        Some(target) if target.contains("apple") => ".dylib",
        Some(target) if target.contains("windows") => ".dll",
        Some(_) => ".so",
    }
}
