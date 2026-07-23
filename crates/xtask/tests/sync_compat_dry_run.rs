#![allow(clippy::expect_used)]

mod common;

use std::process::Command;

use common::{tempdir, write_skill};

#[test]
fn sync_compat_dry_run_reports_mcp_and_skill_actions() {
    let root = tempdir("sync-compat");
    let compat_skill_root = root.join("compat-skills");
    let external_skill_root = root.join("external-skills");
    let hya_skill_root = root.join("hya-skills");
    let compat_config = root.join("opencode.json");
    let hya_config = root.join("config.yaml");

    std::fs::create_dir_all(&compat_skill_root).expect("create compat skill root");
    std::fs::create_dir_all(&external_skill_root).expect("create external skill root");
    write_skill(&compat_skill_root, "plan-review", "repo-local plan review");
    write_skill(
        &external_skill_root,
        "test-driven-development",
        "external tdd skill",
    );

    std::fs::write(
        &compat_config,
        format!(
            concat!(
                "{{\n",
                "  \"skills\": {{ \"paths\": [\"{}\"] }},\n",
                "  \"mcp\": {{\n",
                "    \"codegraph\": {{\n",
                "      \"type\": \"local\",\n",
                "      \"command\": [\"codegraph\", \"serve\", \"--mcp\"],\n",
                "      \"enabled\": true\n",
                "    }}\n",
                "  }}\n",
                "}}\n"
            ),
            external_skill_root.display()
        ),
    )
    .expect("write compat config");
    std::fs::write(&hya_config, "default_model: offline\n").expect("write hya config");

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-compat",
            "--dry-run",
            "--compat-config",
            &compat_config.display().to_string(),
            "--compat-skill-root",
            &compat_skill_root.display().to_string(),
            "--hya-config",
            &hya_config.display().to_string(),
            "--hya-skills-root",
            &hya_skill_root.display().to_string(),
        ])
        .output()
        .expect("run xtask");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );
    assert!(stdout.contains("codegraph"), "stdout was: {stdout}");
    assert!(stdout.contains("plan-review"), "stdout was: {stdout}");
    assert!(
        stdout.contains("test-driven-development"),
        "stdout was: {stdout}"
    );
    assert!(
        !hya_skill_root.join("plan-review").exists(),
        "dry run should not create migrated skill entries"
    );
}
