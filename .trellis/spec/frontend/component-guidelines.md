# Component Guidelines

> How components are built in this project.

---

## Overview

The current TUI uses SolidJS with OpenTUI under `packages/hya-tui-ts`. Extend
the existing components, contexts, dialogs, routes, command registry, and
feature-plugin slots before creating a new framework boundary.

`src/upstream/` is the retained frontend implementation. `src/hya/` contains
hya-owned product, platform, audit, static-host, and SDK-spine integration.

---

## Component Structure

- Keep rendering declarative and derive view state from existing Solid contexts.
- Keep HTTP/SSE access in the SDK and sync contexts; components should not create
  parallel clients or poll backend state.
- Keep route transitions in the route context and command actions in the command
  registry so keybindings, palette actions, and UI behavior stay aligned.
- Put repeated app-wide UI in the existing feature-plugin slots instead of
  wrapping the application in another layout system.
- Keep hya-specific boundary adaptation in `src/hya/` when it does not belong in
  the retained upstream implementation.

---

## Props and State

Use explicit props for local display inputs and existing providers for shared
runtime state. Follow Solid's accessor/store semantics; do not copy React hook
patterns or destructure reactive props in a way that loses tracking.

Validate unknown values at SDK, persistence, or route boundaries. Internal
components should consume the normalized types produced there.

---

## Styling and Accessibility

Use the existing OpenTUI primitives and semantic theme values. Important state
must have a text or symbol signal in addition to color. Preserve usable prompt,
dialog, and transcript layouts on narrow terminals.

---

## Common Mistakes

- Do not add new behavior to the retained Rust TUI for a shipped frontend change.
- Do not bypass `@opencode-ai/sdk/v2` with a second HTTP client.
- Do not duplicate synchronized server state in component-local stores.
- Do not import excluded OpenCode server, worker, updater, web, or desktop code.
- Do not edit generated logo or epilogue data by hand; use the existing asset
  generation script.
