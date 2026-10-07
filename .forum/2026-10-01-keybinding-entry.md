# Keybinding settings entry point

## Question: What is the initial keybinding settings contract?

Answer: `/keybind` opens the binding browser; `list [workspace|conversation|pane]` filters it, and `show <action, command or shortcut>` inspects an entry. The original 0.43.41 inspection command was `/keybindings`; 0.43.42 shortens it and adds `/keybind set <shortcut> [--scope workspace|conversation] <command...>` plus `reset <shortcut|all>`. Targets are full command strings, including arguments, dispatched through the existing registry. Built-in shortcuts stay protected; custom bindings are persisted under TUI preferences `keybindings`. Modal/command input ownership and conversation focus gating precede custom command execution. Failed writes leave runtime bindings intact.

## Question: Where is the Monokai experiment preserved?

Answer: The fork's `monokai-theme` branch points to `9bac2025`. At the user's request, `ert` restores the previous default palette in `cf84b288` before adding the keybinding entry point.

## Question: Why can browser tests fail with a native bundle ABI mismatch after a version bump?

Answer: Starting the backend while its build is still running can pair the previous executable with newly built native tool bundles. Wait for `cargo build --locked -p hya-backend --bin hya` to complete before starting browser fixtures. Rust targets, build logs, and browser results are stored under `~/data` in this environment.


## Question: How does nested completion work for a command being bound?

Answer: `CommandSpec.complete(position, context, registry?)` receives its owning registry. `/keybind set` strips its own prefix, delegates argument completion to the target command, then restores the prefix. This preserves the target command's completion rules without another command-name list or hint tree.

## Question: Which shortcuts are assigned by default after the essentials change?

Answer: Defaults cover commands/help, pane focus, transcript scroll, cancellation and exit, plus composer editing. Ctrl+R/B/P/O/G, F4, global Shift+Tab mode cycling and editor/undo/redo/fork chords are unassigned. Action catalog rows remain with empty keys so scope inference and inspection continue working. `/pending` preserves the former F4 cross-session review path. Local picker, prompt, confirmation and opt-in Vim keys remain local.

## Question: What is the single-shortcut removal command?

Answer: `/keybind unset <shortcut>` removes a custom assignment; reset <shortcut> stays compatible, and reset all clears the custom map. Unset completes only assigned keys, normalizes modifier names and saves before applying. It cannot disable protected defaults. Ctrl+Home/End are no longer app defaults and can be assigned; plain Home/End retain transcript navigation with empty input.

## Question: How should keybinding lists distinguish keys from descriptions?

Answer: List only assigned defaults and custom bindings. Render Shortcut, Action / command and Scope as opt-in picker columns, and reserve the detail pane for descriptions/full commands. Keep unassigned action metadata for `/keybind show` and scope inference. `PickerRow.shortcut` is searchable; `PickerSpec.columns` supplies headings without changing other pickers.

## Question: What is the minimal visible override mechanism as of 0.43.46?

Answer: `keybindings` entries are command/scope objects (assigned) or null (disabled); absent entries use defaults. Set replaces defaults, unset consumes that physical key before app/editor/context handling, reset restores defaults. Browser and Ctrl+I/M/J/H/Ctrl+Shift bans are gone. Legacy control aliases match their physical Tab/Enter/linefeed/Backspace representations; explicit alias overrides replace each other. Command input keeps its administrative keys and is shown as an explicit context in the inventory. Show without a target surfaces a usage error, and list/show include inherited OpenTUI editor and contextual keys.
