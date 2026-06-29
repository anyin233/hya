#![allow(clippy::expect_used)]

mod common;

use std::process::Command;

use common::{tempdir, write_skill};

#[test]
fn sync_opencode_apply_creates_symlink_and_lockfile() {
    let root = tempdir("sync-opencode-apply");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );

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

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );

    let migrated_skill = hya_skill_root.join("plan-review");
    assert!(migrated_skill.exists(), "migrated skill missing");
    assert!(
        std::fs::symlink_metadata(&migrated_skill)
            .expect("skill metadata")
            .file_type()
            .is_symlink(),
        "migrated skill should be a symlink"
    );

    let lockfile = hya_config_home.join("hya/opencode-sync-lock.json");
    assert!(lockfile.exists(), "lockfile missing");

    let config_text = std::fs::read_to_string(&hya_config).expect("read migrated hya config");
    assert!(
        config_text.contains("codegraph"),
        "config was: {config_text}"
    );
}

#[test]
fn sync_opencode_apply_is_idempotent_on_second_run() {
    let root = tempdir("sync-opencode-idempotent");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );

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

    let args = [
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

    let first = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .output()
        .expect("run xtask first time");
    assert!(
        first.status.success(),
        "first run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let first_config = std::fs::read_to_string(&hya_config).expect("read first config");
    let first_lock = std::fs::read_to_string(hya_config_home.join("hya/opencode-sync-lock.json"))
        .expect("read first lock");

    let second = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .output()
        .expect("run xtask second time");
    assert!(
        second.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let second_config = std::fs::read_to_string(&hya_config).expect("read second config");
    let second_lock = std::fs::read_to_string(hya_config_home.join("hya/opencode-sync-lock.json"))
        .expect("read second lock");

    assert_eq!(first_config, second_config, "config changed on rerun");
    assert_eq!(first_lock, second_lock, "lockfile changed on rerun");
}

#[test]
fn sync_opencode_apply_preserves_unmanaged_mcp_entries() {
    let root = tempdir("sync-opencode-preserve-mcp");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );

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
    std::fs::write(
        &hya_config,
        concat!(
            "default_model: offline\n",
            "mcp:\n",
            "  manual_server:\n",
            "    command:\n",
            "      - python3\n",
            "      - keep.py\n",
            "    enabled: true\n"
        ),
    )
    .expect("write hya config");

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );

    let config_text = std::fs::read_to_string(&hya_config).expect("read migrated hya config");
    assert!(
        config_text.contains("manual_server"),
        "unmanaged mcp entry should remain: {config_text}"
    );
    assert!(
        config_text.contains("codegraph"),
        "managed mcp entry should be added: {config_text}"
    );
}

#[test]
fn sync_opencode_apply_does_not_duplicate_existing_same_named_mcp() {
    let root = tempdir("sync-opencode-existing-mcp");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(
        &opencode_skill_root,
        "plan-review",
        "repo-local plan review",
    );

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
    std::fs::write(
        &hya_config,
        concat!(
            "default_model: offline\n",
            "mcp:\n",
            "  codegraph:\n",
            "    command:\n",
            "      - codegraph\n",
            "      - serve\n",
            "      - --mcp\n",
            "    enabled: true\n"
        ),
    )
    .expect("write hya config");

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );

    let config_text = std::fs::read_to_string(&hya_config).expect("read migrated hya config");
    let occurrences = config_text.matches("codegraph:").count();
    assert_eq!(occurrences, 1, "config duplicated codegraph: {config_text}");
}

#[test]
fn sync_opencode_apply_skips_user_authored_skill_directory() {
    let root = tempdir("sync-opencode-skill-conflict");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");
    write_skill(&opencode_skill_root, "plan-review", "incoming skill");

    let user_skill = hya_skill_root.join("plan-review");
    std::fs::create_dir_all(&user_skill).expect("create user skill dir");
    let user_marker = user_skill.join("SKILL.md");
    std::fs::write(
        &user_marker,
        "---\nname: plan-review\ndescription: mine\n---\n",
    )
    .expect("write user skill");

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

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );
    assert!(
        stderr.contains("skill conflict"),
        "expected a conflict report, stderr was: {stderr}"
    );

    assert!(
        !std::fs::symlink_metadata(&user_skill)
            .expect("user skill metadata")
            .file_type()
            .is_symlink(),
        "user-authored skill should remain a real directory"
    );
    assert_eq!(
        std::fs::read_to_string(&user_marker).expect("read user marker"),
        "---\nname: plan-review\ndescription: mine\n---\n",
        "user-authored skill content should be untouched"
    );

    let lockfile = hya_config_home.join("hya/opencode-sync-lock.json");
    let lock_text = std::fs::read_to_string(&lockfile).expect("read lockfile");
    assert!(
        !lock_text.contains("plan-review"),
        "skipped skill should not be recorded as managed: {lock_text}"
    );
}

#[test]
fn sync_opencode_apply_migrates_mcp_environment() {
    let root = tempdir("sync-opencode-mcp-env");
    let opencode_skill_root = root.join("opencode-skills");
    let hya_config_home = root.join("xdg");
    let hya_skill_root = hya_config_home.join("hya/skills");
    let opencode_config = root.join("opencode.json");
    let hya_config = hya_config_home.join("hya/config.yaml");

    std::fs::create_dir_all(&opencode_skill_root).expect("create opencode skill root");
    std::fs::create_dir_all(hya_config.parent().expect("hya config parent"))
        .expect("create hya config parent");

    std::fs::write(
        &opencode_config,
        concat!(
            "{\n",
            "  \"mcp\": {\n",
            "    \"codegraph\": {\n",
            "      \"type\": \"local\",\n",
            "      \"command\": [\"codegraph\", \"serve\", \"--mcp\"],\n",
            "      \"environment\": { \"CODEGRAPH_TOKEN\": \"{env:CG_TOKEN}\" },\n",
            "      \"enabled\": true\n",
            "    }\n",
            "  }\n",
            "}\n"
        ),
    )
    .expect("write opencode config");
    std::fs::write(&hya_config, "default_model: offline\n").expect("write hya config");

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "sync-opencode",
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );

    let config_text = std::fs::read_to_string(&hya_config).expect("read migrated hya config");
    assert!(
        config_text.contains("env:") && config_text.contains("CODEGRAPH_TOKEN: \"{env:CG_TOKEN}\""),
        "migrated config should preserve mcp env templates as quoted scalars: {config_text}"
    );
}
