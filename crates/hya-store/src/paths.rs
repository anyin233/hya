//! Per-user directories hya keeps data in outside the session database.

use std::path::PathBuf;

/// hya's per-user cache directory: `$XDG_CACHE_HOME/hya`, else
/// `$HOME/.cache/hya`; `None` when neither variable is set (an empty value
/// counts as unset). Holds the remote-model cache (`model_cache.db`) and the
/// scratch directories of temporary sessions (`scratch/<session id>`,
/// ADR-0024).
#[must_use]
pub fn user_cache_dir() -> Option<PathBuf> {
    cache_dir_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

fn cache_dir_from(
    xdg_cache_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = xdg_cache_home.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir).join("hya"));
    }
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".cache/hya"))
}

#[cfg(test)]
mod tests {
    use super::cache_dir_from;
    use std::path::PathBuf;

    #[test]
    fn xdg_cache_home_wins_then_home_then_none() {
        assert_eq!(
            cache_dir_from(Some("/xdg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/xdg/hya"))
        );
        assert_eq!(
            cache_dir_from(Some("".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.cache/hya"))
        );
        assert_eq!(cache_dir_from(None, Some("".into())), None);
        assert_eq!(cache_dir_from(None, None), None);
    }
}
