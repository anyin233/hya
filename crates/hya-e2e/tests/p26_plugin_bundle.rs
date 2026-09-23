//! Agentless Plugin bundle installation and static-skill refresh.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t2_21_plugin_installs_and_publishes_static_skill() {
    let package_dir = std::env::temp_dir().join(format!("hya-plugin-e2e-{}", std::process::id()));
    fs::create_dir_all(&package_dir).expect("package dir");
    let source = BundleSource::new(
        "plugin-e2e",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/plugin-e2e, version: 1.0.0, publisher: acme }
resources:
  skills:
    - id: plugin-help
      path: resources/skills/plugin-help.md
"#,
            ),
            SourceFile::new(
                "resources/skills/plugin-help.md",
                b"---\nname: plugin-help\ndescription: Static Plugin skill for process E2E\n---\n# Plugin Help\nPLUGIN_STATIC_SKILL_MARKER\n",
            ),
        ],
    );
    let package = package_dir.join("plugin-e2e.hyabundle");
    fs::write(&package, write_public_package(&source).expect("package")).expect("write package");

    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("skill", json!({"name": "plugin-help"})),
            text_step("PLUGIN_SKILL_LOADED"),
            tool_step("skill", json!({"name": "plugin-help"})),
            text_step("PLUGIN_SKILL_REMOVED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .expect("install");
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let session = env.create_session().await.expect("session");
    env.prompt(session, "load plugin-help")
        .await
        .expect("prompt");
    let requests = env.fake.requests().expect("requests");
    let follow_up = fake_requests_from(&requests, 1);
    assert!(
        follow_up.contains("PLUGIN_STATIC_SKILL_MARKER"),
        "follow-up={follow_up}; {}",
        env.diagnostics()
    );

    let uninstall = env
        .backend
        .bundle_cli(&["bundle", "uninstall", "-y", "acme/plugin-e2e"])
        .expect("uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let removed_session = env.create_session().await.expect("session after uninstall");
    env.prompt(removed_session, "load plugin-help after uninstall")
        .await
        .expect("prompt after uninstall");
    let requests = env.fake.requests().expect("requests after uninstall");
    let removed_follow_up = fake_requests_from(&requests, 3);
    assert!(
        removed_follow_up.contains("skill not found: plugin-help"),
        "removed Plugin skill unexpectedly remained visible: {removed_follow_up}"
    );
    let _ = fs::remove_dir_all(package_dir);
}
