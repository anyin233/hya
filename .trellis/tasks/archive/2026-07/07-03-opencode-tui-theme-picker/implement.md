# Implementation record

- [x] Add command-routing coverage and observe `/theme` and `/themes` return no built-in route.
- [x] Register both commands and route them through the existing `theme.switch` dialog.
- [x] Mark and preselect the current theme, then apply a different selection immediately.
- [x] Add a TUI harness test covering open, current selection, navigation, and application.
- [x] Update sequential version metadata to `0.33.13` and archive `0.33.12` changelog notes.
- [x] Run targeted tests, the full Rust CI-equivalent gate, and local executable builds.
- [x] Complete the reviewed PR at `1bf4aa8b` and safely push stacked PR #8 after fetching its target branches.
