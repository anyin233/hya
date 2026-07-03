# Implementation plan

1. Add a controller test that types `/themes`, expects a theme dialog, selects a non-default theme, and asserts `app.theme` changed. Run the targeted test and confirm it fails.
2. Add `ThemeId`, catalog helpers, and `AppState.theme` in `hya-legacy-tui`.
3. Change `draw` to use `Theme::for_id(&state.theme)` or equivalent.
4. Add slash command registration/completion/help for `/themes`.
5. Add controller dialog handling for `DialogMode::Theme`.
6. Add or update a render test proving a selected theme affects at least one stable color token.
7. Bump release metadata to `0.29.5` and archive current root changelog to `docs/changes/CHANGELOG_0.29.2.md` in this branch.
8. Run:

```sh
cargo test -p hya-backend tui::controller::tests::slash_themes_opens_theme_dialog
cargo test -p hya-legacy-tui
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

9. Commit with `feat(tui): add theme picker`.
