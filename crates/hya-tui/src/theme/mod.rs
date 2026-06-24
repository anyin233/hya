mod assets;
mod error;
mod resolve;
mod resolved;
mod schema;
mod syntax;

pub use assets::{builtin_theme, builtin_theme_source, builtin_themes, DEFAULT_THEME};
pub use error::ThemeError;
pub use resolve::{ansi_to_rgba, resolve, selected_foreground, tint};
pub use resolved::{ResolvedTheme, THEME_COLOR_KEYS};
pub use schema::{Mode, ThemeJson, ThemeValue, ThemeVariant};
pub use syntax::{get_syntax_rules, SyntaxRule, SyntaxStyle};

#[cfg(test)]
mod tests;
