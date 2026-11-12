//! Release installation followed by the daemon lifecycle action it requires.
use anyhow::Context as _;
use hya_updater::release_install::{ReleaseInstallArgs, run_release_install};

pub(crate) async fn run(args: &ReleaseInstallArgs, db: &str) -> anyhow::Result<()> {
    let running = crate::daemon::running(db).await;
    let changes = run_release_install(args).map_err(anyhow::Error::msg)?;
    let Some(found) = running else {
        return Ok(());
    };
    if changes.binary_changed {
        // Run the installed binary: current_exe still refers to the old image.
        let status = tokio::process::Command::new(&changes.executable)
            .args(["--db", db, "serve", "restart"])
            .status()
            .await
            .context("restart the daemon with the installed backend")?;
        anyhow::ensure!(
            status.success(),
            "installed backend could not restart the daemon ({status})"
        );
    } else if changes.bundles_changed {
        reqwest::Client::new()
            .post(format!("{}/v1/bundles:refresh", found.url))
            .json(&serde_json::json!({ "directory": std::env::current_dir()
                .context("read the current directory")?.to_string_lossy() }))
            .send()
            .await
            .context("ask the daemon to refresh installed bundles")?
            .error_for_status()
            .context("daemon rejected the bundle refresh")?;
    }
    Ok(())
}
