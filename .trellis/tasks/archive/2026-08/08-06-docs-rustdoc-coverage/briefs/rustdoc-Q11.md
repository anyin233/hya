# Rustdoc Q11 - `hya-sdk`, `hya-client`

You are writing Rust API documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your batch

Crate(s): `hya-sdk`, `hya-client`. **0 undocumented public items.** Do not touch any other
crate -- other writers are working in parallel.

`hya-sdk`'s `//!` is one line and does not relate the crate to `hya-server`/`hya-native` -- expand it. `hya-client`'s `//!` has a stale consumer clause -- rewrite it.

**`hya-sdk/src/reducer.rs` has a STALE module doc**: it calls `apply` a 'no-op skeleton' and points at a 'W2 deliverable', but `apply` at `reducer.rs:264` has a full match body. Correct it -- do not merely add to it.

For `hya-client`, document `Client`, `ClientError`, `Client::new`, `create_session`, `prompt`, and `events` -- `events` needs its error and stream-termination semantics stated.

NOTE: `reducer.rs`, `store.rs`, and `types.rs` have unrelated uncommitted edits inside their `mod tests` blocks. Leave those alone; only touch doc comments.

## Non-negotiable rules

1. **Read the item before documenting it.** A doc comment that restates the item
   name adds nothing. Say what it is FOR, and for a function, its parameters and
   what it returns.
2. **Do not change code.** No signature, visibility, derive, or behaviour changes.
   Only `///`, `//!`, and `#[doc]` comments. If an item looks like it should not be
   public, leave it public and say so in your report.
3. **Do not run `cargo`.** You do not have approval for shell commands and the
   orchestrator runs the lint to verify. Work from the item list below.
4. **Do not add `#![deny(missing_docs)]`** -- the orchestrator adds it once the
   crate verifies at zero.
5. **Do not run `git commit`.**
6. For a `struct field` or `enum variant`, a single clear line is enough. Spend
   your effort on traits, public entry points, and anything security- or
   protocol-related.
7. If two items mean the same thing, do not paste identical text -- say how they
   differ.

## Item list

Every entry below is a `missing_docs` warning from the compiler, so this list is
exhaustive and mechanical. Grouped by file, with line numbers from the current
tree. Line numbers SHIFT as you insert doc comments -- work bottom-up within a
file, or re-find the item by name.


## When you are done

Report:

1. Which files you touched and roughly how many items you documented.
2. Any item you could NOT document because its purpose was not inferable from the
   code, named as `file:line - Name`.
3. Any public item that in your judgement should not be public. Do not change it.
4. Any code defect you noticed. Do not fix it.
