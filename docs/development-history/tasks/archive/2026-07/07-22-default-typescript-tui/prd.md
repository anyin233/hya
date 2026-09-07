# Make TypeScript TUI the Default

## Goal

Make the shipped `hya` command open the existing TypeScript terminal UI, with no installed or supported route to the Rust TUI.

## Background

- The shipped `hya` binary currently starts the Rust TUI directly.
- The shipped `hya-ts` launcher already starts the TypeScript TUI, owns or attaches to `hya-backend`, forwards session/model startup options, and restores terminal state.
- Source installs and release archives already include `hya`, `hya-backend`, `hya-ts`, and the prepared TypeScript runtime.
- Bare `hya-backend` currently starts an in-process server and launches `hya` as its frontend.

## Requirements

- Bare `hya` must launch the TypeScript TUI using the existing `hya-ts` runtime and backend-lifecycle behavior.
- Bare `hya-backend` must also reach the TypeScript TUI and must not reopen the Rust TUI.
- Existing TypeScript launcher capabilities for project selection, attached servers, session continuation/forking, prompts, agents, models, auth forwarding, signal handling, exit status, and terminal restoration must remain available through the default entrypoint.
- Canonical `hya` must use `hya` help and error branding; direct `hya-ts` invocation must retain its existing alias branding and CLI behavior.
- The shipped non-TUI `hya --import compat` command must remain available through the shared launcher CLI without retaining the Rust TUI path.
- Source installs and release archives must provide a working default `hya` plus its prepared TypeScript runtime.
- Default startup may require Bun and must report a direct launch error when Bun is unavailable; it must not silently fall back to the deprecated Rust UI.
- The Rust TUI must have no installed or supported executable entrypoint (`hya-rust` and `hya --rust` are explicitly excluded) and must no longer be described or selected as the current/default frontend.
- The existing `hya-ts` command must remain as a compatibility alias for its shipped users.
- Rust-frontend-only options are removed with that frontend; the canonical command adopts the existing `hya-ts` option names, including `--session` for direct session startup.
- The change must update the workspace version and newest root changelog according to the release rules.

## Acceptance Criteria

- [x] Invoking shipped `hya` without a subcommand starts the TypeScript TUI path.
- [x] Invoking shipped `hya-ts` retains the same TypeScript launcher behavior for compatibility.
- [x] Help, version, and launch errors identify the invoked `hya` or `hya-ts` entrypoint correctly.
- [x] Invoking bare `hya-backend` starts the TypeScript TUI path and cannot recurse into or select the Rust TUI.
- [x] Default `hya` preserves the tested TypeScript launcher process, backend ownership, signal, exit-status, and terminal-restoration behavior.
- [x] `hya --import compat` retains its existing config-import behavior after the frontend migration.
- [x] Installer and release-package checks prove that `hya` and the prepared TypeScript runtime are colocated and runnable.
- [x] User-facing CLI and architecture documentation identify the TypeScript TUI as default and the Rust TUI as deprecated.
- [x] Rust, Bun/TypeScript, installer, and release-package verification gates pass for the changed surfaces.

## Out Of Scope

- Rewriting TypeScript TUI behavior that the existing `hya-ts` launcher already provides.
- Removing backend headless commands or server APIs.
- Removing the deprecated Rust TUI source crates wholesale; this migration removes their executable/default startup path.
- Supporting standalone `cargo install --path crates/hya`, which cannot install the required sibling launcher and prepared runtime.
