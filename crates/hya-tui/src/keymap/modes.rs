#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum KeymapMode {
    #[default]
    Base,
    Modal,
    Autocomplete,
    Question,
    Custom(String),
}

impl KeymapMode {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Base => "base",
            Self::Modal => "modal",
            Self::Autocomplete => "autocomplete",
            Self::Question => "question",
            Self::Custom(name) => name,
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "base" => Self::Base,
            "modal" => Self::Modal,
            "autocomplete" => Self::Autocomplete,
            "question" => Self::Question,
            other => Self::Custom(other.to_owned()),
        }
    }
}

impl From<&str> for KeymapMode {
    fn from(value: &str) -> Self {
        Self::from_name(value)
    }
}
