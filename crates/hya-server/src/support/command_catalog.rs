use serde::{Deserialize, Serialize};

use crate::support::catalog_place::CatalogPlace;

/// Identity of the trusted bundle that owns the prompt-template commands.
const CORE_COMMANDS_BUNDLE: &str = "hya/core-commands";

/// Reserved command-name prefix of the skill namespace: the skill `reviewer`
/// runs as `/skill:reviewer`, so a skill never collides with a command.
pub(crate) const SKILL_COMMAND_PREFIX: &str = "skill:";

/// Argument hint of every `/skill:<name>` row: the request is optional.
const SKILL_ARGUMENT_HINT: &str = "[request]";

/// The `commands.yaml` asset of `hya/core-commands`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCommands {
    schema_version: u32,
    commands: Vec<CoreCommand>,
}

/// One prompt-template command declared by `hya/core-commands`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCommand {
    name: String,
    description: String,
    /// `extensions.files` id of the template body.
    template: String,
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default)]
    subtask: Option<bool>,
}

/// Prompt-template commands from `hya/core-commands`, loaded once per process.
fn core_commands() -> &'static [(CoreCommand, String)] {
    static COMMANDS: std::sync::OnceLock<Vec<(CoreCommand, String)>> = std::sync::OnceLock::new();
    COMMANDS.get_or_init(|| {
        load_core_commands().unwrap_or_else(|error| panic!("load {CORE_COMMANDS_BUNDLE}: {error}"))
    })
}

fn load_core_commands() -> Result<Vec<(CoreCommand, String)>, String> {
    let catalog =
        hya_bundle::first_party_bundle(CORE_COMMANDS_BUNDLE).map_err(|error| error.to_string())?;
    let [bundle] = catalog.bundles() else {
        return Err("expected one bundle".to_string());
    };
    let asset = |id: &str| {
        bundle
            .extensions()
            .iter()
            .find(|asset| asset.local_id == id)
            .map(|asset| asset.content.clone())
            .ok_or_else(|| format!("missing `{id}` file"))
    };
    let declared: CoreCommands =
        serde_norway::from_str(&asset("commands")?).map_err(|error| error.to_string())?;
    if declared.schema_version != 1 {
        return Err(format!(
            "unsupported commands schema {}",
            declared.schema_version
        ));
    }
    declared
        .commands
        .into_iter()
        .map(|command| {
            let template = asset(&command.template)?;
            Ok((command, template))
        })
        .collect()
}

#[derive(Serialize)]
pub(crate) struct CommandInfo {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,

    pub(crate) source: &'static str,
    #[serde(skip)]
    expansion: Expansion,
    pub(crate) template: String,
    /// Argument hint shown after the name (`<file>` required, `[focus]` optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) argument_hint: Option<String>,

    pub(crate) hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtask: Option<bool>,
    /// `builtin`, `project`, `user`, or a skill row's source
    /// (`CommandSummary.scope`).
    pub(crate) scope: &'static str,
}

/// A place's commands and the diagnostics of the command files it skipped.
pub(crate) struct CommandListing {
    pub(crate) commands: Vec<CommandInfo>,
    pub(crate) diagnostics: Vec<crate::support::command_sources::CommandDiagnostic>,
}

/// Commands visible at `place`: builtins, then disk commands from its
/// directory and each Project root in order, then the user's
/// (`~/.config/hya/commands`; first wins), then a `skill:<name>` row per
/// skill. The global view (no directory) has the builtins, the user's
/// commands, and user skills, with `${path}` left unexpanded since there is
/// no directory to name.
pub(crate) fn list(st: &crate::ServerState, place: &CatalogPlace) -> Vec<CommandInfo> {
    listing(st, place).commands
}

/// [`list`] with the diagnostics of the command files it skipped.
pub(crate) fn listing(st: &crate::ServerState, place: &CatalogPlace) -> CommandListing {
    listing_with_skills(place, crate::support::skill_catalog::list(st, place))
}

/// [`listing`] with `skills` as the `skill:<name>` rows (a session's own
/// skill view, bundle skills included).
pub(crate) fn listing_with_skills(
    place: &CatalogPlace,
    skills: Vec<crate::support::skill_catalog::SkillInfo>,
) -> CommandListing {
    let workdir = place.workdir();
    let mut commands = core_commands()
        .iter()
        .map(|(command, template)| CommandInfo {
            hints: command.hints.clone(),
            ..command_info(
                command.name.clone(),
                command.description.clone(),
                match workdir {
                    Some(workdir) => template.replace("${path}", &workdir.to_string_lossy()),
                    None => template.clone(),
                },
                Vec::new(),
                command.subtask,
            )
        })
        .collect::<Vec<_>>();
    commands.extend([
        command_info(
            "help",
            "show this help",
            "/help".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "model",
            "switch the active model",
            "/model $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
        command_info(
            "clear",
            "start a fresh session",
            "/clear".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "sessions",
            "switch to another session",
            "/sessions".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "workflow",
            "inspect or run workflows",
            "/workflow $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let mut disk: Vec<CommandInfo> = Vec::new();
    let project = place.dirs().into_iter().flat_map(|dir| {
        crate::support::command_sources::disk_commands(dir, "project", &mut diagnostics)
    });
    let project = project.collect::<Vec<_>>();
    let user = crate::support::command_sources::root_commands(
        &crate::support::command_sources::user_command_roots(),
        "user",
        &mut diagnostics,
    );
    for command in project.into_iter().chain(user) {
        if !disk.iter().any(|known| known.name == command.name) {
            disk.push(command);
        }
    }
    upsert_commands(&mut commands, disk);
    commands.extend(skills.into_iter().map(CommandInfo::skill));
    CommandListing {
        commands,
        diagnostics,
    }
}

/// How a catalog row turns into prompt text.
#[derive(Clone, Debug)]
enum Expansion {
    /// A client-side command (`/help`, `/clear`): never expanded here.
    None,
    /// A prompt template with `$1` / `$ARGUMENTS` placeholders.
    Template,
    /// A skill: a pi-style `<skill>` block with the arguments after it.
    Skill {
        /// Catalog path named in the block's `location` attribute.
        location: String,
        /// The skill's directory, the base of its relative references.
        dir: Option<String>,
    },
}

/// Expand `command` against the catalog of `place`.
fn expand_prompt(
    st: &crate::ServerState,
    place: &CatalogPlace,
    command: &str,
    arguments: &str,
) -> Option<String> {
    expand_in(list(st, place), command, arguments)
}

/// Expand `command` against the catalog `session`'s turns see
/// ([`CatalogPlace::for_session`]): a `skill:<name>` also finds the bundle
/// and plugin skills its agent can load.
pub(crate) async fn expand_session_prompt(
    st: &crate::ServerState,
    session: hya_proto::SessionId,
    place: &CatalogPlace,
    command: &str,
    arguments: &str,
) -> Option<String> {
    if !command.starts_with(SKILL_COMMAND_PREFIX) {
        return expand_prompt(st, place, command, arguments);
    }
    let skills = crate::support::skill_catalog::session_listing(st, session, place)
        .await
        .skills;
    expand_in(
        listing_with_skills(place, skills).commands,
        command,
        arguments,
    )
}

fn expand_in(commands: Vec<CommandInfo>, command: &str, arguments: &str) -> Option<String> {
    commands
        .into_iter()
        .find(|item| item.name == command)
        .and_then(|item| item.expand(arguments))
}

/// The text a `/skill:<name> [request]` turn sends, as pi writes it:
///
/// ```text
/// <skill name="reviewer" location="/w/.hya/skills/reviewer/SKILL.md">
/// References are relative to /w/.hya/skills/reviewer.
///
/// <body>
/// </skill>
///
/// <request>
/// ```
///
/// The reference line is left out for a built-in skill (no directory), and
/// the request (with its blank line) when it is empty. Attribute values
/// escape `&`, `"`, `<`, and `>`.
pub(crate) fn skill_block(
    name: &str,
    location: &str,
    dir: Option<&str>,
    body: &str,
    arguments: &str,
) -> String {
    let mut out = format!(
        "<skill name=\"{}\" location=\"{}\">\n",
        escape_attribute(name),
        escape_attribute(location)
    );
    if let Some(dir) = dir {
        out.push_str("References are relative to ");
        out.push_str(dir);
        out.push_str(".\n\n");
    }
    out.push_str(body.trim());
    out.push_str("\n</skill>");
    let request = arguments.trim();
    if !request.is_empty() {
        out.push_str("\n\n");
        out.push_str(request);
    }
    out
}

fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// Expand a command template with pi's substitutions:
///
/// | Syntax | Result |
/// | --- | --- |
/// | `$1`, `$2`, … | One positional argument (empty when missing) |
/// | `$ARGUMENTS`, `$@` | The arguments as typed |
/// | `${1:-default}` | The first argument, or `default` when it is missing or empty |
/// | `${@:-default}` | The arguments, or `default` when there are none |
/// | `${@:N}` | The arguments from position `N`, joined with spaces |
/// | `${@:N:L}` | `L` arguments from position `N` |
///
/// Any other `$` or `${…}` stays as written, and a replacement is never
/// expanded again.
fn expand_template(template: &str, arguments: &str) -> String {
    let positional = split_arguments(arguments);
    let arg = |index: usize| -> &str {
        index
            .checked_sub(1)
            .and_then(|index| positional.get(index))
            .map_or("", String::as_str)
    };
    let mut out = String::with_capacity(template.len().saturating_add(arguments.len()));
    let mut rest = template;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$ARGUMENTS") {
            out.push_str(arguments);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("$@") {
            out.push_str(arguments);
            rest = after;
        } else if let Some((replacement, after)) = expand_braced(tail, arguments, &positional) {
            out.push_str(&replacement);
            rest = after;
        } else {
            let digits = tail[1..].bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                let position = tail[1..=digits].parse::<usize>().unwrap_or(usize::MAX);
                out.push_str(arg(position));
                rest = &tail[1 + digits..];
            } else {
                out.push('$');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A `${…}` form at the start of `tail`: its replacement and the text after
/// it, or `None` when it is not one of the supported forms.
fn expand_braced<'a>(
    tail: &'a str,
    arguments: &str,
    positional: &[String],
) -> Option<(String, &'a str)> {
    let body_start = tail.strip_prefix("${")?;
    let close = body_start.find('}')?;
    let body = &body_start[..close];
    let after = &body_start[close + 1..];
    let (head, default) = match body.split_once(":-") {
        Some((head, default)) => (head, Some(default)),
        None => (body, None),
    };
    if let Some(default) = default {
        let value = if head == "@" {
            arguments.trim().to_owned()
        } else {
            let position = head
                .parse::<usize>()
                .ok()
                .filter(|position| *position > 0)?;
            positional.get(position - 1).cloned().unwrap_or_default()
        };
        return Some((
            if value.is_empty() {
                default.to_owned()
            } else {
                value
            },
            after,
        ));
    }
    let range = head.strip_prefix("@:")?;
    let (start, length) = match range.split_once(':') {
        Some((start, length)) => (start, Some(length.parse::<usize>().ok()?)),
        None => (range, None),
    };
    let start = start.parse::<usize>().ok().filter(|start| *start > 0)?;
    let from = positional.iter().skip(start - 1);
    let picked = match length {
        Some(length) => from.take(length).cloned().collect::<Vec<_>>(),
        None => from.cloned().collect::<Vec<_>>(),
    };
    Some((picked.join(" "), after))
}

/// Split the arguments as pi does: on whitespace, where a quote only opens
/// at the start of an argument (so `doesn't` stays one word) and one with no
/// closing match is plain text; inside quotes a backslash before the same
/// quote keeps it, other backslashes stay as typed.
fn split_arguments(arguments: &str) -> Vec<String> {
    let chars = arguments.chars().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at].is_whitespace() {
            at += 1;
            continue;
        }
        let mut current = String::new();
        if let Some(quote) = chars.get(at).copied().filter(|ch| matches!(ch, '"' | '\'')) {
            let mut end = at + 1;
            let mut closed = None;
            while end < chars.len() {
                if chars[end] == '\\' && chars.get(end + 1) == Some(&quote) {
                    end += 2;
                    continue;
                }
                if chars[end] == quote {
                    closed = Some(end);
                    break;
                }
                end += 1;
            }
            if let Some(end) = closed {
                let mut inner = at + 1;
                while inner < end {
                    if chars[inner] == '\\' && chars.get(inner + 1) == Some(&quote) {
                        current.push(quote);
                        inner += 2;
                    } else {
                        current.push(chars[inner]);
                        inner += 1;
                    }
                }
                at = end + 1;
            }
        }
        while at < chars.len() && !chars[at].is_whitespace() {
            current.push(chars[at]);
            at += 1;
        }
        out.push(current);
    }
    out
}

fn command_info(
    name: impl Into<String>,
    description: impl Into<String>,
    template: String,
    hints: Vec<&'static str>,
    subtask: Option<bool>,
) -> CommandInfo {
    CommandInfo {
        name: name.into(),
        description: Some(description.into()),
        agent: None,
        model: None,
        source: "command",
        expansion: Expansion::None,
        template,
        argument_hint: None,
        hints: hints.into_iter().map(str::to_string).collect(),
        subtask,
        scope: "builtin",
    }
}
impl CommandInfo {
    /// The prompt text of this row for `arguments`; `None` for a
    /// client-side command.
    fn expand(&self, arguments: &str) -> Option<String> {
        match &self.expansion {
            Expansion::None => None,
            Expansion::Template => Some(expand_template(&self.template, arguments)),
            Expansion::Skill { location, dir } => {
                let name = self
                    .name
                    .strip_prefix(SKILL_COMMAND_PREFIX)
                    .unwrap_or(&self.name);
                Some(skill_block(
                    name,
                    location,
                    dir.as_deref(),
                    &self.template,
                    arguments,
                ))
            }
        }
    }

    pub(super) fn command(
        name: String,
        description: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        template: String,
        subtask: Option<bool>,
    ) -> Self {
        Self {
            name,
            description,
            agent,
            model,
            source: "command",
            expansion: Expansion::Template,
            hints: command_hints(&template),
            template,
            argument_hint: None,
            subtask,
            scope: "project",
        }
    }

    /// The frontmatter's `argument-hint` (`<file>` required, `[focus]` optional).
    pub(super) fn with_argument_hint(mut self, hint: Option<String>) -> Self {
        self.argument_hint = hint.filter(|hint| !hint.trim().is_empty());
        self
    }

    /// Where the command is defined (`project`, `user`).
    pub(super) fn with_scope(mut self, scope: &'static str) -> Self {
        self.scope = scope;
        self
    }

    /// The `/skill:<name>` row of `skill`: its body is the template, expanded
    /// into a `<skill>` block ([`skill_block`]).
    fn skill(skill: crate::support::skill_catalog::SkillInfo) -> Self {
        Self {
            name: format!("{SKILL_COMMAND_PREFIX}{}", skill.name),
            description: Some(skill.description),
            agent: None,
            model: None,
            source: "skill",
            expansion: Expansion::Skill {
                location: skill.path,
                dir: skill.dir,
            },
            template: skill.content,
            argument_hint: Some(SKILL_ARGUMENT_HINT.to_string()),
            hints: Vec::new(),
            subtask: None,
            scope: skill.source,
        }
    }
}

fn upsert_commands(commands: &mut Vec<CommandInfo>, incoming: Vec<CommandInfo>) {
    for command in incoming {
        if let Some(existing) = commands.iter_mut().find(|item| item.name == command.name) {
            *existing = command;
        } else {
            commands.push(command);
        }
    }
}

fn command_hints(template: &str) -> Vec<String> {
    let mut numbered = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index > start + 1 {
                let hint = &template[start..index];
                if !numbered.iter().any(|existing| existing == hint) {
                    numbered.push(hint.to_string());
                }
                continue;
            }
        }
        index += 1;
    }
    numbered.sort();
    if template.contains("$ARGUMENTS") {
        numbered.push("$ARGUMENTS".to_string());
    }
    numbered
}

#[cfg(test)]
mod tests {
    use super::expand_template;

    #[test]
    fn init_and_review_templates_come_from_the_core_commands_bundle() {
        let catalog = hya_bundle::first_party_bundle("hya/core-commands")
            .unwrap_or_else(|error| panic!("{error}"));
        let asset = |id: &str| {
            catalog.bundles()[0]
                .extensions()
                .iter()
                .find(|asset| asset.local_id == id)
                .map(|asset| asset.content.replace("${path}", "/work"))
                .unwrap_or_else(|| panic!("missing {id} asset"))
        };
        let place = super::CatalogPlace::directory_for_tests("/work");
        let commands = super::listing_with_skills(&place, Vec::new()).commands;
        for (name, id, subtask) in [("init", "init", None), ("review", "review", Some(true))] {
            let command = commands
                .iter()
                .find(|command| command.name == name)
                .unwrap_or_else(|| panic!("missing /{name}"));
            assert_eq!(command.template, asset(id), "/{name}");
            assert_eq!(command.subtask, subtask, "/{name}");
        }
    }

    #[test]
    fn skill_block_names_the_skill_its_directory_and_appends_the_request() {
        assert_eq!(
            super::skill_block(
                "reviewer",
                "/w/.hya/skills/reviewer/SKILL.md",
                Some("/w/.hya/skills/reviewer"),
                "\n# Reviewer\n\nRead $ARGUMENTS.\n",
                "  fix auth ",
            ),
            "<skill name=\"reviewer\" location=\"/w/.hya/skills/reviewer/SKILL.md\">\n\
             References are relative to /w/.hya/skills/reviewer.\n\n\
             # Reviewer\n\nRead $ARGUMENTS.\n</skill>\n\nfix auth"
        );
        // A built-in skill has no directory, and no request adds no tail.
        assert_eq!(
            super::skill_block("b", "embedded:x/\"q\"&<>", None, "body", ""),
            "<skill name=\"b\" location=\"embedded:x/&quot;q&quot;&amp;&lt;&gt;\">\nbody\n</skill>"
        );
    }

    #[test]
    fn skills_are_skill_prefixed_rows_and_never_shadow_or_yield_to_commands() {
        let dir = std::env::temp_dir().join(format!(
            "hya-skill-commands-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap_or(&dir))
                .unwrap_or_else(|error| panic!("{error}"));
            std::fs::write(path, text).unwrap_or_else(|error| panic!("{error}"));
        };
        write(
            ".hya/skills/greet/SKILL.md",
            "---\nname: greet\ndescription: Say hello\n---\nSay hello.\n",
        );
        write(".hya/commands/greet.md", "COMMAND $ARGUMENTS\n");
        write(".hya/commands/skill:greet.md", "RESERVED\n");
        let place = super::CatalogPlace::directory_for_tests(&dir.to_string_lossy());
        let skills = hya_app::skill_catalog::discover(place.workdir(), place.roots())
            .skills
            .into_iter()
            .map(|skill| crate::support::skill_catalog::info(skill, &place))
            .collect();
        let commands = super::listing_with_skills(&place, skills).commands;
        let expand = |name, arguments| {
            commands
                .iter()
                .find(|row| row.name == name)
                .and_then(|row| row.expand(arguments))
        };
        let named = |name: &str| commands.iter().filter(|row| row.name == name).count();
        assert_eq!(named("greet"), 1);
        assert_eq!(
            named("skill:greet"),
            1,
            "the reserved file is not a command"
        );
        let skill = commands
            .iter()
            .find(|row| row.name == "skill:greet")
            .unwrap_or_else(|| panic!("missing /skill:greet"));
        assert_eq!(skill.source, "skill");
        assert_eq!(skill.argument_hint.as_deref(), Some("[request]"));
        let skill_dir = dir.join(".hya/skills/greet");
        assert_eq!(
            expand("skill:greet", "bob").as_deref(),
            Some(
                format!(
                    "<skill name=\"greet\" location=\"{}\">\nReferences are relative to {}.\n\nSay hello.\n</skill>\n\nbob",
                    skill_dir.join("SKILL.md").display(),
                    skill_dir.display()
                )
                .as_str()
            )
        );
        assert_eq!(expand("greet", "bob").as_deref(), Some("COMMAND bob"));
        // A built-in skill expands without a reference line.
        let builtin = expand("skill:secure-self-update", "")
            .unwrap_or_else(|| panic!("missing built-in skill"));
        assert!(
            builtin.starts_with(
                "<skill name=\"secure-self-update\" location=\"embedded:hya/core-skills/skill/secure-self-update/SKILL.md\">\n"
            ),
            "{builtin}"
        );
        assert!(!builtin.contains("References are relative to"), "{builtin}");
        // Client-side rows never expand.
        assert_eq!(expand("clear", ""), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_pi_template_forms() {
        let expand = super::expand_template;
        assert_eq!(expand("a=$@|b=$ARGUMENTS", "x  y"), "a=x  y|b=x  y");
        assert_eq!(expand("${1:-none}/${2:-none}", "one"), "one/none");
        assert_eq!(expand("${@:-everything}", "  "), "everything");
        assert_eq!(expand("${@:-everything}", "a b"), "a b");
        assert_eq!(expand("${@:2}", "a b c d"), "b c d");
        assert_eq!(expand("${@:2:2}", "a b c d"), "b c");
        assert_eq!(expand("${@:9}", "a b"), "");
        // Anything else stays as written.
        assert_eq!(
            expand("${HOME} ${0:-x} ${@:0} ${@:x} $ $x ${unclosed", "a"),
            "${HOME} ${0:-x} ${@:0} ${@:x} $ $x ${unclosed"
        );
    }

    #[test]
    fn arguments_split_with_pi_quoting() {
        let split = super::split_arguments;
        assert_eq!(split("\"hello world\" tail"), ["hello world", "tail"]);
        assert_eq!(split("doesn't stop"), ["doesn't", "stop"]);
        assert_eq!(split("\"open value"), ["\"open", "value"]);
        assert_eq!(
            split(r#""the \"fast\" path" c:\dir"#),
            [r#"the "fast" path"#, r"c:\dir"]
        );
        assert_eq!(split("'' x"), ["", "x"]);
    }

    #[test]
    fn command_files_take_argument_hints_first_line_descriptions_and_report_skips() {
        let dir = std::env::temp_dir().join(format!(
            "hya-command-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap_or(&dir))
                .unwrap_or_else(|error| panic!("{error}"));
            std::fs::write(path, text).unwrap_or_else(|error| panic!("{error}"));
        };
        write(
            "commands/review.md",
            "---\ndescription: Review\nargument-hint: \"[focus]\"\n---\nReview ${1:-everything}\n",
        );
        write("commands/plain.md", "\n\n  Fix the build please  \nmore\n");
        write("commands/broken.md", "---\ndescription: [x\n---\nbody\n");
        write("commands/skill:x.md", "reserved\n");
        let mut diagnostics = Vec::new();
        let commands = crate::support::command_sources::root_commands(
            &[dir.join("commands")],
            "user",
            &mut diagnostics,
        );
        let review = commands
            .iter()
            .find(|row| row.name == "review")
            .unwrap_or_else(|| panic!("review"));
        assert_eq!(review.argument_hint.as_deref(), Some("[focus]"));
        assert_eq!(review.scope, "user");
        let plain = commands
            .iter()
            .find(|row| row.name == "plain")
            .unwrap_or_else(|| panic!("plain"));
        assert_eq!(plain.description.as_deref(), Some("Fix the build please"));
        assert_eq!(commands.len(), 2);
        let kinds = diagnostics.iter().map(|row| row.kind).collect::<Vec<_>>();
        assert_eq!(kinds, ["invalid", "reserved"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_full_numeric_placeholder_without_reexpanding_arguments() {
        let arguments = "one two three four five six seven eight nine-literal-$1 ten";

        assert_eq!(
            expand_template("first=$1 tenth=$10 missing=$11 all=$ARGUMENTS", arguments),
            "first=one tenth=ten missing= all=one two three four five six seven eight nine-literal-$1 ten"
        );
    }
}
