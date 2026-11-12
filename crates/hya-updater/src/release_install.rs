//! Bare `hya update`: reinstall the running prefix from the published GitHub
//! releases with the same installer `curl … | sh` runs.
//!
//! It trusts HTTPS to the release host and each release's `SHA256SUMS`
//! (docs/install.md).
use clap::Args;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `scripts/hya-install.sh`, embedded so `hya update` runs the exact installer
/// `hya.ed-aisys.com/install.sh` serves. It installs the backend and frontend
/// releases, or one side with `--backend-only`/`--tui-only`.
pub const RELEASE_INSTALLER: &str = include_str!("../../../scripts/hya-install.sh");

/// Options of bare `hya update`.
#[derive(Debug, Default, Args)]
pub struct ReleaseInstallArgs {
    /// Install this release of each selected side instead of the latest one
    /// (`0.43.23` or `v0.43.23`).
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Reinstall a side even when that version is already installed.
    #[arg(long)]
    pub force: bool,
    /// Install prefix (`<prefix>/bin/hya`); defaults to the running hya's prefix.
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,
    /// Update only the backend (`bin/hya`, bundles, Bun adapter).
    #[arg(long, conflicts_with = "tui_only")]
    pub backend_only: bool,
    /// Update only the frontend (Bun, TUI, WebUI).
    #[arg(long)]
    pub tui_only: bool,
}

/// The install prefix of a released `hya` at `executable`: the real (symlink
/// resolved) `<prefix>/bin/hya` whose prefix also holds `lib/hya`.
pub fn install_prefix(executable: &Path) -> Result<PathBuf, String> {
    let real = executable
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", executable.display()))?;
    let bin = real
        .parent()
        .filter(|dir| dir.file_name() == Some("bin".as_ref()));
    match bin.and_then(Path::parent) {
        Some(prefix) if prefix.join("lib/hya").is_dir() => Ok(prefix.to_path_buf()),
        _ => Err(format!(
            "{} is not an installed hya (<prefix>/bin/hya beside <prefix>/lib/hya); \
             pass --prefix to install a release, or rebuild a source checkout",
            real.display()
        )),
    }
}

/// Content changes installed by a successful release installation.
#[derive(Debug)]
pub struct InstalledChanges {
    /// Installed backend path, used to start the new daemon image.
    pub executable: PathBuf,
    /// Whether `bin/hya` contents changed.
    pub binary_changed: bool,
    /// Whether the installed bundle tree changed.
    pub bundles_changed: bool,
}

/// Run the embedded installer and compare installed backend and bundle contents.
pub fn run_release_install(args: &ReleaseInstallArgs) -> Result<InstalledChanges, String> {
    let prefix = match &args.prefix {
        Some(prefix) => prefix.clone(),
        None => {
            let executable = std::env::current_exe()
                .map_err(|error| format!("cannot locate the running hya: {error}"))?;
            install_prefix(&executable)?
        }
    };
    let executable = prefix.join("bin/hya");
    let binary_before = fingerprint(&executable)?;
    let bundles_before = fingerprint(&prefix.join("bundles"))?;
    let mut command = Command::new("sh");
    command.args(["-s", "--", "--prefix"]).arg(&prefix);
    if let Some(version) = &args.version {
        command.args(["--version", version]);
    }
    if args.force {
        command.arg("--force");
    }
    if args.backend_only {
        command.arg("--backend-only");
    }
    if args.tui_only {
        command.arg("--tui-only");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run sh: {error}"))?;
    let written = child
        .stdin
        .take()
        .ok_or_else(|| "sh has no stdin".to_string())
        .and_then(|mut stdin| {
            stdin
                .write_all(RELEASE_INSTALLER.as_bytes())
                .map_err(|error| format!("cannot pass the installer to sh: {error}"))
        });
    let status = child
        .wait()
        .map_err(|error| format!("installer did not finish: {error}"))?;
    written?;
    if status.success() {
        Ok(InstalledChanges {
            binary_changed: binary_before != fingerprint(&executable)?,
            bundles_changed: bundles_before != fingerprint(&prefix.join("bundles"))?,
            executable,
        })
    } else {
        Err(format!("the installer failed ({status})"))
    }
}

// Hash sorted paths and file contents, so additions/removals and replacements count.
fn fingerprint(path: &Path) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    fn visit(path: &Path, hash: &mut Sha256) -> std::io::Result<()> {
        if path.is_dir() {
            let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let name = entry.file_name();
                hash.update(name.as_encoded_bytes().len().to_le_bytes());
                hash.update(name.as_encoded_bytes());
                visit(&entry.path(), hash)?;
            }
        } else {
            let mut file = std::fs::File::open(path)?;
            hash.update(file.metadata()?.len().to_le_bytes());
            let mut buffer = [0; 65536];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    if path.exists() {
        visit(path, &mut hash).map_err(|error| format!("read {}: {error}", path.display()))?;
    }
    Ok(hash.finalize().to_vec())
}

#[cfg(test)]
mod tests {
    use super::install_prefix;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("hya-release-install-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create scratch: {error}"));
            Self(dir.canonicalize().unwrap_or(dir))
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap_or_else(|error| panic!("create dir: {error}"));
            }
            fs::write(&path, "").unwrap_or_else(|error| panic!("write file: {error}"));
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fingerprints_detect_replacements_additions_and_removals() {
        let scratch = Scratch::new("fingerprint");
        let root = scratch.0.join("bundles");
        let missing = super::fingerprint(&root)
            .unwrap_or_else(|error| panic!("fingerprint fixture: {error}"));
        let file = scratch.file("bundles/a.hyabundle");
        fs::write(&file, "first").unwrap_or_else(|error| panic!("fingerprint fixture: {error}"));
        let first = super::fingerprint(&root)
            .unwrap_or_else(|error| panic!("fingerprint fixture: {error}"));
        assert_ne!(missing, first);
        assert_eq!(
            first,
            super::fingerprint(&root)
                .unwrap_or_else(|error| panic!("fingerprint fixture: {error}"))
        );
        fs::write(&file, "other").unwrap_or_else(|error| panic!("fingerprint fixture: {error}"));
        assert_ne!(
            first,
            super::fingerprint(&root)
                .unwrap_or_else(|error| panic!("fingerprint fixture: {error}"))
        );
        fs::remove_file(file).unwrap_or_else(|error| panic!("fingerprint fixture: {error}"));
        assert_eq!(
            missing,
            super::fingerprint(&root)
                .unwrap_or_else(|error| panic!("fingerprint fixture: {error}"))
        );
    }

    #[test]
    fn prefix_is_the_parent_of_bin_holding_lib_hya() {
        let scratch = Scratch::new("installed");
        let exe = scratch.file("prefix/bin/hya");
        fs::create_dir_all(scratch.0.join("prefix/lib/hya")).unwrap_or_default();
        assert_eq!(install_prefix(&exe), Ok(scratch.0.join("prefix")));
    }

    #[test]
    fn a_build_tree_or_bare_bin_is_not_an_install() {
        let scratch = Scratch::new("build");
        let debug = scratch.file("target/debug/hya");
        let Err(error) = install_prefix(&debug) else {
            panic!("target/debug accepted as an install");
        };
        assert!(error.contains("--prefix"), "{error}");
        // `bin/hya` without the frontends beside it (e.g. /usr/bin) is refused.
        let bare = scratch.file("usr/bin/hya");
        assert!(install_prefix(&bare).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_hya_updates_the_prefix_it_points_into() {
        let scratch = Scratch::new("symlink");
        let exe = scratch.file("opt/hya/bin/hya");
        fs::create_dir_all(scratch.0.join("opt/hya/lib/hya")).unwrap_or_default();
        fs::create_dir_all(scratch.0.join("home/bin")).unwrap_or_default();
        let link = scratch.0.join("home/bin/hya");
        std::os::unix::fs::symlink(&exe, &link).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(install_prefix(&link), Ok(scratch.0.join("opt/hya")));
    }
}
