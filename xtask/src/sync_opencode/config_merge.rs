use crate::sync_opencode::discover::McpCandidate;

pub(crate) fn merge_managed_mcp(current: &str, managed: &[&McpCandidate]) -> String {
    let mut output = Vec::new();
    let mut current_section: Option<String> = None;
    let mut skip_section = false;
    let managed_names = managed
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect::<Vec<_>>();

    for line in current.lines() {
        let top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if top_level {
            current_section = None;
            skip_section = false;
        }

        if line.trim() == "mcp:" {
            current_section = Some("mcp".to_string());
            output.push(line.to_string());
            continue;
        }

        if current_section.as_deref() == Some("mcp")
            && line.starts_with("  ")
            && !line.starts_with("    ")
        {
            let section_name = line.trim().trim_end_matches(':').to_string();
            skip_section = managed_names
                .iter()
                .any(|managed_name| managed_name == &section_name);
            if skip_section {
                continue;
            }
        }

        if current_section.as_deref() == Some("mcp") && skip_section {
            continue;
        }

        output.push(line.to_string());
    }

    let has_mcp = output.iter().any(|line| line.trim() == "mcp:");
    if !has_mcp {
        if !output.is_empty() {
            output.push(String::new());
        }
        output.push("mcp:".to_string());
    }

    let mcp_index = output
        .iter()
        .position(|line| line.trim() == "mcp:")
        .unwrap_or_else(|| unreachable!("mcp header should exist after insertion"));
    let insertion_index = output
        .iter()
        .enumerate()
        .skip(mcp_index + 1)
        .find(|(_, line)| !line.starts_with(" ") && !line.is_empty())
        .map_or(output.len(), |(index, _)| index);

    let mut rendered = Vec::new();
    for (index, line) in output.into_iter().enumerate() {
        if index == insertion_index {
            rendered.extend(render_managed_mcp(managed));
        }
        rendered.push(line);
    }
    if insertion_index == rendered.len() {
        rendered.extend(render_managed_mcp(managed));
    }

    let mut joined = rendered.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

pub(crate) fn remove_managed_mcp(current: &str, managed_ids: &[String]) -> String {
    let mut output = Vec::new();
    let mut current_section: Option<String> = None;
    let mut skip_section = false;

    for line in current.lines() {
        let top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if top_level {
            current_section = None;
            skip_section = false;
        }

        if line.trim() == "mcp:" {
            current_section = Some("mcp".to_string());
            output.push(line.to_string());
            continue;
        }

        if current_section.as_deref() == Some("mcp")
            && line.starts_with("  ")
            && !line.starts_with("    ")
        {
            let section_name = line.trim().trim_end_matches(':').to_string();
            skip_section = managed_ids
                .iter()
                .any(|managed_id| managed_id == &section_name);
            if skip_section {
                continue;
            }
        }

        if current_section.as_deref() == Some("mcp") && skip_section {
            continue;
        }

        output.push(line.to_string());
    }

    let mut joined = output.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn render_managed_mcp(managed: &[&McpCandidate]) -> Vec<String> {
    let mut lines = Vec::new();
    for candidate in managed {
        lines.push(format!("  {}:", candidate.name));
        lines.push("    command:".to_string());
        for part in &candidate.command {
            lines.push(format!("      - {part}"));
        }
        if !candidate.env.is_empty() {
            lines.push("    env:".to_string());
            for (key, value) in &candidate.env {
                lines.push(format!("      {key}: {}", quote_yaml_scalar(value)));
            }
        }
        lines.push("    enabled: true".to_string());
    }
    lines
}

fn quote_yaml_scalar(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(crate) fn existing_mcp_names(current: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_mcp = false;

    for line in current.lines() {
        let top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if top_level {
            in_mcp = line.trim() == "mcp:";
            continue;
        }
        if in_mcp && line.starts_with("  ") && !line.starts_with("    ") {
            names.push(line.trim().trim_end_matches(':').to_string());
        }
    }

    names
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;
    use crate::sync_opencode::discover::McpCandidate;

    #[derive(Deserialize)]
    struct ParsedMcp {
        mcp: BTreeMap<String, ParsedServer>,
    }

    #[derive(Deserialize)]
    struct ParsedServer {
        command: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        enabled: bool,
    }

    #[test]
    fn rendered_env_template_round_trips_as_string() {
        let mut env = BTreeMap::new();
        env.insert("CG_TOKEN".to_string(), "{env:CG_TOKEN}".to_string());
        let candidate = McpCandidate {
            name: "codegraph".to_string(),
            command: vec!["codegraph".to_string(), "serve".to_string()],
            env,
        };

        let rendered = merge_managed_mcp("default_model: offline\n", &[&candidate]);
        let parsed: ParsedMcp = serde_norway::from_str(&rendered)
            .unwrap_or_else(|error| panic!("rendered yaml should parse: {error}\n{rendered}"));

        let server = parsed.mcp.get("codegraph").expect("codegraph entry");
        assert_eq!(server.command, vec!["codegraph", "serve"]);
        assert_eq!(
            server.env.get("CG_TOKEN").map(String::as_str),
            Some("{env:CG_TOKEN}")
        );
        assert!(server.enabled);
    }
}
