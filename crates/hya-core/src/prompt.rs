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

/// The shortest correct mental model of the subagent system (ADR-0015/0016),
/// rendered for one request from the coordination tools it actually
/// advertises (`has(name)`), so each agent — built-in or bundle, root or
/// member — is taught exactly the tools the harness allocated to it.
///
/// Written from the observed failure modes of real multi-agent runs: the main
/// agent tried to `report`, agents read `#channel` ids as files, tried to stop
/// already archived agents, and a narrow subagent could not find its mail.
/// Every line exists to prevent one of those. `None` when the request has no
/// coordination tool at all.
#[must_use]
pub fn team_quick_reference(has: impl Fn(&str) -> bool, depth: u32) -> Option<String> {
    let mail = has("list_channel");
    let mut lines: Vec<&str> = Vec::new();
    if has("task") {
        lines.push(
            "- `task` is non-blocking: it returns the child's handle immediately. Results arrive later as mail — watch for `[NEW MAIL]` notices appended to tool results.",
        );
    }
    if mail {
        lines.push(
            "- New mail arrives automatically appended to tool results (`[NEW MAIL]`) — do NOT poll `list_channel` or sleep waiting for mail; children's live status is in `list_channel`'s team section (busy + last-heartbeat age).",
        );
    }
    if has("wait") && has("task") {
        lines.push(
            "- To block until your subagents finish (they call `report` or are archived; going idle is not finishing), call `wait` once — optionally naming targets, any/all, and a timeout; with the channel tools loaded it also returns when new mail arrives for you (each message once). If it reports a subagent stalled (its turn ended without a report), mail it to continue or `archive` it — waiting again will not restart it. It is the only correct way to wait: never loop on status tools.",
        );
    } else if has("wait") && mail {
        lines.push(
            "- To block until new mail arrives for you, call `wait` once (optionally with a timeout). It is the only correct way to wait: never loop on status tools.",
        );
    }
    if depth == 0 {
        if !lines.is_empty() || has("send") || has("archive") {
            lines.push(
                "- `report` is ONLY for subagents to end their own task. As the main agent NEVER call `report` — deliver your final answer as normal text.",
            );
        }
    } else if has("report") {
        lines.push(if has("archive") {
            "- Finish your task with `report` (once, with your result for your parent). It is rejected while you have unread mail — read the channel the error names, answer if needed, then report again — or live subagents (`archive` them first)."
        } else {
            "- Finish your task with `report` (once, with your result for your parent). It is rejected while you have unread mail — read the channel the error names, answer if needed, then report again."
        });
    }
    if mail && has("read") {
        lines.push(
            "- Read mail history with `read channel://<id>` (latest) or `channel://<id>?last=N`; `list_channel` shows channels + unread counts. A `#id` is never a file path.",
        );
    }
    if has("send") {
        lines.push(
            "- `send` covers all mail: `#channel` posts on that channel (a group channel broadcasts to your unit — leader-only); a bare handle DMs that vertical peer (`^parent` DMs your parent through your registration DM channel; an archived child revives with its saved state). Omit the channel to use your default: the unit you lead, else your parent.",
        );
    }
    if has("archive") {
        lines.push(if has("search_agent") {
            "- `archive` stops a LIVE subagent you no longer need (by handle or session id) and archives it. Archiving is not deletion: an archived agent keeps its handle and session, and `send` to its handle wakes it again. Already-archived agents are found with `search_agent`."
        } else {
            "- `archive` stops a LIVE subagent you no longer need (by handle or session id) and archives it. Archiving is not deletion: an archived agent keeps its handle and session, and `send` to its handle wakes it again."
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("## Team quick reference\n{}", lines.join("\n")))
}

/// Compose agent base + Environment + discovered project context files.
///
/// The team quick reference is not part of this static prompt: it is rendered
/// per request from the tools that request advertises
/// ([`team_quick_reference`]).
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
    } else if layer.is_empty() {
        base.to_string()
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

    const FULL_TOOLS: [&str; 10] = [
        "task",
        "wait",
        "report",
        "archive",
        "search_agent",
        "send",
        "list_channel",
        "read",
        "grep",
        "bash",
    ];

    fn reference(tools: &[&str], depth: u32) -> String {
        team_quick_reference(|name| tools.contains(&name), depth).unwrap_or_default()
    }

    /// The team reference is rendered per request from the tools that request
    /// advertises; the static agent prompt no longer carries it.
    #[test]
    fn build_system_prompt_no_longer_carries_the_team_reference() {
        let out = build_system_prompt("You are hya.", &env(), &[]);
        assert!(!out.contains("Team quick reference"), "{out}");
    }

    /// The report prohibition is main-only: the depth-0 reference carries it,
    /// no subagent reference does, and no builtin subagent prompt may contain
    /// the prohibition wording (subagents MUST report freely).
    #[test]
    fn report_prohibition_is_main_only() {
        let main = reference(&FULL_TOOLS, 0);
        assert!(
            main.contains("As the main agent NEVER call `report`"),
            "the main reference keeps the prohibition line: {main}"
        );
        let member = reference(&FULL_TOOLS, 1);
        assert!(!member.contains("NEVER call `report`"), "{member}");
        assert!(
            member.contains("Finish your task with `report`"),
            "{member}"
        );
        for agent in crate::builtin_agents::builtin_agents() {
            let Some(prompt) = agent.prompt else { continue };
            assert!(
                !prompt.contains("NEVER call `report`"),
                "subagent prompt `{}` must not forbid report",
                agent.id
            );
        }
    }

    /// Every tool-shaped backtick token in the team quick reference must
    /// resolve in the CURRENT builtin registry (canonical or hidden alias).
    /// Non-tool spellings (channel URLs, sentinels, notices) are exempt.
    #[test]
    fn quick_reference_tool_tokens_resolve_in_the_registry() {
        let registry = hya_tool::ToolRegistry::builtins();
        for depth in [0, 1] {
            let out = reference(&FULL_TOOLS, depth);
            assert!(out.starts_with("## Team quick reference"), "{out}");
            for token in out.split('`').skip(1).step_by(2) {
                let token = token.trim();
                let is_non_tool_spelling = token.is_empty()
                    || token.contains("://")
                    || token.starts_with('#')
                    || token.starts_with('[')
                    || token.starts_with('^');
                if is_non_tool_spelling {
                    continue;
                }
                assert!(
                    registry.get(token).is_some(),
                    "quick reference names `{token}` but no such tool exists"
                );
            }
        }
    }

    /// A line only appears when the request advertises the tools it teaches.
    #[test]
    fn quick_reference_lists_only_the_tools_the_agent_has() {
        let member = reference(&["report", "wait", "send", "list_channel", "read"], 1);
        for present in [
            "`report`",
            "`wait`",
            "`send`",
            "`list_channel`",
            "channel://",
        ] {
            assert!(member.contains(present), "missing {present}: {member}");
        }
        for absent in ["`task`", "`archive`", "`search_agent`"] {
            assert!(!member.contains(absent), "leaked {absent}: {member}");
        }
        let quiet = reference(&["wait", "grep"], 1);
        assert!(
            !quiet.contains("`send`") && !quiet.contains("channel://"),
            "{quiet}"
        );
        assert_eq!(
            team_quick_reference(|name| name == "grep", 1),
            None,
            "no coordination tool, no reference"
        );
    }

    #[test]
    fn team_reference_teaches_wait_instead_of_polling() {
        let out = reference(&FULL_TOOLS, 0);
        assert!(out.contains("call `wait` once"), "{out}");
    }

    #[test]
    fn team_reference_forbids_polling_for_mail() {
        let out = reference(&FULL_TOOLS, 0);
        assert!(
            out.contains("do NOT poll `list_channel`"),
            "the quick reference must forbid polling for mail: {out}"
        );
        assert!(
            out.contains("[NEW MAIL]"),
            "the reference must say mail arrives via [NEW MAIL] notices"
        );
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
