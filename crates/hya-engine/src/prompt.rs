use std::path::{Path, PathBuf};

/// Process-local environment facts rendered into the Harness prompt layer.
/// The working directory is the kernel prompt's `<cwd>`.
pub struct PromptEnv {
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
    context_chain(workdir)
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).ok()?;
            Some((path.to_string_lossy().into_owned(), content))
        })
        .collect()
}

/// One `AGENTS.md` that [`discover_context_files`] reads, as frontends list it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFileEntry {
    /// Absolute path.
    pub path: PathBuf,
    /// `project` (in `workdir` itself), `parent` (an ancestor), or `user`
    /// (`$HOME/AGENTS.md`).
    pub scope: &'static str,
    /// File size in bytes.
    pub bytes: u64,
}

/// The files [`discover_context_files`] reads for `workdir`, with their scope
/// and size, parent-first, without reading their contents.
#[must_use]
pub fn context_file_entries(workdir: &Path) -> Vec<ContextFileEntry> {
    let start = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| std::fs::canonicalize(&home).unwrap_or(home));
    context_chain(workdir)
        .into_iter()
        .filter_map(|path| {
            let bytes = std::fs::metadata(&path).ok()?.len();
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let scope = if home.as_deref() == Some(dir.as_path()) {
                "user"
            } else if dir == start {
                "project"
            } else {
                "parent"
            };
            Some(ContextFileEntry { path, scope, bytes })
        })
        .collect()
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
        "## Environment\n- platform: {}\n- date: {}\n",
        env.platform, env.date
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
            "- A `task` result marked running acknowledges launch. Continue independent work while the children run; address follow-up mail to the returned handle for that child. Results arrive later as mail.",
        );
    }
    if mail {
        lines.push(
            "- New mail arrives automatically appended to tool results (`[NEW MAIL]`) — do NOT poll `list_channel` or sleep waiting for mail; children's live status is in `list_channel`'s team section (busy + last-heartbeat age).",
        );
    }
    if has("wait") && has("task") {
        lines.push(
            "- To block until your subagents finish (they report — a final answer that ends their turn counts as a done report — or are archived), call `wait` once — optionally naming targets, any/all, and a timeout; with the channel tools loaded it also returns when new mail arrives for you (each message once). If it reports a subagent stalled (its turn ended with nothing delivered: no answer, a cut-off answer, or unread mail or live subagents of its own), mail it to continue or `archive` it — waiting again will not restart it. A subagent the user stopped is listed `[stopped by the user]`: never try to restart it; tell the user what you need from it. It is the only correct way to wait: never loop on status tools.",
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
            "- Finish your task with `report` (once, with your result for your parent; an accepted report ends your turn). It is rejected while you have unread mail — read the channel the error names, answer if needed, then report again — or live subagents (`archive` them first). Ending your turn with a final answer and no `report` delivers that answer as a done report and ends your episode, so ask your parent questions with `send`, and use `report` to say failed."
        } else {
            "- Finish your task with `report` (once, with your result for your parent; an accepted report ends your turn). It is rejected while you have unread mail — read the channel the error names, answer if needed, then report again. Ending your turn with a final answer and no `report` delivers that answer as a done report and ends your episode, so ask your parent questions with `send`, and use `report` to say failed."
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PromptEnv {
        PromptEnv {
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
        for agent in crate::builtin_agents::preset_agents() {
            let Some(prompt) = agent.prompt.as_deref() else {
                continue;
            };
            assert!(
                !prompt.contains("NEVER call `report`"),
                "subagent prompt `{}` must not forbid report",
                agent.id.as_str()
            );
        }
    }

    /// Every tool-shaped backtick token in the team quick reference must
    /// resolve in the CURRENT builtin registry (canonical or hidden alias).
    /// Non-tool spellings (channel URLs, sentinels, notices) are exempt.
    #[test]
    fn quick_reference_tool_tokens_resolve_in_the_registry() {
        let registry = hya_tool_loader::builtins();
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
    fn team_reference_explains_launch_and_delivery() {
        let out = reference(&FULL_TOOLS, 0);
        assert!(
            out.contains("running acknowledges launch")
                && out.contains("Results arrive later as mail"),
            "{out}"
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
        assert!(out.starts_with("## Environment\n- platform: linux\n- date: 2026-06-21\n"));
        assert!(out.contains("## Project context: AGENTS.md"));
        assert!(out.contains("Prefer spaces."));
        assert!(!out.contains("You are hya"));
    }

    #[test]
    fn context_file_entries_scope_and_size_follow_discovery() {
        let base = std::env::temp_dir().join(format!("hya-ctx-entries-{}", std::process::id()));
        let child = base.join("child");
        assert!(std::fs::create_dir_all(&child).is_ok());
        assert!(std::fs::write(base.join("AGENTS.md"), "PARENT").is_ok());
        assert!(std::fs::write(child.join("AGENTS.md"), "CHILD!").is_ok());
        let entries = context_file_entries(&child);
        let base = std::fs::canonicalize(&base).unwrap_or(base);
        let parent = entries
            .iter()
            .find(|entry| entry.path == base.join("AGENTS.md"));
        let own = entries
            .iter()
            .find(|entry| entry.path == base.join("child/AGENTS.md"));
        assert_eq!(
            parent.map(|entry| (entry.scope, entry.bytes)),
            Some(("parent", 6))
        );
        assert_eq!(
            own.map(|entry| (entry.scope, entry.bytes)),
            Some(("project", 6))
        );
        // Parent first, like the prompt.
        let order: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
        let at = |path: &Path| order.iter().position(|item| item == path);
        assert!(at(&base.join("AGENTS.md")) < at(&base.join("child/AGENTS.md")));
        let _ = std::fs::remove_dir_all(&base);
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
