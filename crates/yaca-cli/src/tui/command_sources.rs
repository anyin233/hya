use std::path::Path;

use crate::skills;

use super::commands;

pub fn custom_commands(workdir: &Path) -> Vec<commands::CustomCommand> {
    let mut commands = commands::load_markdown_commands(workdir).unwrap_or_default();
    commands.extend(
        skills::discover_skills(&skills::skill_dirs(workdir))
            .into_iter()
            .map(|skill| commands::CustomCommand::skill(skill.name, skill.description)),
    );
    commands
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "yaca-command-source-test-{nanos}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn custom_commands_include_project_skills() {
        let root = temp_root();
        let skill_dir = root.join(".yaca/skills/review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review the current diff\n---\nbody",
        )
        .unwrap();

        let commands = super::custom_commands(&root);

        let review = commands
            .iter()
            .find(|command| command.name == "review")
            .expect("review skill command");
        assert!(review.is_skill());
        assert_eq!(review.description, "Review the current diff");
    }
}
