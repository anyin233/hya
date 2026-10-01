# Keybinding settings entry point

## Question: What is the initial keybinding settings contract?

Answer: `/keybindings` opens the action browser; `list [workspace|conversation|pane]` filters it, and `show <action or command>` inspects an action. This first step is read-only. Shortcut labels and descriptions come from `src/keys/bindings.ts`; `src/keys/catalog.ts` adds stable action IDs, related commands and routing context. Modal ownership and pane dispatch remain in the existing router. Future editing should extend this entry point instead of adding a competing key dispatcher.

## Question: Where is the Monokai experiment preserved?

Answer: The fork's `monokai-theme` branch points to `9bac2025`. At the user's request, `ert` restores the previous default palette in `cf84b288` before adding the keybinding entry point.

## Question: Why can browser tests fail with a native bundle ABI mismatch after a version bump?

Answer: Starting the backend while its build is still running can pair the previous executable with newly built native tool bundles. Wait for `cargo build --locked -p hya-backend --bin hya` to complete before starting browser fixtures. Rust targets, build logs, and browser results are stored under `~/data` in this environment.
