# hya-extra/orchestrator

Optional bundle with a main agent that exposes only `code` to its model.
JavaScript programs call hya tools, workers, and an optional typed Jev judge;
every call has a source line and a JSONL trace. The worker agent retains the
ordinary file and shell tools.

## Prerequisites

- Bun 1.4.2 (the verified runtime) on `PATH`.
- hya with nested `tools.list` / `tool.call` host capabilities, agent
  `advertise`, and process `timeout_ms` support.
- A TypeSafe API key only when using `models.classify`.

## Package and install

```sh
cargo run -p xtask -- package-bundle bundles/extra/orchestrator orchestrator.hyabundle
hya bundle install orchestrator.hyabundle
hya
```

In the TUI, enter `/agent orchestrator` to select it. For headless runs, set
`default_agent: orchestrator` in hya's `config.yaml` before `hya exec`.

The coordinator supplies `prompts/orchestrator.md` and `prompts/worker.md`.
Those files must be present before packaging. The base checkout predating the
host/manifest changes cannot prepare this bundle.

Configure the optional judge in
`<hya config dir>/bundles/hya-extra%2Forchestrator/config.yml` (see
`config.example.yml`). With no judge, tools and workers still work;
`models.classify` rejects with configuration instructions.

For example, the `code` input can be:

```json
{"script":"const results = await Promise.all([tools.read({path:'README.md'}), tools.glob({pattern:'docs/*.md'})]); return results;"}
```

The script is an async function body in a `node:vm` context. The VM runs in a
worker thread so the parent can enforce the deadline even after an `await`.
Imports, process access, fetch, Bun, and timers are unavailable. This controls
how effects are recorded; it is not a security boundary. Calls that already
happened are not rolled back. When a script completes, fails, or times out, pending `agents.run` workers
are stopped with `archive({target: handle, reason})`. The trace records
`handle` and `session` as soon as `task` returns, then marks leftover calls
`abandoned:true` with `archived:true` on cleanup success or `archived:false`
and `archive_error` on failure. Trace lines show `abandoned and archived, <handle>`
or the archive error. Pending starts get up to five seconds to return;
unresolved starts show `abandoned (start unknown)`. Workers whose own
`agents.run` timeout already returned remain live and can be managed through
ordinary hya tools.

## Files

- `runtime.ts` — self-contained stdio process, VM runner, and exported logic.
- `runtime.test.ts` — fake-host/Jev tests and real-child stdio tests; `bun test`.
- `bundle.yaml` — runtime, tool, and two agent declarations.
- `config.example.yml`, this README — documentation, not packaged.

Only the runtime and agent prompts are declared for packaging. See
[`docs/extra-bundles.md`](../../../docs/extra-bundles.md#hya-extraorchestrator)
for usage, globals, options, result/trace schemas, and judge config fields.
