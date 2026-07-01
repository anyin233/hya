use std::collections::BTreeSet;

#[must_use]
pub fn expand_skill_mentions(input: &str) -> String {
    let skills = skill_mentions(input);
    if skills.is_empty() {
        return input.to_string();
    }
    let instructions = skills
        .into_iter()
        .map(|name| {
            format!(
                "- {name}: call the `skill` tool with {{\"name\":\"{}\"}} before acting, then follow the loaded instructions.",
                escape_json(&name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{input}\n\n<context source=\"@skills\">\nThe user explicitly selected these skills for this prompt:\n{instructions}\n</context>"
    )
}

fn skill_mentions(input: &str) -> BTreeSet<String> {
    input
        .split_whitespace()
        .filter_map(|token| {
            let raw = token.strip_prefix("@skill:")?;
            let name = raw.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | ')' | ']'));
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

fn escape_json(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    #[test]
    fn expands_skill_mentions_into_tool_instruction_context() {
        let expanded = super::expand_skill_mentions("Use @skill:release please");

        assert!(expanded.starts_with("Use @skill:release please"));
        assert!(expanded.contains("<context source=\"@skills\">"));
        assert!(expanded.contains("call the `skill` tool with {\"name\":\"release\"}"));
    }

    #[test]
    fn skill_mentions_are_deduplicated() {
        let expanded = super::expand_skill_mentions("@skill:release and @skill:release");

        assert_eq!(expanded.matches("{\"name\":\"release\"}").count(), 1);
    }

    #[test]
    fn malformed_skill_mentions_are_ignored() {
        assert_eq!(
            super::expand_skill_mentions("email@example.com"),
            "email@example.com"
        );
        assert_eq!(super::expand_skill_mentions("@skill: "), "@skill: ");
    }
}
