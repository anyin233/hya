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
        KeyCode::Esc if prompt.stage == PermissionPromptStage::Reject => {
            prompt.stage = PermissionPromptStage::Permission;
            prompt.selected = 2;
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
        KeyCode::Enter
            if prompt.stage == PermissionPromptStage::Permission && prompt.selected == 2 =>
        {
            prompt.stage = PermissionPromptStage::Reject;
            None
        }
        KeyCode::Enter if prompt.stage == PermissionPromptStage::Always && prompt.selected == 1 => {
            prompt.stage = PermissionPromptStage::Permission;
            prompt.selected = 1;
            None
        }
        KeyCode::Enter => Some(decision_from(prompt)),
        KeyCode::Left | KeyCode::Char('h') => {
            if prompt.stage != PermissionPromptStage::Reject {
                prompt.selected = previous_option(prompt);
            }
            None
        }
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
            if prompt.stage != PermissionPromptStage::Reject {
                prompt.selected = next_option(prompt);
            }
            None
        }
        KeyCode::Backspace if prompt.stage == PermissionPromptStage::Reject => {
            prompt.reply.pop();
            None
        }
        KeyCode::Char(c) => {
            if prompt.stage == PermissionPromptStage::Reject
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
        PermissionPromptStage::Reject => Decision::Reject {
            feedback: (!prompt.reply.trim().is_empty()).then(|| prompt.reply.clone()),
        },
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
    fn reject_waits_for_feedback_stage() {
        // Given: the OpenCode reject option is selected on the permission stage.
        let mut app = app_with_permission(2, PermissionPromptStage::Permission);

        // When: the user confirms the reject option.
        let decision = handle_permission_key(key(KeyCode::Enter), &mut app);

        // Then: yaca opens the dedicated reject feedback stage before sending a decision.
        let prompt = app.permission.as_ref();
        assert_eq!(decision, None);
        assert!(matches!(
            prompt,
            Some(prompt) if prompt.stage == PermissionPromptStage::Reject
        ));
    }

    #[test]
    fn reject_feedback_confirms_or_cancels_like_opencode() {
        // Given: the dedicated reject feedback stage is open.
        let mut app = app_with_permission(0, PermissionPromptStage::Reject);

        // When: the user types feedback and confirms it.
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('u')), &mut app),
            None
        );
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('s')), &mut app),
            None
        );
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('e')), &mut app),
            None
        );
        let decision = handle_permission_key(key(KeyCode::Enter), &mut app);

        // Then: yaca sends the reject decision with the feedback text.
        assert_eq!(
            decision,
            Some(Decision::Reject {
                feedback: Some("use".to_string())
            })
        );

        // When: the user cancels the reject feedback stage.
        let mut app = app_with_permission(0, PermissionPromptStage::Reject);
        let decision = handle_permission_key(key(KeyCode::Esc), &mut app);

        // Then: yaca returns to the reject option on the permission stage.
        let prompt = app.permission.as_ref();
        assert_eq!(decision, None);
        assert!(matches!(
            prompt,
            Some(prompt)
                if prompt.stage == PermissionPromptStage::Permission && prompt.selected == 2
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
