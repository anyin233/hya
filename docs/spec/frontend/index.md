# Frontend Development Guidelines

> Best practices for frontend development in this project.

---

## Overview

This directory contains guidelines for frontend development.

> **Status (v1 API consolidation).** The current `packages/hya-tui-ts`
> frontend is the vendored legacy TUI: its backend integration was built on
> `@opencode-ai/sdk/v2` against the deleted Compat HTTP surface, so it is
> deliberately broken at runtime and will be replaced by a new TUI built on
> [`hya-sdk-v1`](../../../crates/hya-sdk-v1) and the `hya.v1` contract
> ([protocol guide](../../protocol/README.md)). Until that replacement lands,
> treat the guides below as the design reference for the retained package's
> rendering/interaction contracts; any guideline that pins `@opencode-ai/sdk`
> imports or names deleted endpoints (`/tui/*`, `/permission`, `/question`,
> `/config/providers`, `/session/{id}/tree`) documents the retired integration,
> not a working target. New backend integration code must go through
> `hya-sdk-v1`.

---

## Guidelines Index

| Guide | Description | Status |
|-------|-------------|--------|
| [Directory Structure](./directory-structure.md) | Module organization and file layout | Current |
| [Component Guidelines](./component-guidelines.md) | Component patterns, props, composition | Current |
| [Hook Guidelines](./hook-guidelines.md) | Custom hooks, data fetching patterns | To fill |
| [State Management](./state-management.md) | Local state, global state, server state | To fill |
| [Quality Guidelines](./quality-guidelines.md) | Code standards, forbidden patterns | Current |
| [Workflow Presentation](./workflow-presentation.md) | Event-driven Session Workflow sidebar, validation, lifecycle, and PTY contracts | Documented |
| [Type Safety](./type-safety.md) | Type patterns, validation | To fill |

---

## How to Fill These Guidelines

For each guideline file:

1. Document your project's **actual conventions** (not ideals)
2. Include **code examples** from your codebase
3. List **forbidden patterns** and why
4. Add **common mistakes** your team has made

The goal is to help AI assistants and new team members understand how YOUR project works.

---

**Language**: All documentation should be written in **English**.
