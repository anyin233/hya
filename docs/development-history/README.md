# Development History

These task artifacts and developer journals are preserved historical evidence.
They do not configure the current agent workflow, and their old commands,
statuses, context manifests, and approval rules are not current instructions.
Task management now uses `planning-with-files`; see [development guidance](../development.md)
and the repository's `AGENTS.md`.

## Preserved records

- `tasks/`: prior task directories, including the original `archive/` hierarchy.
- `workspace/`: prior developer journals and indexes.

Original contents are retained. Resolve historical `.trellis/tasks/` and
`.trellis/workspace/` references to `docs/development-history/tasks/` and
`docs/development-history/workspace/`, respectively. Project coding guidelines
formerly under `.trellis/spec/` now live in [`docs/spec/`](../spec/).
References to removed workflow scripts describe the former tooling, not an
instruction to restore or execute it.

An unarchived task directory is not evidence that work remains active. When a
user resumes old work, inspect its recorded state and create or reuse the relevant
`.planning/<YYYY-MM-DD-slug>/` plan without changing the historical record.
