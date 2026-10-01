# Keybinding settings entry point

## Question: What is the initial keybinding settings contract?

Answer: `/keybind` opens the binding browser; `list [workspace|conversation|pane]` filters it, and `show <action, command or shortcut>` inspects an entry. The original 0.43.41 inspection command was `/keybindings`; 0.43.42 shortens it and adds `/keybind set <shortcut> [--scope workspace|conversation] <command...>` plus `reset <shortcut|all>`. Targets are full command strings, including arguments, dispatched through the existing registry. Built-in shortcuts stay protected; custom bindings are persisted under TUI preferences `keybindings`. Modal/command input ownership and conversation focus gating precede custom command execution. Failed writes leave runtime bindings intact.

## Question: Where is the Monokai experiment preserved?

Answer: The fork's `monokai-theme` branch points to `9bac2025`. At the user's request, `ert` restores the previous default palette in `cf84b288` before adding the keybinding entry point.

## Question: Why can browser tests fail with a native bundle ABI mismatch after a version bump?

Answer: Starting the backend while its build is still running can pair the previous executable with newly built native tool bundles. Wait for `cargo build --locked -p hya-backend --bin hya` to complete before starting browser fixtures. Rust targets, build logs, and browser results are stored under `~/data` in this environment.


## Question: How does nested completion work for a command being bound?

Answer: `CommandSpec.complete(position, context, registry?)` receives its owning registry. `/keybind set` strips its own prefix, delegates argument completion to the target command, then restores the prefix. This preserves the target command's completion rules without another command-name list or hint tree.
