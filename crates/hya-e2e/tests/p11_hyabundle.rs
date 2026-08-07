//! T2.7–T2.8 — hyabundle install/list/info/uninstall + spawn installed agent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, materialize_public_bundle, text_step};
use hya_proto::Event;

const BUNDLE_ID: &str = "hya/valid-public";
const BUNDLE_AGENT: &str = "valid-public-lead";

#[tokio::test]
async fn t2_7_hyabundle_install_list_info_uninstall() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("bundle-cli-noop")])
        .build()
        .await
        .expect("e2e env");

    let package = materialize_public_bundle(&env.backend.project.join("bundles"))
        .expect("materialize .hyabundle");
    assert!(
        package.is_file(),
        "fixture missing at {}",
        package.display()
    );

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", package.to_str().unwrap()])
        .expect("install");
    assert!(
        install.status.success(),
        "install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
    let install_out = String::from_utf8_lossy(&install.stdout);
    assert!(
        install_out.contains(BUNDLE_ID),
        "install stdout missing id: {install_out}"
    );

    let list = env.backend.bundle_cli(&["bundle", "list"]).expect("list");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains(BUNDLE_ID),
        "list missing installed bundle: {list_out}"
    );

    let info = env
        .backend
        .bundle_cli(&["bundle", "info", BUNDLE_ID])
        .expect("info");
    assert!(info.status.success());
    let info_out = String::from_utf8_lossy(&info.stdout);
    assert!(
        info_out.contains(BUNDLE_AGENT),
        "info must list package agent {BUNDLE_AGENT}: {info_out}"
    );
    assert!(
        info_out.contains("origin=installed"),
        "info missing origin=installed: {info_out}"
    );

    let uninstall = env
        .backend
        .bundle_cli(&["bundle", "uninstall", BUNDLE_ID])
        .expect("uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let final_list = env
        .backend
        .bundle_cli(&["bundle", "list"])
        .expect("final list");
    let final_out = String::from_utf8_lossy(&final_list.stdout);
    assert!(
        !final_out.contains(BUNDLE_ID),
        "bundle still listed after uninstall: {final_out}"
    );
}

#[tokio::test]
async fn t2_8_hyabundle_spawn_installed_package_agent() {
    // Materialize outside the temp backend root so Drop does not race the copy.
    let package_dir = std::env::temp_dir().join(format!(
        "hya-e2e-bundle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let package = materialize_public_bundle(&package_dir).expect("materialize");
    let env = E2eEnvBuilder::new()
        .preinstall_bundle(package)
        .agent(BUNDLE_AGENT)
        .scripts(vec![text_step("BUNDLE_AGENT_OK")])
        .build()
        .await
        .expect("e2e env");
    let _ = std::fs::remove_dir_all(&package_dir);

    let agents = env.list_agents().await.expect("list agents");
    let agents_text = agents.to_string();
    assert!(
        agents_text.contains(BUNDLE_AGENT),
        "roster must include installed package agent {BUNDLE_AGENT}; agents={agents}; {}",
        env.diagnostics()
    );

    let session = env
        .create_session_with_agent(BUNDLE_AGENT)
        .await
        .expect("create package agent session");
    let _ = env
        .prompt(session, "hello from bundle agent")
        .await
        .expect("bundle agent prompt");

    let events = env.events(session, None).await.expect("events");
    let mut text = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(&delta),
            Event::TextReplace { text: t, .. } => text.push_str(&t),
            _ => {}
        }
    }
    assert!(
        text.contains("BUNDLE_AGENT_OK"),
        "package agent session must stream BUNDLE_AGENT_OK; text={text:?}; {}",
        env.diagnostics()
    );
}
