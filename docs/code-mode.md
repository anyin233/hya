# Code mode

## Introduction

Code mode gives the built-in agents one extra tool, `code-mode__run`. The model
writes a JavaScript program; the program calls the agent's other tools, starts
worker agents and waits for their reports, and can ask a judge model typed
questions. Only what the program outputs returns to the model. One model round
can then run several tools, run them in parallel, branch on results, and
filter a large result before the model reads it. A program can be stored under
a name and run again with new arguments.

Before a program runs, the bundle checks it: syntax, tool names, required
arguments and unavailable globals. A program that fails the check makes no
call, and the result shows the line and column to fix, so a model does not
spend a round, or real side effects, on a program that cannot work. The check
belongs to the bundle, so it is on exactly when code mode is on.

Every call a program makes is one record of a trace, with the source line that
made it. That is the point beyond saving rounds: the plan is in the program,
and each effect can be traced to the line that caused it.

It is supplied by the first-party bundle `hya/code-mode`, which ships with
every backend and is **off by default**. You turn it on in `config.yaml`:

| `tools.code.mode` | What the model is offered |
| --- | --- |
| `normal` (default) | The usual tools. The bundle publishes nothing and starts no process. |
| `with_tools` | The usual tools plus `code-mode__run`. |
| `only` | `code-mode__run` alone. Every other tool stays executable, but only from a program: a direct call of one is refused with a message that names `code-mode__run`. A subagent is still offered `report`. |

Why three modes: `with_tools` lets a model choose per step between a direct
call and a program. `only` forces every action through a program, so each
model round is a plan in code and each effect is a traced call of that
program.

## Usage

```yaml
# ~/.config/hya/config.yaml
tools:
  code:
    mode: with_tools   # normal | with_tools | only
```

The mode is read when the backend starts. Restart a running daemon after
changing it (`hya serve restart`). `hya bundle list` shows `hya/code-mode` as
`disabled` while the mode is `normal`. `hya bundle disable hya/code-mode`
turns the tool off whatever the mode says.

The bundle's process runs on Bun: `$BUN`, then the Bun a release ships under
`<prefix>/lib/hya/bin/`, then `PATH`. With a mode other than `normal` and no
Bun, the bundle fails to load and turns report that error.

A program the model might send:

```js
const files = await tools.glob({ pattern: "src/**/*.rs" });
const sizes = await Promise.all(
  files.paths.map(async (path) => {
    const file = await tools.read({ path });
    return { path, lines: file.output.split("\n").length };
  }),
);
return sizes.sort((a, b) => b.lines - a.lines).slice(0, 5);
```

A program that delegates and branches on what the worker found:

```js
const found = await agents.run({
  agent: "hya-scout",
  prompt: `Where is ${args.symbol} defined and who calls it?`,
  result: { file: "path of the definition", callers: "number of call sites" },
});
if (!found.result) return found.report;
const source = await tools.read({ path: found.result.file });
return { ...found.result, lines: source.output.split("\n").length };
```

Sent as `{"script": "...", "args": {"symbol": "parse_config"}, "save_as":
"where-is"}`, it is stored; `{"run": "where-is", "args": {"symbol": "X"}}`
runs it again. `{"list": true}` lists stored programs and `{"show":
"where-is"}` returns the source and the last error, so a failed program is
repaired, not rewritten.

`agents.run` can start any agent the calling agent may spawn with `task`. With
the optional [`hya-extra/orchestrator`](extra-bundles.md) bundle installed that
includes its specialised workers; an `orchestrator-scout` answer on an
unchanged work tree is remembered and reused without a model call.

`models.classify` needs a judge in the bundle's configuration file
(`<hya config dir>/bundles/hya%2Fcode-mode/config.yml`, see
`bundles/first-party/code-mode/config.example.yml`); without one the call
rejects.

Which agents get it: the built-in agents (`hya-main`, `hya-plan`,
`hya-task`, `hya-scout`), whose view holds every published tool. An agent
defined by an installed bundle selects its own tools in `resource_view` and
cannot select another bundle's tool, so code mode does not change it.

What a program can reach is exactly the calling agent's own view: a read-only
agent's program has no write tool. Each call a program makes is authorized
like a direct call of that tool (same permission mode, rules and asks), and
files it changes are recorded on the `code-mode__run` call. Nested calls
produce no `tool_call_requested` or `tool_result` events of their own; the
trace in the result and the trace file are their record.

In `only` mode the model cannot see the other tools' schemas, so the engine
appends them to the description of `code-mode__run` in each request: one
section per hidden tool with its name, description and argument schema. The
team quick reference in the system prompt, which teaches only offered tools,
is absent in this mode.

## Interface

### Configuration

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `tools.code.mode` | `normal` \| `with_tools` \| `only` | `normal` | See the table above. Any other value, or another key under `tools.code`, is a configuration error. |

### Tool

| Contract | Value |
| --- | --- |
| Bundle | `hya/code-mode` (first-party, `bundles/first-party/code-mode`), process `bun run runtime.ts run`, request timeout 3600 s |
| Tool name | `code-mode__run` |
| Input | Exactly one of `{"script": string, "args"?: object, "save_as"?: string}`, `{"run": string, "args"?: object}`, `{"list": true}`, `{"show": string}`. `script` is the body of an async function. A name matches `^[a-z0-9][a-z0-9-]{0,63}$`. |
| Host capabilities used | [`tools.list`, `tool.call`, `workspace.state`](plugin-protocol.md#request-scoped-host-capabilities) |

The runtime is the one that implements the `workflow` tool of
[`hya-extra/orchestrator`](extra-bundles.md): `bundles/extra/orchestrator/runtime.ts`
is a byte-identical copy, and the process argument (`run` or `workflow`)
selects the tool. `workflow` is the same program runner without `tools` and
with `agents.run` limited to the orchestrator's three workers.

The script runs in a `node:vm` context of the bundle's process. It has no
`require`, `import`, `process`, `Bun`, `fetch`, or timer. This makes effects go
through recorded calls; it is not a security boundary.

| Global | Meaning |
| --- | --- |
| `args` | The `args` object of the call, or `{}`. |
| `tools.<name>(args)` | Call a tool of the agent's view. `<name>` is the tool name with every character that is not valid in a JavaScript identifier replaced by `_`. Resolves to the tool's raw output value; rejects with an `Error` carrying the tool's error text. |
| `ALL_TOOLS` | `{name, description}[]` of every callable tool, fetched once before the script starts. |
| `describeTool(name)` | `{name, description, input_schema}` or `undefined`. |
| `agents.run({agent, prompt, description?, result?, model?, effort?, timeout_secs?, cache?})` | Start one worker agent and wait for its report, through hya's own `task`, `wait` and channel `read`. Resolves to `{handle, session, status, report, result}`; `status` is `done`, `failed`, `cancelled` or `timeout`. `result` maps field names to descriptions; the worker is told to end its report with a `RESULT: {...}` line, which comes back parsed (or `null`). `timeout_secs` defaults to 600 (at most 1800); a worker that runs out is archived. An `orchestrator-scout` answer on an unchanged work tree is remembered (`cached: true` on reuse) unless `cache: false`. |
| `models.classify({state, questions})` | Ask the configured judge typed questions (`bool`, `choice`, `score`) about `state`; resolves to `{answers}`. Rejects when no judge is configured. |
| `text(value)` | Add an item to the output (a string as is, another value as JSON). |
| `console.log(...)` | Add a line to a console block after the output. |
| `return value` | Same as `text(value)`. |

An optional first line sets limits:
`// @options: {"timeout_ms": 1800000, "max_output_chars": 3000}`.
`timeout_ms` defaults to 1800000 and is at most 3000000. `max_output_chars`
defaults to 3000 and is at most 3600, because hya caps a bundle tool result at
5000 characters.

### Pre-run check

Every inline or stored program is checked after the tool list is fetched and
before the script starts. The check is part of the bundle's runtime, so it runs
exactly when the bundle does: `tools.code.mode: normal` or `hya bundle disable
hya/code-mode` turns it off with the tool. It uses only Bun built-ins (the JSC
compiler that runs the script, and `Bun.Transpiler` to locate a syntax error)
plus a token scan; it adds no dependency.

| Rule | Reported when |
| --- | --- |
| Syntax | The script does not compile. Only the first error is shown, with Bun's explanation (`"await" can only be used inside an "async" function`). |
| Unknown tool | `tools.<name>` names no callable tool; the three nearest names are listed. |
| Missing argument | A literal object argument of `tools.<name>(…)`, `agents.run(…)` or `models.classify(…)` lacks a key the schema requires, and has no spread or computed key. |
| Unknown argument | The same object has a key the schema does not list, and the schema is closed (`additionalProperties: false`). `agents.run` and `models.classify` are closed. |
| Unavailable global | `require`, `import`, `process`, `Bun`, `fetch`, timers, `Buffer`, `module`, `exports`, `__dirname`, `__filename` are used and not bound by the script. `typeof x` is allowed. |
| Workflow only | `tools` is referenced, or `agents.run` names a literal agent outside the three workers. |

Arguments passed as a variable (`tools.read(args)`) are not checked; the tool
still validates them when called. A rejected program is a failed run: `save_as`
still stores it and `show` returns the error. The result looks like:

```text
Script failed in 0.0 s

Script error: the script was checked before running and made no call; fix it and send it again:
L2:23: tools.read: missing required argument `path` (given: file_path)
  const r = await tools.read({ file_path: files[0] });
                        ^
<trace>

</trace>
trace file: <absolute path>
```

At most five problems are listed (`… N more` follows). An error the check
cannot see, thrown while the script runs, ends with its script line:
`Script error: null is not an object (evaluating 'b.field') (line 3)`.

The check takes about 20 µs per KB of script (p50; p99 stays under 1 ms up to
about 20 KB) and under 1 ms for the first script of a fresh process.

### Result and trace

```text
Script completed in 12.3 s          (or: Script failed in 4.1 s)
<output items, in order>
<console_output> … </console_output>   (only when used)
Script error: <message>              (only on failure; partial output is kept)
<trace>
#1 L3 tools.bash {"command":"cargo test"} -> ok, 5429 chars, 2.3 s
#2 L5 tools.read {"path":"src/lib.rs"} -> error: file not found, 0.0 s
#3 L9 agents.run hya-scout "where is parse_config" -> done, main/hya-scout-amiya, 48.0 s
</trace>
trace file: <absolute path>
```

- A run from the store names the program in the first line (`Script
  where-is completed in …`); a save adds `saved as <name>`.
- One trace line per tool, agent or judge call, in start order; `L<n>` is the 1-based script
  line of the call.
- The full trace is written as JSON Lines to
  `<hya config dir>/bundles/hya%2Fcode-mode/traces/<session>/<call>.jsonl`:
  a first record with the script, `args` and options, one record per call
  (`seq`, `line`, `kind`, `name`, `input` and `output` cut to 4000 characters,
  `ok`, `error`, `started_ms`, `ms`; an agent record also has `handle`,
  `session`, `result`, and the [workspace state](plugin-protocol.md#request-scoped-host-capabilities)
  before and after as `state_before`/`state_after`), and a last record with
  the status and output text.
- Stored programs live in `<hya config dir>/bundles/hya%2Fcode-mode/workflows/`
  (`<name>.js` and `<name>.json` with description, run count, last status and
  last error); remembered observations in `…/memo/`.
- Output longer than `max_output_chars` keeps its start and end.
- Calls made before a failure are not undone. Calls still running when the
  script ends are abandoned.

### Engine

`SessionEngine::with_exclusive_tool(Some(name))` is the general mechanism
behind `only`: for every agent whose offered tools include `name`, the model
is offered `name` alone (plus `report` for a subagent), the description of
`name` gains a section per hidden tool, and a model call that names a hidden
tool fails. An agent that is not offered `name` is unaffected. `hya-app` sets it to `code-mode__run` for `tools.code.mode:
only`.

### Limits

- A program's output is capped at 3600 characters per call. A program has to
  select what the model needs (slice a file, filter matches); in `only` mode a
  large file takes several calls to read in full.
- The mode is fixed for the life of the backend process.
- Bundle-defined agents are not affected (see above).
