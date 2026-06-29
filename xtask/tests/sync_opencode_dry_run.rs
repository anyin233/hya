#![allow(clippy::expect_used)]

mod common;

use std::process::Command;

use common::{tempdir, write_skill};

#[test]
fn sync_opencode_dry_run_reports_mcp_and_skill_actions() {
    let root = tempdir("sync-opencode");
    let opencode_skill_root = root.join("opencode-skills");
    let external_skill_root = root.join("external-skills");
    let hya_skill_root = root.join("hya-skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = root.join("config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(&external_skill_root).expect("create external skill root");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );
    write_skill(
        &external_skill_root,
        "test-driven-development",
        "external tdd skill",
    );

    std::fs::write(
        &opencode_config,
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
    .expect("write opencode config");
    std::fs::write(&hya_config, "default_model: offline\n").expect("write hya config");

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
            "--dry-run",
            "--opencode-config",
            &opencode_config.display().to_string(),
            "--opencode-skill-root",
            &opencode_skill_root.display().to_string(),
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
