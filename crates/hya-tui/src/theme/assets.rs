use std::collections::BTreeMap;

use super::error::ThemeError;
use super::schema::ThemeJson;

pub const DEFAULT_THEME: &str = "hya";

struct BuiltinThemeAsset {
    name: &'static str,
    source: &'static str,
}

const BUILTIN_THEME_ASSETS: &[BuiltinThemeAsset] = &[
    BuiltinThemeAsset {
        name: "aura",
        source: include_str!("../../assets/themes/aura.json"),
    },
    BuiltinThemeAsset {
        name: "ayu",
        source: include_str!("../../assets/themes/ayu.json"),
    },
    BuiltinThemeAsset {
        name: "catppuccin",
        source: include_str!("../../assets/themes/catppuccin.json"),
    },
    BuiltinThemeAsset {
        name: "catppuccin-frappe",
        source: include_str!("../../assets/themes/catppuccin-frappe.json"),
    },
    BuiltinThemeAsset {
        name: "catppuccin-macchiato",
        source: include_str!("../../assets/themes/catppuccin-macchiato.json"),
    },
    BuiltinThemeAsset {
        name: "cobalt2",
        source: include_str!("../../assets/themes/cobalt2.json"),
    },
    BuiltinThemeAsset {
        name: "cursor",
        source: include_str!("../../assets/themes/cursor.json"),
    },
    BuiltinThemeAsset {
        name: "dracula",
        source: include_str!("../../assets/themes/dracula.json"),
    },
    BuiltinThemeAsset {
        name: "everforest",
        source: include_str!("../../assets/themes/everforest.json"),
    },
    BuiltinThemeAsset {
        name: "flexoki",
        source: include_str!("../../assets/themes/flexoki.json"),
    },
    BuiltinThemeAsset {
        name: "github",
        source: include_str!("../../assets/themes/github.json"),
    },
    BuiltinThemeAsset {
        name: "gruvbox",
        source: include_str!("../../assets/themes/gruvbox.json"),
    },
    BuiltinThemeAsset {
        name: "kanagawa",
        source: include_str!("../../assets/themes/kanagawa.json"),
    },
    BuiltinThemeAsset {
        name: "material",
        source: include_str!("../../assets/themes/material.json"),
    },
    BuiltinThemeAsset {
        name: "matrix",
        source: include_str!("../../assets/themes/matrix.json"),
    },
    BuiltinThemeAsset {
        name: "mercury",
        source: include_str!("../../assets/themes/mercury.json"),
    },
    BuiltinThemeAsset {
        name: "monokai",
        source: include_str!("../../assets/themes/monokai.json"),
    },
    BuiltinThemeAsset {
        name: "nightowl",
        source: include_str!("../../assets/themes/nightowl.json"),
    },
    BuiltinThemeAsset {
        name: "nord",
        source: include_str!("../../assets/themes/nord.json"),
    },
    BuiltinThemeAsset {
        name: "one-dark",
        source: include_str!("../../assets/themes/one-dark.json"),
    },
    BuiltinThemeAsset {
        name: "osaka-jade",
        source: include_str!("../../assets/themes/osaka-jade.json"),
    },
    BuiltinThemeAsset {
        name: "hya",
        source: include_str!("../../assets/themes/hya.json"),
    },
    BuiltinThemeAsset {
        name: "orng",
        source: include_str!("../../assets/themes/orng.json"),
    },
    BuiltinThemeAsset {
        name: "lucent-orng",
        source: include_str!("../../assets/themes/lucent-orng.json"),
    },
    BuiltinThemeAsset {
        name: "palenight",
        source: include_str!("../../assets/themes/palenight.json"),
    },
    BuiltinThemeAsset {
        name: "rosepine",
        source: include_str!("../../assets/themes/rosepine.json"),
    },
    BuiltinThemeAsset {
        name: "solarized",
        source: include_str!("../../assets/themes/solarized.json"),
    },
    BuiltinThemeAsset {
        name: "synthwave84",
        source: include_str!("../../assets/themes/synthwave84.json"),
    },
    BuiltinThemeAsset {
        name: "tokyonight",
        source: include_str!("../../assets/themes/tokyonight.json"),
    },
    BuiltinThemeAsset {
        name: "vesper",
        source: include_str!("../../assets/themes/vesper.json"),
    },
    BuiltinThemeAsset {
        name: "vercel",
        source: include_str!("../../assets/themes/vercel.json"),
    },
    BuiltinThemeAsset {
        name: "zenburn",
        source: include_str!("../../assets/themes/zenburn.json"),
    },
    BuiltinThemeAsset {
        name: "carbonfox",
        source: include_str!("../../assets/themes/carbonfox.json"),
    },
];

pub fn builtin_themes() -> Result<BTreeMap<&'static str, ThemeJson>, ThemeError> {
    let mut themes = BTreeMap::new();
    for asset in BUILTIN_THEME_ASSETS {
        themes.insert(asset.name, parse_builtin(asset)?);
    }
    Ok(themes)
}

pub fn builtin_theme(name: &str) -> Result<Option<ThemeJson>, ThemeError> {
    match BUILTIN_THEME_ASSETS.iter().find(|asset| asset.name == name) {
        Some(asset) => parse_builtin(asset).map(Some),
        None => Ok(None),
    }
}

#[must_use]
pub fn builtin_theme_source(name: &str) -> Option<&'static str> {
    BUILTIN_THEME_ASSETS
        .iter()
        .find(|asset| asset.name == name)
        .map(|asset| asset.source)
}

fn parse_builtin(asset: &BuiltinThemeAsset) -> Result<ThemeJson, ThemeError> {
    serde_json::from_str(asset.source).map_err(|source| ThemeError::ParseBuiltin {
        name: asset.name,
        source,
    })
}
