---
name: zvec-grep
description: Semantic workspace search with zvec-grep when exact wording or location is unknown.
---

Use the `zvec_grep_search` tool (exposed as
`zvec-grep__mcp__zvec-grep__zvec_grep_search`) for a workspace-grounded
question when the wording or location of the answer is unknown, or the request needs semantic,
fuzzy, relationship, chronology, causality, comparison, or cross-file
synthesis. Prefer native `grep`/`rg` instead when locating an exact word,
quotation, name, date, key, filename, path, source fragment, or regex is
sufficient — semantic search adds latency and imprecision for those.

Always pass an **absolute** `root` equal to the current workdir; the server
requires it. Start broad with one `query`, then narrow with `fts`, `globs`,
or `fileTypes` if the first pass is noisy. For a mixed task, run
`zvec_grep_search` first, then fall back to `grep`/`rg` for focused,
exact-anchor follow-up.

If the response reports the index is missing or stale, do **not** try to
build, refresh, or drop anything yourself — this bundle's default toolset
only exposes search. Tell the user the workspace has no (or a stale) index
and ask them to run `zg index` in the project root, then retry the search.
Never attempt to delete or drop an index; that capability is intentionally
not available here.
