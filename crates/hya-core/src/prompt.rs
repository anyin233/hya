pub struct PromptEnv {
    pub cwd: String,
    pub platform: String,
    pub date: String,
}

#[must_use]
pub fn build_system_prompt(
    base: &str,
    env: &PromptEnv,
    context_files: &[(String, String)],
) -> String {
    let mut out = format!(
        "{}\n\n## Environment\n- cwd: {}\n- platform: {}\n- date: {}\n",
        base.trim(),
        env.cwd,
        env.platform,
        env.date
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
}
