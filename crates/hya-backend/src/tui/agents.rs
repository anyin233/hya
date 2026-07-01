use hya_core::AgentSpec;
use hya_legacy_tui::DialogItem;
use hya_proto::AgentName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub prompt_append: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutedPrompt {
    pub agent: String,
    pub prompt: String,
}

impl AgentProfile {
    fn new(name: &str, description: &str, prompt_append: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            prompt_append: prompt_append.to_string(),
        }
    }
}

#[must_use]
pub fn builtin_profiles() -> Vec<AgentProfile> {
    vec![
        AgentProfile::new("build", "Default coding agent", ""),
        AgentProfile::new(
            "plan",
            "Plan before editing",
            "Plan first. Do not edit until the implementation path is clear.",
        ),
        AgentProfile::new(
            "general",
            "General assistant",
            "Answer directly and keep tool use minimal.",
        ),
        AgentProfile::new(
            "explore",
            "Explore the codebase",
            "Investigate broadly and report grounded findings before changing code.",
        ),
        AgentProfile::new(
            "scout",
            "Fast focused search",
            "Search quickly for the narrowest relevant evidence.",
        ),
    ]
}

#[must_use]
pub fn profile_by_name<'a>(profiles: &'a [AgentProfile], name: &str) -> Option<&'a AgentProfile> {
    profiles.iter().find(|profile| profile.name == name)
}

#[must_use]
pub fn dialog_items(profiles: &[AgentProfile]) -> Vec<DialogItem> {
    profiles
        .iter()
        .map(|profile| DialogItem {
            label: profile.name.clone(),
            detail: profile.description.clone(),
        })
        .collect()
}

#[must_use]
pub fn strip_leading_agent_mention(input: &str) -> Option<RoutedPrompt> {
    let trimmed = input.trim_start();
    let (first, rest) = trimmed.split_once(char::is_whitespace)?;
    let agent = first.strip_prefix('@')?;
    let profiles = builtin_profiles();
    profile_by_name(&profiles, agent)?;
    Some(RoutedPrompt {
        agent: agent.to_string(),
        prompt: rest.trim_start().to_string(),
    })
}

pub fn apply_profile(agent: &mut AgentSpec, base_prompt: &str, profile: &AgentProfile) {
    agent.name = AgentName::new(&profile.name);
    agent.system_prompt = if profile.prompt_append.is_empty() {
        base_prompt.to_string()
    } else {
        format!("{base_prompt}\n\n{}", profile.prompt_append)
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtins_include_opencode_style_agent_profiles() {
        let profiles = super::builtin_profiles();

        assert!(profiles.iter().any(|profile| profile.name == "build"));
        assert!(profiles.iter().any(|profile| profile.name == "plan"));
        assert!(profiles.iter().any(|profile| profile.name == "explore"));
        assert!(profiles.iter().any(|profile| profile.name == "scout"));
    }

    #[test]
    fn leading_agent_mention_selects_agent_and_removes_mention() {
        let routed = super::strip_leading_agent_mention("@plan sketch the design");

        assert_eq!(
            routed.as_ref().map(|routed| routed.agent.as_str()),
            Some("plan")
        );
        assert_eq!(
            routed.as_ref().map(|routed| routed.prompt.as_str()),
            Some("sketch the design")
        );
    }
}
