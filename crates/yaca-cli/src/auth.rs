use std::path::{Path, PathBuf};

pub fn auth_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("yaca/auth"))
}

pub fn save_token_in(dir: &Path, provider: &str, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body = format!("token: \"{}\"\n", yaml_escape(token.trim()));
    std::fs::write(dir.join(format!("{provider}.yaml")), body)
}

#[must_use]
pub fn load_token_in(dir: &Path, provider: &str) -> Option<String> {
    let content = std::fs::read_to_string(dir.join(format!("{provider}.yaml"))).ok()?;
    let token = parse_token_field(&content)?;
    (!token.is_empty()).then_some(token)
}

fn yaml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

fn parse_token_field(content: &str) -> Option<String> {
    let value = content
        .lines()
        .find_map(|line| line.trim().strip_prefix("token:"))?
        .trim();
    Some(yaml_unquote(value))
}

fn yaml_unquote(s: &str) -> String {
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() - 2);
    let mut chars = s[1..s.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn save_token(provider: &str, token: &str) -> std::io::Result<()> {
    let dir = auth_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    save_token_in(&dir, provider, token)
}

#[must_use]
pub fn load_token(provider: &str) -> Option<String> {
    load_token_in(&auth_dir()?, provider)
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
        let dir = std::env::temp_dir().join(format!("yaca-auth-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_token_and_missing_is_none() {
        let dir = tempdir();
        assert_eq!(load_token_in(&dir, "anthropic"), None);
        save_token_in(&dir, "anthropic", "  tok-123\n").unwrap();
        assert_eq!(
            load_token_in(&dir, "anthropic"),
            Some("tok-123".to_string())
        );
        assert_eq!(load_token_in(&dir, "other"), None);
    }

    #[test]
    fn token_is_stored_as_yaml() {
        let dir = tempdir();
        save_token_in(&dir, "12th", "sk-xyz").unwrap();
        let raw = std::fs::read_to_string(dir.join("12th.yaml")).unwrap();
        assert!(raw.contains("token:"), "on-disk format is yaml: {raw}");
        assert_eq!(load_token_in(&dir, "12th"), Some("sk-xyz".to_string()));
    }

    #[test]
    fn yaml_round_trips_special_chars() {
        let dir = tempdir();
        let token = r#"sk-a:b"c\d e"#;
        save_token_in(&dir, "p", token).unwrap();
        assert_eq!(load_token_in(&dir, "p"), Some(token.to_string()));
    }
}
