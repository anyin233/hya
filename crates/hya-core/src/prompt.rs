use std::path::{Path, PathBuf};

/// Process-local environment facts rendered into the Harness prompt layer.
pub struct PromptEnv {
    /// Absolute or display cwd string.
    pub cwd: String,
    /// OS/platform label for the Environment block.
    pub platform: String,
    /// Calendar date string (`YYYY-MM-DD`).
    pub date: String,
}

/// UTC calendar date `YYYY-MM-DD` for Environment prompt material.
#[must_use]
pub fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}

/// Walk from `workdir` toward filesystem root (stopping at `$HOME`) and collect
/// every `AGENTS.md`, parent-first.
///
/// Each entry is `(absolute_or_display_path, file_contents)`. Missing or
/// unreadable files are skipped. This is the sole discovery implementation;
/// callers re-export rather than reimplement walk order.
#[must_use]
pub fn discover_context_files(workdir: &Path) -> Vec<(String, String)> {
    let start = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        let candidate = d.join("AGENTS.md");
        if candidate.is_file() {
            chain.push(candidate);
        }
        if home.as_deref() == Some(d) {
            break;
        }
        dir = d.parent();
    }
    chain.reverse();
    let mut files = Vec::new();
    for path in chain {
        if let Ok(content) = std::fs::read_to_string(&path) {
            files.push((path.to_string_lossy().into_owned(), content));
        }
    }
    files
}

/// Render Environment + project-context sections without an agent base.
///
/// Separators match historical Harness composition (`## Environment`, then
/// `## Project context: {name}` per discovered file).
#[must_use]
pub fn render_environment_and_context(
    env: &PromptEnv,
    context_files: &[(String, String)],
) -> String {
    let mut out = format!(
        "## Environment\n- cwd: {}\n- platform: {}\n- date: {}\n",
        env.cwd, env.platform, env.date
    );
    for (name, content) in context_files {
        out.push_str("\n## Project context: ");
        out.push_str(name);
        out.push('\n');
        out.push_str(content.trim());
        out.push('\n');
    }
    out
}

/// Compose agent base + Environment + discovered project context files.
#[must_use]
pub fn build_system_prompt(
    base: &str,
    env: &PromptEnv,
    context_files: &[(String, String)],
) -> String {
    let layer = render_environment_and_context(env, context_files);
    let base = base.trim();
    if base.is_empty() {
        layer
    } else {
        format!("{base}\n\n{layer}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PromptEnv {
        PromptEnv {
            cwd: "/work/proj".to_string(),
            platform: "linux".to_string(),
            date: "2026-06-21".to_string(),
        }
    }

    fn tempdir(label: &str) -> PathBuf {
        let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        else {
            panic!("system clock before UNIX_EPOCH while creating tempdir for {label}");
        };
        let nanos = duration.as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hya-core-prompt-{label}-{nanos}-{}",
            std::process::id()
        ));
        assert!(
            std::fs::create_dir_all(&dir).is_ok(),
            "failed to create tempdir for {label}: {}",
            dir.display()
        );
        std::fs::canonicalize(&dir).unwrap_or(dir)
    }

    #[test]
    fn includes_base_env_and_context() {
        let ctx = vec![("AGENTS.md".to_string(), "Always use tabs.".to_string())];
        let out = build_system_prompt("You are hya.", &env(), &ctx);
        assert!(out.contains("You are hya."));
        assert!(out.contains("/work/proj"));
        assert!(out.contains("linux"));
        assert!(out.contains("2026-06-21"));
        assert!(out.contains("## Project context: AGENTS.md"));
        assert!(out.contains("Always use tabs."));
    }

    #[test]
    fn no_context_section_when_empty() {
        let out = build_system_prompt("Base.", &env(), &[]);
        assert!(out.contains("Base."));
        assert!(out.contains("/work/proj"));
        assert!(!out.contains("Project context"));
    }

    #[test]
    fn render_environment_and_context_omits_agent_base() {
        let ctx = vec![("AGENTS.md".to_string(), "Prefer spaces.".to_string())];
        let out = render_environment_and_context(&env(), &ctx);
        assert!(out.starts_with("## Environment\n"));
        assert!(out.contains("## Project context: AGENTS.md"));
        assert!(out.contains("Prefer spaces."));
        assert!(!out.contains("You are hya"));
    }

    #[test]
    fn discover_context_files_parent_before_child_with_project_separators() {
        // No process-global HOME mutation. Unrelated ancestor AGENTS.md may appear;
        // assert only the relative order of the two fixture entries.
        let root = tempdir("discover");
        let parent = root.join("proj");
        let child = parent.join("nested");
        assert!(
            std::fs::create_dir_all(&child).is_ok(),
            "failed to create nested fixture dir: {}",
            child.display()
        );
        assert!(
            std::fs::write(parent.join("AGENTS.md"), "PARENT_AGENTS_BODY").is_ok(),
            "failed to write parent fixture AGENTS.md under: {}",
            parent.display()
        );
        assert!(
            std::fs::write(child.join("AGENTS.md"), "CHILD_AGENTS_BODY").is_ok(),
            "failed to write child fixture AGENTS.md under: {}",
            child.display()
        );

        let files = discover_context_files(&child);
        let rendered = render_environment_and_context(&env(), &files);

        let Some(parent_idx) = files
            .iter()
            .position(|(_, body)| body.contains("PARENT_AGENTS_BODY"))
        else {
            panic!("parent fixture AGENTS.md must be discovered: {files:?}");
        };
        let Some(child_idx) = files
            .iter()
            .position(|(_, body)| body.contains("CHILD_AGENTS_BODY"))
        else {
            panic!("child fixture AGENTS.md must be discovered: {files:?}");
        };
        assert!(
            parent_idx < child_idx,
            "parent fixture before child among discovered files: {files:?}"
        );
        let Some(parent_pos) = rendered.find("PARENT_AGENTS_BODY") else {
            panic!("parent body in rendered guidance: {rendered}");
        };
        let Some(child_pos) = rendered.find("CHILD_AGENTS_BODY") else {
            panic!("child body in rendered guidance: {rendered}");
        };
        assert!(
            parent_pos < child_pos,
            "parent project context before child: {rendered}"
        );
        assert!(
            rendered.matches("## Project context:").count() >= 2,
            "at least one separator per fixture file: {rendered}"
        );
    }
}
