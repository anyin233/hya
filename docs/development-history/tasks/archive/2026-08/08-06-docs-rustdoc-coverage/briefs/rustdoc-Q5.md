# Rustdoc Q5 - `hya-provider`

You are writing Rust API documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your batch

Crate(s): `hya-provider`. **103 undocumented public items.** Do not touch any other
crate -- other writers are working in parallel.

The crate-level `//!` is present; expand it only if it does not explain how a backend is added.

Document the three defining traits FIRST -- `Provider`, `Protocol`, and `Decoder`. Anyone adding a backend implements them and today gets no contract text at all. State call ordering, error semantics, and who owns SSE framing. Then `CompletionRequest`, `Capabilities`, `ProviderError`, `ProviderRouter`, `HttpProvider`, `ProviderKind`, `EventStream`.

docs/architecture/providers.md documents this same surface -- read it and stay consistent with it.

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

### `crates/hya-provider/src/anthropic.rs` (1)

11:struct

### `crates/hya-provider/src/anthropic/decoder.rs` (2)

24:struct, 35:assoc fn

### `crates/hya-provider/src/dev.rs` (2)

15:struct, 19:assoc fn

### `crates/hya-provider/src/fake.rs` (11)

13:enum, 14:var, 15:var, 16:var, 17:field, 18:field, 20:var, 21:var, 24:struct, 32:assoc fn, 49:assoc fn

### `crates/hya-provider/src/google.rs` (3)

11:struct, 200:struct, 211:assoc fn

### `crates/hya-provider/src/http.rs` (10)

31:enum, 32:var, 33:var, 36:var, 37:var, 38:var, 43:method, 80:struct, 109:assoc fn, 235:method

### `crates/hya-provider/src/lib.rs` (60)

6:module, 8:module, 9:module, 11:module, 12:module, 43:type alias, 46:enum, 48:var, 50:var, 52:var, 54:var, 56:var, 59:field, 59:field, 63:struct, 64:field, 65:field, 66:field, 67:field, 68:field, 69:field, 70:field, 117:struct, 118:field, 119:field, 120:field, 121:field, 125:enum, 126:var, 127:var, 128:var, 129:var, 130:var, 131:var, 132:var, 137:assoc fn, 151:method, 164:method, 176:method, 188:method, 322:struct, 323:field, 324:field, 325:field, 326:field, 327:field, 328:field, 329:field, 330:field, 341:trait, 342:method, 343:method, 351:method, 354:method, 374:trait, 375:method, 376:method, 379:trait, 380:method, 381:method

### `crates/hya-provider/src/openai.rs` (1)

19:struct

### `crates/hya-provider/src/openai/decoder.rs` (2)

30:struct, 42:assoc fn

### `crates/hya-provider/src/openai/response_decoder.rs` (2)

51:struct, 65:assoc fn

### `crates/hya-provider/src/openai/responses.rs` (1)

16:struct

### `crates/hya-provider/src/router.rs` (8)

10:struct, 16:assoc fn, 21:method, 26:method, 39:method, 45:method, 53:method, 64:method

## When you are done

Report:

1. Which files you touched and roughly how many items you documented.
2. Any item you could NOT document because its purpose was not inferable from the
   code, named as `file:line - Name`.
3. Any public item that in your judgement should not be public. Do not change it.
4. Any code defect you noticed. Do not fix it.
