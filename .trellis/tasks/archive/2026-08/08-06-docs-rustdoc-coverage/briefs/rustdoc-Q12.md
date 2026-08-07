# Rustdoc Q12 - `hya-updater`, `hya-e2e`, `hya-ts`

You are writing Rust API documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your batch

Crate(s): `hya-updater`, `hya-e2e`, `hya-ts`. **144 undocumented public items.** Do not touch any other
crate -- other writers are working in parallel.

`hya-updater`'s `//!` is the strongest in the workspace -- leave it. `hya-e2e`'s is solid -- leave it. **`hya-ts`'s `//!` leads with the crate name and restates it, and `main.rs` has none** -- rewrite the lib one to explain the launcher-shim role (resolve and spawn the Bun TUI frontend against a hya backend) and add one to `main.rs`.

`hya-updater`: `read_floor` (`journal.rs`) is security-adjacent -- it is the anti-rollback floor, so say what a wrong value permits.

`hya-e2e`: `ToolCallStep`, `tool_step`, `tools_step` are what a test author reaches for first.

`hya-ts`: `Cli` has ~10 undocumented public fields; `BunCommand` and `invocation_name` also need text.

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

### `crates/hya-e2e/src/backend.rs` (9)

22:field, 23:field, 52:assoc fn, 70:field, 71:field, 72:field, 73:field, 76:field, 261:method

### `crates/hya-e2e/src/error.rs` (7)

7:var, 9:var, 11:var, 13:var, 15:var, 17:var, 19:var

### `crates/hya-e2e/src/fake_llm.rs` (7)

29:struct, 30:field, 31:field, 106:method, 119:method, 324:function, 331:function

### `crates/hya-e2e/src/scenario.rs` (32)

73:assoc fn, 78:method, 95:method, 101:method, 107:method, 113:method, 119:method, 132:method, 138:method, 144:method, 149:method, 182:field, 183:field, 184:field, 185:field, 186:field, 187:field, 191:method, 195:method, 208:method, 219:method, 227:method, 232:method, 236:method, 252:method, 271:method, 279:method, 283:method, 291:method, 295:method, 299:method, 418:method

### `crates/hya-ts/src/lib.rs` (22)

10:struct, 16:field, 18:field, 20:field, 22:field, 24:field, 32:field, 34:field, 36:field, 38:field, 41:function, 58:field, 63:field, 76:field, 84:field, 88:field, 92:field, 100:field, 275:struct, 276:field, 277:field, 278:field

### `crates/hya-updater/src/error.rs` (24)

7:field, 7:var, 9:var, 11:var, 13:field, 13:field, 13:var, 15:field, 15:field, 15:var, 17:var, 19:var, 21:field, 21:field, 21:var, 23:field, 23:field, 23:var, 25:field, 25:var, 27:var, 29:var, 31:var, 33:var

### `crates/hya-updater/src/fetch.rs` (2)

16:field, 17:field

### `crates/hya-updater/src/journal.rs` (8)

15:var, 16:var, 24:field, 25:field, 26:field, 32:field, 33:field, 193:function

### `crates/hya-updater/src/layout.rs` (6)

14:field, 15:field, 16:field, 17:field, 18:field, 19:field

### `crates/hya-updater/src/metadata.rs` (15)

6:field, 14:field, 20:field, 21:field, 33:field, 34:field, 35:field, 48:field, 56:field, 57:field, 58:field, 59:field, 60:field, 61:field, 62:field

### `crates/hya-updater/src/pipeline.rs` (9)

25:field, 26:field, 27:field, 28:field, 34:field, 35:field, 38:field, 41:field, 44:field

### `crates/hya-updater/src/stage.rs` (3)

13:field, 14:field, 18:method

## When you are done

Report:

1. Which files you touched and roughly how many items you documented.
2. Any item you could NOT document because its purpose was not inferable from the
   code, named as `file:line - Name`.
3. Any public item that in your judgement should not be public. Do not change it.
4. Any code defect you noticed. Do not fix it.
