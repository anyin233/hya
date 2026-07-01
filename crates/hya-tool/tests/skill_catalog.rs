#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct HomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }
}

fn tempdir() -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-tool-skill-catalog-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

#[test]
fn skill_dirs_start_with_hya_project_then_hya_config_then_external_fallbacks() {
    let home = tempdir();
    let workdir = tempdir();
    let _home = HomeGuard::set(&home);

    let dirs = hya_tool::skill_dirs_for_workdir(&workdir);

    assert_eq!(dirs[0], workdir.join(".hya/skills"));
    assert_eq!(dirs[1], home.join(".config/hya/skills"));
    assert_eq!(dirs[2], home.join(".claude/skills"));
    assert_eq!(dirs[3], home.join(".config/opencode/skills"));
    assert_eq!(dirs[4], home.join(".config/opencode/skill"));
    assert_eq!(dirs[5], workdir.join(".opencode/skills"));
    assert_eq!(dirs[6], workdir.join(".opencode/skill"));
    assert_eq!(dirs[7], workdir.join(".agents/skills"));
    assert_eq!(dirs[8], home.join(".codex/skills"));
    assert_eq!(dirs[9], home.join(".agents/skills"));
}

#[test]
fn discover_skills_preserves_root_order_and_first_name_wins() {
    let home = tempdir();
    let workdir = tempdir();
    let _home = HomeGuard::set(&home);
    write_skill(
        &workdir.join(".hya/skills/z-local"),
        "shared",
        "Local shared",
        "local body",
    );
    write_skill(
        &workdir.join(".hya/skills/a-local"),
        "local-only",
        "Local only",
        "local only body",
    );
    write_skill(
        &home.join(".config/hya/skills/shared"),
        "shared",
        "Home shared",
        "home body",
    );
    write_skill(
        &home.join(".config/hya/skills/b-home"),
        "home-only",
        "Home only",
        "home body",
    );

    let skills = hya_tool::discover_skills(&workdir);

    let names = skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["local-only", "shared", "home-only"]);
    let shared = skills.iter().find(|skill| skill.name == "shared").unwrap();
    assert_eq!(shared.description, "Local shared");
    assert_eq!(shared.content, "local body");
}
