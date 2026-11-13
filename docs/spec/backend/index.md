# Backend Development Guidelines

> Best practices for backend development in this project.

---

## Overview

These guidelines capture executable backend invariants across storage, error
handling, quality gates, subagent spawning, and Workflow control. For the crate
and directory map, see [Project Structure](../../project-structure.md).

---

## Guidelines Index

| Guide | Description | Status |
|-------|-------------|--------|
| [Project Structure](../../project-structure.md) | Workspace crate layout, 28 builtin tools, 12 first-party bundles, and dependency DAG | Current |
| [Database Guidelines](./database-guidelines.md) | SQLite, event-log, admission, and migration contracts | Current |
| [Error Handling](./error-handling.md) | Typed store/core/tool errors and fail-closed patterns | Current |
| [Quality Guidelines](./quality-guidelines.md) | Executable backend quality and release contracts | Current |
| [Task Tool](./task-tool.md) | Single/batch validation plus bounded background spawn admission and typed overload | Current |
| [Workflow Control](./workflow-control.md) | Event-sourced Workflow state, atomic admission, recovery ownership, surfaces, and SDK contracts | Current |

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
