mod footer;
mod model;
mod render;
mod state;

pub use render::{draw, render_lines};
pub use state::{QuestionAction, QuestionState};

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;

    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};

    use super::{draw, render_lines, QuestionAction, QuestionState};

    fn request() -> serde_json::Value {
        json!({
            "id": "que_1",
            "sessionID": "ses_1",
            "questions": [
                {
                    "header": "Mode",
                    "question": "Which mode?",
                    "options": [
                        { "label": "Fast", "description": "Quick answer" },
                        { "label": "Careful", "description": "More checks" }
                    ],
                    "multiple": false,
                    "custom": true
                },
                {
                    "header": "Tags",
                    "question": "Pick tags",
                    "options": [
                        { "label": "Rust", "description": "Rust code" },
                        { "label": "TUI", "description": "Terminal UI" }
                    ],
                    "multiple": true,
                    "custom": true
                }
            ]
        })
    }

    #[test]
    fn question_state_collects_single_multi_custom_and_confirm_answers() {
        let mut state = QuestionState::new("que_1");

        assert_eq!(state.select(&request()), QuestionAction::None);
        assert_eq!(state.tab(), 1);
        assert_eq!(state.answers(2), vec![vec!["Fast".to_owned()], vec![]]);

        state.set_selected(1);
        assert_eq!(state.select(&request()), QuestionAction::None);
        assert_eq!(state.answers(2)[1], vec!["TUI".to_owned()]);

        state.set_selected(2);
        assert_eq!(state.select(&request()), QuestionAction::None);
        assert!(state.editing());
        state.push_custom_char('d');
        state.push_custom_char('x');
        assert_eq!(state.save_custom(&request()), QuestionAction::None);
        assert_eq!(state.answers(2)[1], vec!["TUI".to_owned(), "dx".to_owned()]);

        state.next_tab(&request());
        assert_eq!(
            state.submit(&request()),
            QuestionAction::Reply {
                answers: vec![
                    vec!["Fast".to_owned()],
                    vec!["TUI".to_owned(), "dx".to_owned()]
                ],
            }
        );
    }

    #[test]
    fn question_overlay_renders_question_and_options() {
        let request = request();
        let state = QuestionState::new("que_1");
        let theme = resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap();
        let lines = render_lines(&request, &state, &theme);
        let text = lines
            .iter()
            .flat_map(|line| line.0.iter())
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(text.contains("Which mode?"));
        assert!(text.contains("Fast"));
        assert!(text.contains("Careful"));
        assert!(text.contains("Type your own answer"));

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &request, &state, &theme))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Which mode?"));
        assert!(rendered.contains("Fast"));
    }
}
