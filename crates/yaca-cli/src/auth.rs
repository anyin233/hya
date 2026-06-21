use std::path::{Path, PathBuf};

pub fn auth_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("yaca/auth"))
}

pub fn save_token_in(dir: &Path, provider: &str, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{provider}.token")), token.trim())
}

#[must_use]
pub fn load_token_in(dir: &Path, provider: &str) -> Option<String> {
    let content = std::fs::read_to_string(dir.join(format!("{provider}.token"))).ok()?;
    let token = content.trim().to_string();
    (!token.is_empty()).then_some(token)
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
}
