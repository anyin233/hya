use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tool::Decision;
use yaca_tui::{AppState, PermissionPrompt, PermissionPromptStage};

pub(super) fn handle_permission_key(key: KeyEvent, app: &mut AppState) -> Option<Decision> {
    let prompt = app.permission.as_mut()?;
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Some(Decision::Reject { feedback: None });
    }
    match key.code {
        KeyCode::Esc if prompt.stage == PermissionPromptStage::Always => {
            prompt.stage = PermissionPromptStage::Permission;
            prompt.selected = 1;
            None
        }
        KeyCode::Esc => Some(Decision::Reject { feedback: None }),
        KeyCode::Enter
            if prompt.stage == PermissionPromptStage::Permission && prompt.selected == 1 =>
        {
            prompt.stage = PermissionPromptStage::Always;
            prompt.selected = 0;
            None
        }
        KeyCode::Enter if prompt.stage == PermissionPromptStage::Always && prompt.selected == 1 => {
            prompt.stage = PermissionPromptStage::Permission;
            prompt.selected = 1;
            None
        }
        KeyCode::Enter => Some(decision_from(prompt)),
        KeyCode::Left | KeyCode::Char('h') => {
            prompt.selected = previous_option(prompt);
            None
        }
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
            prompt.selected = next_option(prompt);
            None
        }
        KeyCode::Backspace if prompt.stage == PermissionPromptStage::Permission => {
            prompt.reply.pop();
            None
        }
        KeyCode::Char(c) => {
            if prompt.stage == PermissionPromptStage::Permission
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            {
                prompt.reply.push(c);
            }
            None
        }
        _ => None,
    }
}

fn previous_option(prompt: &PermissionPrompt) -> usize {
    let len = prompt.options().len();
    if len == 0 {
        return 0;
    }
    (prompt.selected + len - 1) % len
}

fn next_option(prompt: &PermissionPrompt) -> usize {
    let len = prompt.options().len();
    if len == 0 {
        return 0;
    }
    (prompt.selected + 1) % len
}

fn decision_from(prompt: &PermissionPrompt) -> Decision {
    match prompt.stage {
        PermissionPromptStage::Permission => match prompt.selected {
            0 => Decision::AllowOnce,
            1 => Decision::AllowAlways,
            _ => Decision::Reject {
                feedback: (!prompt.reply.trim().is_empty()).then(|| prompt.reply.clone()),
            },
        },
        PermissionPromptStage::Always => Decision::AllowAlways,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_permission(selected: usize, stage: PermissionPromptStage) -> AppState {
        AppState {
            permission: Some(PermissionPrompt {
                title: "bash".to_string(),
                detail: "rm -rf /tmp/x".to_string(),
                selected,
                reply: String::new(),
                stage,
            }),
            ..AppState::default()
        }
    }

    #[test]
    fn allow_always_waits_for_confirmation() {
        // Given: the OpenCode persistent-allow option is selected.
        let mut app = app_with_permission(1, PermissionPromptStage::Permission);

        // When: the user confirms once.
        let decision = handle_permission_key(key(KeyCode::Enter), &mut app);

        // Then: yaca opens the confirmation stage before sending the decision.
        let prompt = app.permission.as_ref();
        assert_eq!(decision, None);
        assert!(matches!(
            prompt,
            Some(prompt)
                if prompt.stage == PermissionPromptStage::Always && prompt.selected == 0
        ));

        let decision = handle_permission_key(key(KeyCode::Enter), &mut app);
        assert_eq!(decision, Some(Decision::AllowAlways));
    }

    #[test]
    fn allow_always_escape_returns_to_permission_stage() {
        // Given: the persistent-allow confirmation stage is open.
        let mut app = app_with_permission(0, PermissionPromptStage::Always);

        // When: the user cancels with Escape.
        let decision = handle_permission_key(key(KeyCode::Esc), &mut app);

        // Then: yaca returns to the original permission choice row.
        let prompt = app.permission.as_ref();
        assert_eq!(decision, None);
        assert!(matches!(
            prompt,
            Some(prompt)
                if prompt.stage == PermissionPromptStage::Permission && prompt.selected == 1
        ));
    }

    #[test]
    fn vim_keys_cycle_permission_options_like_opencode() {
        // Given: the first permission option is selected.
        let mut app = app_with_permission(0, PermissionPromptStage::Permission);

        // When: the user presses h, matching OpenCode's previous-option binding.
        let decision = handle_permission_key(key(KeyCode::Char('h')), &mut app);

        // Then: selection wraps to the final option.
        assert_eq!(decision, None);
        assert_eq!(
            app.permission.as_ref().map(|prompt| prompt.selected),
            Some(2)
        );

        // When: the user presses l, matching OpenCode's next-option binding.
        let decision = handle_permission_key(key(KeyCode::Char('l')), &mut app);

        // Then: selection wraps back to the first option.
        assert_eq!(decision, None);
        assert_eq!(
            app.permission.as_ref().map(|prompt| prompt.selected),
            Some(0)
        );
    }
}
