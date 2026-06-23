#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingsView {
    pub title: String,
    pub groups: Vec<KeyBindingGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingGroup {
    pub label: String,
    pub items: Vec<KeyBindingItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingItem {
    pub key: String,
    pub label: String,
}
