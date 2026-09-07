# Backend Development Guidelines

> Best practices for backend development in this project.

---

## Overview

This directory combines current backend contracts with a small number of
explicit placeholders. Guides that still need project-specific content remain
marked `To fill`; populated guides describe checked-in behavior.

---

## Guidelines Index

| Guide | Description | Status |
|-------|-------------|--------|
| [Directory Structure](./directory-structure.md) | Module organization and file layout | To fill |
| [Database Guidelines](./database-guidelines.md) | SQLite, event-log, admission, and migration contracts | Current |
| [Error Handling](./error-handling.md) | Typed store/core/tool errors and fail-closed patterns | Current |
| [Quality Guidelines](./quality-guidelines.md) | Executable backend quality and release contracts | Current |
| [Task Tool](./task-tool.md) | Single/batch validation plus bounded background spawn admission and typed overload | Documented |
| [Workflow Control](./workflow-control.md) | Event-sourced Workflow state, atomic admission, recovery ownership, surfaces, and SDK contracts | Documented |
| [Logging Guidelines](./logging-guidelines.md) | Structured logging, log levels | To fill |

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
