use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

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
    let chain = context_chain(workdir);
    let stamps: Vec<_> = chain.iter().map(file_stamp).collect();

    // Serve from cache only when the chain is identical AND every file's
    // (mtime, len) is unchanged. The chain walk is re-run every call — it is
    // cheap `is_file()` probes — because a NEWLY added AGENTS.md higher up must
    // invalidate, and validating only previously-seen files would miss it.
    if let Some(cache) = context_cache().lock().ok()
        && let Some(entry) = cache.get(&chain)
        && entry.stamps == stamps
    {
        return entry.files.clone();
    }

    let mut files = Vec::new();
    for path in &chain {
        if let Ok(content) = std::fs::read_to_string(path) {
            CONTEXT_FILE_READS.fetch_add(1, Ordering::Relaxed);
            files.push((path.to_string_lossy().into_owned(), content));
        }
    }
    if let Ok(mut cache) = context_cache().lock() {
        // Unbounded growth is not a concern: keys are workdir chains, of which a
        // process sees a handful. Clear rather than evict if that ever changes.
        cache.insert(
            chain,
            CachedChain {
                stamps,
                files: files.clone(),
            },
        );
    }
    files
}

/// `AGENTS.md` paths from `workdir` up to `$HOME`, parent-first.
fn context_chain(workdir: &Path) -> Vec<PathBuf> {
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
    chain
}

/// Change-detection stamp for one context file: modified time and length.
///
/// Length is included because coarse mtime granularity can hide a same-second
/// rewrite.
fn file_stamp(path: &PathBuf) -> (Option<SystemTime>, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    }
}

struct CachedChain {
    stamps: Vec<(Option<SystemTime>, u64)>,
    files: Vec<(String, String)>,
}

fn context_cache() -> &'static Mutex<HashMap<Vec<PathBuf>, CachedChain>> {
    static CACHE: OnceLock<Mutex<HashMap<Vec<PathBuf>, CachedChain>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

static CONTEXT_FILE_READS: AtomicU64 = AtomicU64::new(0);

/// Total `AGENTS.md` reads performed by [`discover_context_files`] this process.
///
/// Exposed so tests can prove the cache actually avoids filesystem reads; a
/// content-equality assertion alone cannot distinguish a cache from a re-read.
#[must_use]
pub fn context_file_reads() -> u64 {
    CONTEXT_FILE_READS.load(Ordering::Relaxed)
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

    /// Serializes every test that calls `discover_context_files`.
    ///
    /// `context_file_reads()` is a process-global counter, so a concurrent test
    /// doing its own discovery would inflate the delta and make the cache-hit
    /// assertion pass or fail by luck.
    fn discovery_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
    fn repeat_discovery_does_not_re_read_unchanged_files() {
        let _guard = discovery_guard();
        let root = tempdir("cache-reads");
        assert!(std::fs::write(root.join("AGENTS.md"), "READ_COUNT_BODY").is_ok());

        let _ = discover_context_files(&root);
        let after_first = context_file_reads();
        let _ = discover_context_files(&root);
        let after_second = context_file_reads();

        assert_eq!(
            after_first, after_second,
            "an unchanged chain must be served from cache without re-reading"
        );
    }

    #[test]
    fn cached_discovery_returns_the_same_content_on_repeat() {
        let _guard = discovery_guard();
        let root = tempdir("cache-hit");
        let file = root.join("AGENTS.md");
        assert!(std::fs::write(&file, "CACHE_BODY_ONE").is_ok());

        let first = discover_context_files(&root);
        let second = discover_context_files(&root);
        assert_eq!(first, second, "a repeat walk must return identical content");
        assert!(
            first
                .iter()
                .any(|(_, body)| body.contains("CACHE_BODY_ONE")),
            "fixture body must be discovered: {first:?}"
        );
    }

    #[test]
    fn editing_a_context_file_invalidates_the_cache() {
        let _guard = discovery_guard();
        let root = tempdir("cache-edit");
        let file = root.join("AGENTS.md");
        assert!(std::fs::write(&file, "BEFORE_EDIT_BODY").is_ok());
        let before = discover_context_files(&root);
        assert!(before.iter().any(|(_, b)| b.contains("BEFORE_EDIT_BODY")));

        // Rewrite with a different length so mtime-granularity cannot mask it.
        assert!(std::fs::write(&file, "AFTER_EDIT_BODY_THAT_IS_LONGER").is_ok());
        let after = discover_context_files(&root);
        assert!(
            after.iter().any(|(_, b)| b.contains("AFTER_EDIT_BODY")),
            "an edited AGENTS.md must not serve stale cached content: {after:?}"
        );
        assert!(
            !after.iter().any(|(_, b)| b.contains("BEFORE_EDIT_BODY")),
            "stale body must be gone: {after:?}"
        );
    }

    #[test]
    fn adding_a_context_file_to_the_chain_invalidates_the_cache() {
        let _guard = discovery_guard();
        let root = tempdir("cache-add");
        let child = root.join("nested");
        assert!(std::fs::create_dir_all(&child).is_ok());
        assert!(std::fs::write(child.join("AGENTS.md"), "CHILD_ONLY_BODY").is_ok());
        let before = discover_context_files(&child);
        let before_count = before.len();

        // A NEW file appearing higher in the chain must be picked up: validating
        // only the previously-seen files would miss this.
        assert!(std::fs::write(root.join("AGENTS.md"), "NEWLY_ADDED_PARENT").is_ok());
        let after = discover_context_files(&child);
        assert!(
            after.iter().any(|(_, b)| b.contains("NEWLY_ADDED_PARENT")),
            "a newly added AGENTS.md must invalidate the cache: {after:?}"
        );
        assert_eq!(
            after.len(),
            before_count + 1,
            "exactly one new entry expected: {after:?}"
        );
    }

    #[test]
    fn discover_context_files_parent_before_child_with_project_separators() {
        let _guard = discovery_guard();
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
