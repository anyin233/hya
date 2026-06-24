#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Home {
        prompt: Option<String>,
    },
    Session {
        session_id: String,
        prompt: Option<String>,
    },
    Plugin {
        id: String,
        data: Option<serde_json::Value>,
    },
}

impl Default for Route {
    fn default() -> Self {
        Self::Home { prompt: None }
    }
}
