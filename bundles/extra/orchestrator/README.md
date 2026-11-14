# hya-extra/orchestrator

An optional main agent whose only model-facing tool is `workflow`. JavaScript
programs arrange specialised workers and an optional TypeSafe Jev judge. The
orchestrator has no file, search, command or edit tools; every external effect
comes from a worker or judge call with a source line and JSONL trace.

## Package and use

Requires Bun 1.4.2 on `PATH` and hya with nested `tool.call`, agent `advertise`
and process `timeout_ms` support. The bundle packages a main-agent prompt
and three specialised worker prompts.

```sh
cargo run -p xtask -- package-bundle bundles/extra/orchestrator orchestrator.hyabundle
hya bundle install orchestrator.hyabundle
hya
```

Select `/agent orchestrator` in the TUI. For headless runs, set
`default_agent: orchestrator` in hya's `config.yaml` before `hya exec`.

| Worker | Tools | Purpose |
| --- | --- | --- |
| `orchestrator-scout` | `read`, `grep`, `glob` | Find facts; cannot run commands or change files. |
| `orchestrator-runner` | `bash`, `read` | Run commands and report outcomes; no edit tools, and its prompt prohibits shell edits. |
| `orchestrator-editor` | `read`, `grep`, `glob`, `write`, `edit` | Make changes; cannot run commands or tests. |

Each worker also receives harness coordination tools, including `report`.
An editor makes a change and a runner checks it. The runner's shell restriction
is a prompt rule, not a filesystem sandbox.

## Workflow interface

The tool accepts exactly one of:

```ts
{script: string, args?: object, save_as?: string}
{run: string, args?: object}
{list: true}
```

Start with `{"list":true}` to find reusable workflows. For example, run and save:

```json
{"script":"// Find files for a request\nreturn await agents.run({agent:'orchestrator-scout',prompt:args.request,result:{paths:'Relevant file paths'}});","args":{"request":"Find the exporter and its tests"},"save_as":"find-files"}
```

Reuse it with `{"run":"find-files","args":{"request":"Find the parser"}}`.
Names match `^[a-z0-9][a-z0-9-]{0,63}$`. Unknown names produce a tool error
listing stored names; malformed inputs describe the three accepted forms.

The script is an async function body. Its globals are:

- `args`: the supplied object, default `{}`.
- `agents.run({agent, prompt, description?, result?, model?, effort?, timeout_secs?})`:
  `agent` is required and must name one of the three workers above; `prompt` is
  a non-empty string. `result?: Record<string,string>` describes requested report
  fields. Timeout defaults to 1800 seconds and accepts `0..1800`. Resolves to
  `{handle, session, status, report, result}`, where `status` is `done`, `failed`,
  `cancelled` or `timeout`, `report` is rendered channel text, and `result` is an
  object or `null`.
- `models.classify({state, questions})`: typed judge questions; returns `{answers}`.
  Question and answer schemas are in the interface reference linked below.
- `text(value)`, `console.log(...values)`, and `return value`: collect output.

Worker `result` fields append this instruction after a blank line:

```text
End your report with one final line of the form RESULT: {...} — a single-line JSON object with exactly these fields:
- <field>: <description>
```

The last `RESULT:` line is parsed, including one following a `[sender]` prefix.
Missing/invalid JSON, a non-object or a missing requested field yields `null`.
A result is parsed even without requested fields. `Promise.all` supports
independent concurrent calls.

`tools`, `ALL_TOOLS` and `describeTool` are removed. Touching `tools` explains
that a workflow can only call `agents.run` and `models.classify`. Imports,
`require`, `process`, `Bun`, `fetch` and timers are unavailable. The VM runs in
a worker thread so deadlines cover CPU loops after `await`. This is effect
recording discipline, not a security boundary. The runtime's single host-call
gate permits only `task`, `wait`, `archive` and `read` with a `channel://` path;
it never discovers host tools.

Only a first-line `// @options: {"timeout_ms":1800000,"max_output_chars":3000}`
comment sets options. Both must be positive integers; maxima are 3000000 ms
and 3600 characters respectively. The host process timeout is 3600000 ms.

## Store, results and traces

A completed `save_as` writes the exact source to
`<HYA_BUNDLE_CONFIG_DIR>/workflows/<name>.js` and metadata to `<name>.json`:
`{name, description, saved_ms, runs, last_run_ms, last_status}`. Names and
descriptions are strings; times are Unix epoch milliseconds, `runs` is an
integer, and status is `completed` or `failed`. Description comes from the
first `//` line, excluding an options line. A new/replaced source starts with
one run (the successful saving execution); subsequent stored runs increment
it and update the last time/status, including failures. A failed save leaves
any previous source and metadata untouched. The per-installation store
survives sessions. Listing returns name, description, saved time and run count
without executing. hya supplies the writable configuration directory for
the bundle process.

Plugin replies are `{ok: boolean, output: string, time_ms: number}`. Text starts
with `Workflow completed in … s` or `Workflow failed in … s`; a stored run uses
`Workflow <name> completed in … s` (or `failed`). Successful saves add
`saved as <name>`. Output items and optional `<console_output>` precede the
`<trace>` of `agents.run` and `models.classify` calls. Failures retain partial
output and add `Script error: <message>`. Display output stays below 4800
characters, retaining output start/end and the first/last trace lines.

Full JSONL traces live at
`<HYA_BUNDLE_CONFIG_DIR>/traces/<session>/<call>.jsonl` (system temp when unset;
session/call components percent-encoded). The first record is
`{script, args, stored, options}`; `stored` names the stored source or successful save,
or is `null`. Call records have `seq`, `line`, `kind` (`agent` or `classify`),
`name`, `input`, `ok`, `output?`, `error?`, `started_ms`, `ms`; agent records
also carry `result`, and `handle`/`session` once started. Input/output are JSON
strings cut to 4000 characters. The final record holds `{status, output}`
with the complete untruncated output. Calls are recorded in start order.

Effects are not undone. Pending worker calls are abandoned and known workers
are archived before returning, with `abandoned`, `archived` and `archive_error`
recorded. Pending starts get up to five seconds to return an identity;
unresolved starts show `abandoned (start unknown)`. Workers whose own helper
timeout already returned remain live and can be managed through hya's
ordinary coordination tools.

## Configuration and checks

The optional judge reads `HYA_BUNDLE_CONFIG_FILE`, normally
`<hya config dir>/bundles/hya-extra%2Forchestrator/config.yml`, or
`.hya/bundles/orchestrator/config.yml` for a project install. See
`config.example.yml`. Workers and stored scripts work without a judge;
`models.classify` rejects clearly until configured.

| Key | Type | Default |
| --- | --- | --- |
| `judge.endpoint` | non-empty string | `https://api.typesafe.ai/v1/systemone` |
| `judge.model` | non-empty string | `jev-1.14.0` |
| `judge.api_key` | non-empty string | Set exactly one key source. |
| `judge.api_key_file` | absolute path string | Read and trim the key from this file. |
| `judge.timeout_ms` | integer `1..3000000` | `20000` |

hya's optional `agents.<agent-id>.model` overrides are independent of the judge.
Unknown keys inside `judge` are rejected. Configuration is read at process
start; hya restarts it at the next root binding after a config edit. Restart
hya after changing only the key file.

Run `bun test` in this directory for offline fake-host/fake-Jev and real-child
stdio tests. Only `runtime.ts` and the four prompts are packaged; tests,
this README and `config.example.yml` are documentation/development files.
See [Extra bundles](../../../docs/extra-bundles.md#hya-extraorchestrator) for
the complete classification and protocol contracts.
