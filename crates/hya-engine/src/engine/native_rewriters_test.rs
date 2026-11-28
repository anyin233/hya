use std::process::Command;
use std::sync::Arc;

use hya_proto::SessionId;
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};

use super::{EventBus, SessionEngine};

#[tokio::test]
async fn missing_native_bundle_library_fails_startup_install()
-> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "HYA_TEST_NATIVE_REWRITER_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let engine = SessionEngine::new(
            SessionStore::connect_memory().await?,
            Arc::new(ProviderRouter::new()),
            crate::test_support::runtime(hya_tool_loader::from_tool_families(&[])),
            PermissionPlane::new(PermissionRules::default()).0,
            EventBus::default(),
        );
        assert!(engine.rewriters.is_empty());
        // The host's startup install path (`hya-app` runtime) for an enabled bundle.
        let Err(error) = hya_tool_loader::load_native_bundles(
            &["hya/compaction"],
            Arc::new(hya_tool_loader::MinimalHostRuntime::empty()),
        ) else {
            panic!("startup must return a bundle load error");
        };
        assert!(error.contains("hya/compaction"), "{error}");
        return Ok(());
    }

    // A fresh process with no adjacent libraries cannot reuse the loader cache.
    let exe = std::env::current_exe()?;
    let dir = exe.with_file_name(format!("missing-rewriter-{}", SessionId::new()));
    std::fs::create_dir(&dir)?;
    let child = dir.join("fixture");
    let result = std::fs::hard_link(exe, &child).and_then(|()| {
        Command::new(child)
            .args([
                "--exact",
                "engine::native_rewriters_test::missing_native_bundle_library_fails_startup_install",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
    });
    std::fs::remove_dir_all(dir)?;
    let output = result?;
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{output:?}"
    );
    Ok(())
}
