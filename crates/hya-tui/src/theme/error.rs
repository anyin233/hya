#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("failed to parse builtin theme {name}: {source}")]
    ParseBuiltin {
        name: &'static str,
        source: serde_json::Error,
    },
    #[error("theme key \"{key}\" is missing")]
    MissingThemeKey { key: &'static str },
    #[error("color reference \"{name}\" not found in defs or theme")]
    MissingReference { name: String },
    #[error("circular color reference: {chain}")]
    CircularReference { chain: String },
    #[error("invalid hex color \"{value}\"")]
    InvalidHex { value: String },
    #[error("invalid ANSI color code {value}")]
    InvalidAnsi { value: String },
    #[error("theme key \"{key}\" must be a number")]
    InvalidNumber { key: &'static str },
}
