use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeJson {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default)]
    pub defs: BTreeMap<String, String>,
    pub theme: BTreeMap<String, ThemeValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThemeValue {
    Variant(ThemeVariant),
    String(String),
    Number(serde_json::Number),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeVariant {
    pub dark: Box<ThemeValue>,
    pub light: Box<ThemeValue>,
}

impl ThemeVariant {
    pub(crate) fn get(&self, mode: Mode) -> &ThemeValue {
        match mode {
            Mode::Dark => &self.dark,
            Mode::Light => &self.light,
        }
    }
}
