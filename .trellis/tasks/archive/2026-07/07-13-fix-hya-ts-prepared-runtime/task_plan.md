# Task Plan

## Goal

Repair the already-pushed `0.33.0` prepared runtime without widening the TUI
migration.

## Phases

- [complete] Add focused staged-runtime and installer regressions; observe RED.
- [complete] Remap the pinned SDK v2 export to its client and ship runtime config.
- [complete] Bump to `0.33.1`, run all gates, independently review, commit, push, and archive.

## Constraints

- Keep Bun as a system prerequisite and the TypeScript package frontend-only.
- Do not rewrite or revert the pushed `0.33.0` commits.
- Preserve unrelated dirty workspace files.
