#![allow(clippy::expect_used)]

mod common;

use std::process::Command;

use common::{tempdir, write_skill};

#[test]
fn sync_opencode_prune_removes_only_managed_state() {
    let root = tempdir("sync-opencode-prune");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(&hya_skill_root).expect("create hya skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );

    let user_skill = hya_skill_root.join("manual-skill");
    std::fs::create_dir_all(&user_skill).expect("create manual skill");
    std::fs::write(
        user_skill.join("SKILL.md"),
        "---\nname: manual-skill\ndescription: keep me\n---\n",
    )
    .expect("write manual skill");

    std::fs::write(
        &opencode_config,
        concat!(
            "{\n",
            "  \"mcp\": {\n",
            "    \"codegraph\": {\n",
            "      \"type\": \"local\",\n",
            "      \"command\": [\"codegraph\", \"serve\", \"--mcp\"],\n",
            "      \"enabled\": true\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    )
    .expect("write opencode config");
    std::fs::write(&hya_config, "default_model: offline\n").expect("write hya config");

    let apply_args = [
        "sync-opencode",
        "--opencode-config",
        &opencode_config.display().to_string(),
        "--opencode-skill-root",
        &opencode_skill_root.display().to_string(),
        "--hya-config",
        &hya_config.display().to_string(),
        "--hya-skills-root",
        &hya_skill_root.display().to_string(),
    ];

    let applied = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(apply_args)
        .output()
        .expect("run apply");
    assert!(
        applied.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );

    let pruned = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
            "--prune",
            "--hya-config",
            &hya_config.display().to_string(),
            "--hya-skills-root",
            &hya_skill_root.display().to_string(),
        ])
        .output()
        .expect("run prune");

    let stderr = String::from_utf8_lossy(&pruned.stderr);
    assert!(
        pruned.status.success(),
        "expected success, stderr was: {stderr}"
    );
    assert!(user_skill.exists(), "manual skill should remain");
    assert!(
        !hya_skill_root.join("plan-review").exists(),
        "managed skill should be removed"
    );

    let config_text = std::fs::read_to_string(&hya_config).expect("read pruned config");
    assert!(
        !config_text.contains("codegraph"),
        "managed mcp should be removed: {config_text}"
    );
}
