use crossterm::event::{KeyCode, KeyEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectedBlockActionKind {
    Revert,
    Branch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedBlockAction {
    pub kind: SelectedBlockActionKind,
    pub message_index: usize,
}

#[must_use]
pub fn selected_block_action(
    selected_message: Option<usize>,
    input: &str,
    key: &KeyEvent,
) -> Option<SelectedBlockAction> {
    if !input.is_empty() || !key.modifiers.is_empty() {
        return None;
    }
    let message_index = selected_message?;
    let kind = match key.code {
        KeyCode::Char('r') => SelectedBlockActionKind::Revert,
        KeyCode::Char('b') => SelectedBlockActionKind::Branch,
        _ => return None,
    };
    Some(SelectedBlockAction {
        kind,
        message_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn selected_block_keys_map_to_actions_when_prompt_is_empty() {
        assert_eq!(
            selected_block_action(Some(2), "", &key(KeyCode::Char('r'))),
            Some(SelectedBlockAction {
                kind: SelectedBlockActionKind::Revert,
                message_index: 2,
            })
        );
        assert_eq!(
            selected_block_action(Some(1), "", &key(KeyCode::Char('b'))),
            Some(SelectedBlockAction {
                kind: SelectedBlockActionKind::Branch,
                message_index: 1,
            })
        );
    }

    #[test]
    fn selected_block_keys_do_not_steal_typing_or_unselected_input() {
        assert_eq!(
            selected_block_action(Some(0), "draft", &key(KeyCode::Char('r'))),
            None
        );
        assert_eq!(
            selected_block_action(None, "", &key(KeyCode::Char('b'))),
            None
        );
        assert_eq!(
            selected_block_action(
                Some(0),
                "",
                &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
            ),
            None
        );
    }
}
