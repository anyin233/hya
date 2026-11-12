# How do the plugin hook contract and TypeScript adapters stay in sync?

`crates/hya-plugin/src/messages.rs` owns the serde payloads and the
`plugin_hooks!` schema (`Variant: "wire.name" => Params => Reply`). Rust expands
the enum and name helpers together. `cargo run -p xtask -- gen-plugin` reads
that schema and its reachable serde types to generate both adapters'
`src/wire.gen.ts` names, types, and payload guards. The committed output is
checked by `generated_plugin_contract_and_runtime_are_current` in xtask.

Do not split the Rust enum from generated exhaustive helpers: xtask depends on
hya-plugin through hya-app, so adding an enum variant would prevent the
regeneration command from compiling. The macro avoids that bootstrap cycle.

The Bun adapter's `src/stdio.ts` and `src/protocol.ts` are canonical; gen-plugin
copies them to the Claude adapter and the stale-output gate checks the copies.
Both installed adapter directories remain self-contained. Initialization is a
barrier; independent request ids and observations run concurrently; shutdown
stops admission, drains active work, then disposes. Each hook chain still folds
its handlers in order. Tests hold a Bun tool and a real Claude shell hook open
while a second request completes.

The Bun tool context contains only `sessionID` and output `metadata()`. Removed
placeholders are `messageID`, `agent`, `directory`, `worktree`, `abort`, and
`ask`. No first-party bundle references those fields. Native call-scoped host
capabilities and `chat.params` request headers remain unchanged. Rust goldens
and both TS adapters pin all 19 hooks, including optional fields and nested
completion requests.
