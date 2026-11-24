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
}

/// Commands visible at `place`: builtins, then disk commands from its
/// directory and each Project root in order (first wins), then skills. The
/// global view (no directory) has only builtins and user skills, with
/// `${path}` left unexpanded since there is no directory to name.
pub(crate) fn list(place: &CatalogPlace) -> Vec<CommandInfo> {
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
    let mut disk: Vec<CommandInfo> = Vec::new();
    for dir in place.dirs() {
        for command in crate::support::command_sources::disk_commands(dir) {
            if !disk.iter().any(|known| known.name == command.name) {
                disk.push(command);
            }
        }
    }
    upsert_commands(&mut commands, disk);
    add_skill_commands(&mut commands, place);
    commands
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

/// Expand `command` against the catalog a session's turns see
/// ([`CatalogPlace::for_session`]).
pub(crate) fn expand_prompt(
    place: &CatalogPlace,
    command: &str,
    arguments: &str,
) -> Option<String> {
    list(place)
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

fn expand_template(template: &str, arguments: &str) -> String {
    let positional = split_arguments(arguments);
    let mut out = String::with_capacity(template.len().saturating_add(arguments.len()));
    let mut chars = template.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        if template[idx..].starts_with("$ARGUMENTS") {
            out.push_str(arguments);
            for _ in 0.."ARGUMENTS".len() {
                chars.next();
            }
            continue;
        }
        let mut position = 0usize;
        let mut has_digits = false;
        while let Some((_, next)) = chars.peek().copied() {
            if let Some(digit) = next.to_digit(10) {
                has_digits = true;
                position = position.saturating_mul(10).saturating_add(digit as usize);
                chars.next();
            } else {
                break;
            }
        }
        if has_digits {
            if let Some(replacement) = position.checked_sub(1).and_then(|idx| positional.get(idx)) {
                out.push_str(replacement);
            }
        } else {
            out.push('$');
        }
    }
    out
}

fn split_arguments(arguments: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in arguments.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
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
        }
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

/// One `/skill:<name>` row per visible skill. The `skill:` namespace is
/// reserved (disk commands cannot use it), so a skill never shadows a
/// command or is shadowed by one.
fn add_skill_commands(commands: &mut Vec<CommandInfo>, place: &CatalogPlace) {
    commands.extend(
        crate::support::skill_catalog::list(place)
            .into_iter()
            .map(CommandInfo::skill),
    );
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
        let commands = super::list(&super::CatalogPlace::directory_for_tests("/work"));
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
        let commands = super::list(&place);
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
            super::expand_prompt(&place, "skill:greet", "bob").as_deref(),
            Some(
                format!(
                    "<skill name=\"greet\" location=\"{}\">\nReferences are relative to {}.\n\nSay hello.\n</skill>\n\nbob",
                    skill_dir.join("SKILL.md").display(),
                    skill_dir.display()
                )
                .as_str()
            )
        );
        assert_eq!(
            super::expand_prompt(&place, "greet", "bob").as_deref(),
            Some("COMMAND bob")
        );
        // A built-in skill expands without a reference line.
        let builtin = super::expand_prompt(&place, "skill:secure-self-update", "")
            .unwrap_or_else(|| panic!("missing built-in skill"));
        assert!(
            builtin.starts_with(
                "<skill name=\"secure-self-update\" location=\"embedded:hya/core-skills/skill/secure-self-update/SKILL.md\">\n"
            ),
            "{builtin}"
        );
        assert!(!builtin.contains("References are relative to"), "{builtin}");
        // Client-side rows never expand.
        assert_eq!(super::expand_prompt(&place, "clear", ""), None);
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
