# Fix batch G6 - stale source line anchors in `agent-tool-surface.md`

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your file

- `docs/architecture/agent-tool-surface.md`

Do not create or edit any other file.

## The problem

This document cites source locations as `file:line` — **157 of them**, more than
every other document in the repository combined. Example shapes:

```
([crates/hya-tool/src/tool.rs:262-264](../../crates/hya-tool/src/tool.rs#L262-L264))
`crates/hya-tool/src/read.rs:41-52`
```

A recent pass added doc comments throughout `crates/`, which **shifted line
numbers in nearly every cited file**. `crates/hya-app/src/runtime.rs` went from
14924 to 14975 lines; `crates/hya-core/src/completion.rs` from 279 to 341. So most
of these anchors now point at unrelated code.

An independent audit confirmed specific examples:

| Anchor in the doc | What it claims to point at | Where the item actually is now |
| --- | --- | --- |
| `tool.rs:237-271` | `builtins()` | 313-345 |
| `tool.rs:983-1037` | `AskUserTool` | 1059-1128 |
| `tool.rs:130` | `SEARCH_LIMIT` | 180 |
| `tool.rs:~626` | `builtin_permission` | 626-635 |

## Your job

For **every** `file:line` or `file:line-line` citation in this document:

1. Identify which **symbol or behaviour** the citation is supporting. It is
   almost always named in the surrounding sentence (a struct, a function, a
   constant, a schema field).
2. Find that symbol's **current** location in the source file.
3. Update the line number, and update the matching `#L...` fragment in the
   markdown link so the two agree.

If you cannot determine what a citation was pointing at, **remove the line number
and cite the file alone** (`crates/hya-tool/src/tool.rs`). A correct file
reference beats a precise-looking wrong one — that is the whole failure being
fixed here. Report every citation you had to degrade this way.

## Non-negotiable rules

1. Verify each symbol's location by actually reading the file. Do not compute an
   offset and apply it to every anchor — different files shifted by different
   amounts, and some did not shift at all.
2. Change **only** line numbers and their `#L` fragments. Do not reword the
   surrounding prose, do not add or remove sections, and do not "improve"
   anything else in this file. Another writer may be editing content here later;
   your diff should be almost entirely digits.
3. Where the linked path itself is wrong (the file moved or was renamed), fix the
   path too and say so in your report.
4. Do not run `git commit`.

## When you are done

Report:

1. How many citations you checked, how many you corrected, and how many were
   already right.
2. How many you degraded to a file-only reference, and which ones.
3. Any citation whose target symbol no longer exists at all — that is a doc
   correctness problem beyond line drift, so name it rather than guessing.
