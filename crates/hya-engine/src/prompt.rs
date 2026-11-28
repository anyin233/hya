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

    #[test]
    fn team_quick_reference_rules_tool_resolution_and_filtering() {
        let registry = hya_tool_loader::builtins();
        let main = reference(&FULL_TOOLS, 0);
        let member = reference(&FULL_TOOLS, 1);

        // Main-only report prohibition; subagents report freely; guidance content holds.
        assert!(main.contains("As the main agent NEVER call `report`"));
        assert!(
            main.contains("running acknowledges launch")
                && main.contains("Results arrive later as mail")
                && main.contains("call `wait` once")
                && main.contains("do NOT poll `list_channel`")
                && main.contains("[NEW MAIL]")
        );
        assert!(
            !member.contains("NEVER call `report`")
                && member.contains("Finish your task with `report`")
        );
        let Ok(core_agents) = hya_bundle::first_party_bundle("hya/core-agents") else {
            panic!("first-party hya/core-agents must load");
        };
        for agent in core_agents.bundles()[0].agents() {
            if let Some(prompt) = agent.prompt.as_deref() {
                assert!(
                    !prompt.contains("NEVER call `report`"),
                    "{}",
                    agent.id.as_str()
                );
            }
        }

        // Every backtick tool token in the reference resolves in the builtin registry.
        for out in [&main, &member] {
            assert!(out.starts_with("## Team quick reference"));
            for token in out.split('`').skip(1).step_by(2) {
                let token = token.trim();
                if token.is_empty()
                    || token.contains("://")
                    || token.starts_with('#')
                    || token.starts_with('[')
                    || token.starts_with('^')
                {
                    continue;
                }
                assert!(registry.get(token).is_some(), "unknown token `{token}`");
            }
        }

        // Only offered tools are mentioned; no coordination tools => None.
        let narrowed = reference(&["report", "wait", "send", "list_channel", "read"], 1);
        for present in [
            "`report`",
            "`wait`",
            "`send`",
            "`list_channel`",
            "channel://",
        ] {
            assert!(narrowed.contains(present), "missing {present}");
        }
        for absent in ["`task`", "`archive`", "`search_agent`"] {
            assert!(!narrowed.contains(absent), "leaked {absent}");
        }
        let quiet = reference(&["wait", "grep"], 1);
        assert!(!quiet.contains("`send`") && !quiet.contains("channel://"));
        assert_eq!(team_quick_reference(|name| name == "grep", 1), None);
    }

    #[test]
    fn context_file_discovery_entries_and_rendering_order_parent_before_child() {
        let out = render_environment_and_context(
            &env(),
            &[("AGENTS.md".to_string(), "Prefer spaces.".to_string())],
        );
        assert!(out.starts_with("## Environment\n- platform: linux\n- date: 2026-06-21\n"));
        assert!(out.contains("## Project context: AGENTS.md") && out.contains("Prefer spaces."));
        assert!(!out.contains("You are hya"));

        let root = tempdir("discover");
        let parent = root.join("proj");
        let child = parent.join("nested");
        assert!(std::fs::create_dir_all(&child).is_ok());
        assert!(std::fs::write(parent.join("AGENTS.md"), "PARENT_AGENTS_BODY").is_ok());
        assert!(std::fs::write(child.join("AGENTS.md"), "CHILD_AGENTS_BODY!").is_ok());

        let entries = context_file_entries(&child);
        let parent_canon = std::fs::canonicalize(&parent).unwrap_or(parent.clone());
        let p_entry = entries
            .iter()
            .find(|e| e.path == parent_canon.join("AGENTS.md"));
        let c_entry = entries
            .iter()
            .find(|e| e.path == parent_canon.join("nested/AGENTS.md"));
        assert_eq!(p_entry.map(|e| (e.scope, e.bytes)), Some(("parent", 18)));
        assert_eq!(c_entry.map(|e| (e.scope, e.bytes)), Some(("project", 18)));
        let order: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(
            order
                .iter()
                .position(|p| p == &parent_canon.join("AGENTS.md"))
                < order
                    .iter()
                    .position(|p| p == &parent_canon.join("nested/AGENTS.md"))
        );

        let files = discover_context_files(&child);
        let rendered = render_environment_and_context(&env(), &files);
        assert!(
            rendered
                .find("PARENT_AGENTS_BODY")
                .zip(rendered.find("CHILD_AGENTS_BODY!"))
                .is_some_and(|(parent_pos, child_pos)| parent_pos < child_pos)
                && rendered.matches("## Project context:").count() >= 2
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
