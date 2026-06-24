use hya_tui::state::{AppState, Route};

#[test]
fn navigate_replaces_current_route_with_session_route() {
    let mut state = AppState::default();
    let next = Route::Session {
        session_id: "ses_test".into(),
        prompt: Some("resume".into()),
    };

    state.navigate(next.clone());

    assert_eq!(state.route, next);
}
