# 0.43.46

- Make keybinding overrides explicit: set replaces defaults, unset disables keys, reset restores defaults, with persisted null overrides.
- Remove blanket browser, Ctrl+I/M/J/H and Ctrl+Shift assignment bans; match legacy terminal aliases.
- Expose inherited editor and contextual keys in the binding list and shortcut inspection.
- Show a visible usage error when `/keybind show` has no target.
