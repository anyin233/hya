use std::path::Path;

use super::tools::{REWRITE, TODO};

const INTRO: &str = "You are a coding assistant running inside hya, a coding agent harness. Help users inspect files, run commands, and modify code.";

/// The kernel's tools and their `<tools>` lines, in prompt order.
const TOOLS: [(&str, &str); 6] = [
    ("read", "read a text file or image"),
    ("bash", "run a shell command"),
    ("edit", "replace text in a file"),
    ("write", "create or overwrite a file"),
    (
        REWRITE,
        "remove everything before this call from the conversation, keeping the summary you pass",
    ),
    (
        TODO,
        "record the task's completion criteria as checkable end states and update each one's status when it changes; the current list survives rewrites",
    ),
];

const FILE_OPERATIONS_RULE: &str = "Use bash for file operations like ls, rg, find";

/// The rules after the file-operations rule.
const RULES: [&str; 10] = [
    "Only tool results tell you what is true outside this conversation. Your own text is a plan or a claim until a tool result supports it.",
    "Before reporting that something works, run the check and read its result.",
    "Check the final deliverable itself against the most direct evidence the task gives: the source it was copied or merged from, the literal wording it requires, a stated value, prefix, or dimension. A check built on the same assumption as the thing it checks proves nothing.",
    "When a tool result contradicts your result, resolve the contradiction before doing anything else.",
    "Before replacing a result that passed a check, quote the words of the request that the new one meets and the old one did not. If the request does not say it, keep the old one.",
    "Use every path, value, field and parameter that the request or its input gives, as given. Leaving one out or replacing it needs a reason from the request's own words.",
    "Between results that both fit the request, keep the one its data support most directly. A result is not better for being rounder, simpler or more symmetric, nor for matching a guess about how the work will be checked.",
    "A rewrite summary is your own text, not an observation. Put in it what a later correction would need: the task, what was checked and how, what failed and why, what is still open.",
    "Be concise in your responses",
    "Show file paths clearly when working with files",
];

/// The kernel's system prompt for a session working in `cwd`.
#[must_use]
pub fn system_prompt(cwd: &Path) -> String {
    compose(cwd, |_| true, &[], None)
}

/// The kernel prompt for a round that offers the tools `offered` accepts.
///
/// `<tools>` lists the kernel's line for each offered kernel tool name, in the
/// kernel's order and whoever implements it, then `extra` (name and line, for
/// tools the kernel does not know) in the given order. `file_operations`
/// replaces the rule that sends file operations to `bash`; without it that
/// rule is kept only when `bash` is offered.
#[must_use]
pub fn compose(
    cwd: &Path,
    offered: impl Fn(&str) -> bool,
    extra: &[(&str, &str)],
    file_operations: Option<&str>,
) -> String {
    let tools = TOOLS
        .iter()
        .filter(|(name, _)| offered(name))
        .chain(extra)
        .map(|(name, line)| format!("- {name}: {line}\n"))
        .collect::<String>();
    let file_operations = file_operations.or(offered("bash").then_some(FILE_OPERATIONS_RULE));
    let rules = file_operations
        .into_iter()
        .chain(RULES)
        .map(|rule| format!("- {rule}\n"))
        .collect::<String>();
    format!(
        "{INTRO}\n\n<tools>\n{tools}</tools>\n\n<rules>\n{rules}</rules>\n\n<cwd>\n{}\n</cwd>",
        cwd.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_prompt_is_unchanged() {
        assert_eq!(
            system_prompt(Path::new("/work")),
            "You are a coding assistant running inside hya, a coding agent harness. Help users inspect files, run commands, and modify code.

<tools>
- read: read a text file or image
- bash: run a shell command
- edit: replace text in a file
- write: create or overwrite a file
- rewrite: remove everything before this call from the conversation, keeping the summary you pass
- todo: record the task's completion criteria as checkable end states and update each one's status when it changes; the current list survives rewrites
</tools>

<rules>
- Use bash for file operations like ls, rg, find
- Only tool results tell you what is true outside this conversation. Your own text is a plan or a claim until a tool result supports it.
- Before reporting that something works, run the check and read its result.
- Check the final deliverable itself against the most direct evidence the task gives: the source it was copied or merged from, the literal wording it requires, a stated value, prefix, or dimension. A check built on the same assumption as the thing it checks proves nothing.
- When a tool result contradicts your result, resolve the contradiction before doing anything else.
- Before replacing a result that passed a check, quote the words of the request that the new one meets and the old one did not. If the request does not say it, keep the old one.
- Use every path, value, field and parameter that the request or its input gives, as given. Leaving one out or replacing it needs a reason from the request's own words.
- Between results that both fit the request, keep the one its data support most directly. A result is not better for being rounder, simpler or more symmetric, nor for matching a guess about how the work will be checked.
- A rewrite summary is your own text, not an observation. Put in it what a later correction would need: the task, what was checked and how, what failed and why, what is still open.
- Be concise in your responses
- Show file paths clearly when working with files
</rules>

<cwd>
/work
</cwd>"
        );
    }

    #[test]
    fn composed_prompt_lists_offered_tools_and_picks_the_file_rule() {
        let offered = |name: &str| ["read", "rewrite", "todo"].contains(&name);
        let prompt = compose(Path::new("/w"), offered, &[("lookup", "find things")], None);
        assert!(
            prompt.contains(
                "<tools>\n- read: read a text file or image\n- rewrite: remove everything"
            )
        );
        assert!(prompt.contains("survives rewrites\n- lookup: find things\n</tools>"));
        assert!(!prompt.contains("bash"));
        assert!(prompt.contains("<rules>\n- Only tool results"));

        let prompt = compose(Path::new("/w"), offered, &[], Some("Use the file tools"));
        assert!(prompt.contains("<rules>\n- Use the file tools\n- Only tool results"));
    }
}
