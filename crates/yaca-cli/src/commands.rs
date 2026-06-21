use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Slash {
    Help,
    Model(String),
    Clear,
    Exit,
    Sessions,
    Template(String),
}

#[must_use]
pub fn parse_slash(input: &str) -> Option<Slash> {
    let rest = input.trim().strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    Some(match cmd {
        "help" | "?" => Slash::Help,
        "model" => Slash::Model(arg.to_string()),
        "clear" | "new" => Slash::Clear,
        "exit" | "quit" | "q" => Slash::Exit,
        "sessions" => Slash::Sessions,
        other if !other.is_empty() => Slash::Template(other.to_string()),
        _ => Slash::Help,
    })
}

#[must_use]
pub fn help_text() -> String {
    "Commands:\n\
     /help            show this help\n\
     /model <id>      switch the active model\n\
     /clear, /new     start a fresh session\n\
     /exit, /quit     quit yaca\n\
     /sessions        switch to another session\n\
     /<name>          run prompt template <name>.md"
        .to_string()
}

#[must_use]
pub fn resolve_template(name: &str, dirs: &[PathBuf]) -> Option<String> {
    for d in dirs {
        let path = d.join(format!("{name}.md"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("yaca-cmd-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_builtins_and_templates() {
        assert_eq!(parse_slash("/help"), Some(Slash::Help));
        assert_eq!(
            parse_slash("/model gpt-5"),
            Some(Slash::Model("gpt-5".to_string()))
        );
        assert_eq!(parse_slash("/model"), Some(Slash::Model(String::new())));
        assert_eq!(parse_slash("/clear"), Some(Slash::Clear));
        assert_eq!(parse_slash("/new"), Some(Slash::Clear));
        assert_eq!(parse_slash("/exit"), Some(Slash::Exit));
        assert_eq!(parse_slash("/sessions"), Some(Slash::Sessions));
        assert_eq!(
            parse_slash("/review"),
            Some(Slash::Template("review".to_string()))
        );
        assert_eq!(parse_slash("hello world"), None);
    }

    #[test]
    fn help_lists_core_commands() {
        let h = help_text();
        assert!(h.contains("/help"));
        assert!(h.contains("/model"));
        assert!(h.contains("/clear"));
        assert!(h.contains("/exit"));
    }

    #[test]
    fn resolve_template_reads_first_match() {
        let dir = tempdir();
        std::fs::write(dir.join("review.md"), "Please review the diff.").unwrap();
        assert_eq!(
            resolve_template("review", std::slice::from_ref(&dir)),
            Some("Please review the diff.".to_string())
        );
        assert_eq!(
            resolve_template("missing", std::slice::from_ref(&dir)),
            None
        );
    }
}
