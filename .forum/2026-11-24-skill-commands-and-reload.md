# Why is a skill command `/skill:<name>`, and why does `/reload-context` wait for the next turn?

A skill used to be a bare `/<name>` command row, so a `.hya/commands` file or a
TUI command of the same name hid it, and a body without `$ARGUMENTS` dropped
the request. `skill:` is now a reserved namespace (as in pi): the catalog
lists `skill:<name>`, rejects command files named `skill:…`, and expands a
`<skill name location>` block with the request appended. The TUI folds that
block from the durable message text, so every client shows it the same way.

A session records its environment, `AGENTS.md`, and skill index once
(`InitialContextRecorded`, first record wins) and every round reads it.
`RefreshSessionContext` appends `InitialContextInvalidated`, which clears it;
the next turn records it again. It is refused while a turn runs because a
cleared record mid-turn would drop the context from the remaining rounds. The
changed system prompt misses the provider's prompt cache once.

Pitfall: `cargo run -p xtask -- gen-api` compiles `hya-server` first. Change
the `.proto`, run `gen-api`, then write the server code that uses the new
types; the other order fails to build. A new `Event` variant also needs
`cargo run -p xtask -- gen-plugin` (the adapters' `wire.gen.ts`). After a
`hya-tool` change, a `cargo test`/`clippy` over several packages rebuilds
`target/debug/deps/libhya_base_tools.so` against another `hya-tool` build, and
every TUI terminal spec then fails at once with "native tool bundle
`hya_base_tools` was built for a different hya-tool ABI": run
`cargo build -p hya-backend --bin hya` again right before the specs.

[Skills](../docs/skills.md#invoke-a-skill) · [Reload](../docs/tui.md#reload)
