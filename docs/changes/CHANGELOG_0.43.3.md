# 0.43.3

## Remember the TUI permission mode

- The OpenTUI frontend saves a successfully selected permission mode in its local `tui.json` preferences and applies it to new sessions, including after a restart.
- Existing sessions keep their backend-stored permission mode; canceled or rejected changes do not replace the saved default.
