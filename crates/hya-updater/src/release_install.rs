//! Bare `hya update`: reinstall the running prefix from the published GitHub
//! release with the same installer `curl … | sh` runs.
//!
//! This path is separate from the signed-metadata TCB in the rest of this
//! crate: it trusts HTTPS to the release host and the release's `SHA256SUMS`
//! (docs/install.md).
use clap::Args;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `scripts/hya-install.sh`, embedded so `hya update` runs the exact backend
/// installer the release publishes.
pub const RELEASE_INSTALLER: &str = include_str!("../../../scripts/hya-install.sh");
/// Frontend installer embedded in `hya update tui`.
pub const FRONTEND_RELEASE_INSTALLER: &str = include_str!("../../../scripts/hya-tui-install.sh");

/// Options of bare `hya update`.
#[derive(Debug, Default, Args)]
pub struct ReleaseInstallArgs {
    /// Install this release instead of the latest one (`0.43.23` or `v0.43.23`).
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Reinstall even when that version is already installed.
    #[arg(long)]
    pub force: bool,
    /// Install prefix (`<prefix>/bin/hya`); defaults to the running hya's prefix.
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,
}

/// Options of `hya update tui`.
#[derive(Debug, Default, Args)]
pub struct FrontendReleaseInstallArgs {
    /// Install this frontend release instead of the latest one.
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Reinstall even when that frontend version is already installed.
    #[arg(long)]
    pub force: bool,
    /// Install prefix (`<prefix>/lib/hya`); defaults to the running hya's prefix.
    #[arg(long, value_name = "DIR")]
    pub prefix: Option<PathBuf>,
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

/// Run the embedded backend installer with `sh`.
pub fn run_release_install(args: &ReleaseInstallArgs) -> Result<(), String> {
    run_component_install(
        args.version.as_deref(),
        args.force,
        args.prefix.as_deref(),
        RELEASE_INSTALLER,
    )
}

/// Run the embedded frontend installer with `sh`.
pub fn run_frontend_release_install(args: &FrontendReleaseInstallArgs) -> Result<(), String> {
    run_component_install(
        args.version.as_deref(),
        args.force,
        args.prefix.as_deref(),
        FRONTEND_RELEASE_INSTALLER,
    )
}

fn run_component_install(
    version: Option<&str>,
    force: bool,
    requested_prefix: Option<&Path>,
    installer: &str,
) -> Result<(), String> {
    let prefix = match requested_prefix {
        Some(prefix) => prefix.to_path_buf(),
        None => {
            let executable = std::env::current_exe()
                .map_err(|error| format!("cannot locate the running hya: {error}"))?;
            install_prefix(&executable)?
        }
    };
    let mut command = Command::new("sh");
    command.args(["-s", "--", "--prefix"]).arg(&prefix);
    if let Some(version) = version {
        command.args(["--version", version]);
    }
    if force {
        command.arg("--force");
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
                .write_all(installer.as_bytes())
                .map_err(|error| format!("cannot pass the installer to sh: {error}"))
        });
    let status = child
        .wait()
        .map_err(|error| format!("installer did not finish: {error}"))?;
    written?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the installer failed ({status})"))
    }
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
