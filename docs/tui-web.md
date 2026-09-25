# Browser-rendered TUI (`packages/hya-tui-web`)

`packages/hya-tui-web` runs a terminal program on a real PTY and renders it in
a browser with xterm.js. It has two jobs:

- **TUI test environment.** Playwright drives Chromium against the rendered
  terminal. Tests type keys, resize the viewport, read the screen as text,
  check per-cell colors and glyph widths, and attach screenshots, with no
  tmux scraping.
- **WebUI.** The same host serves the TUI to any browser, so the TUI and the
  WebUI ship as one frontend.

The host is terminal-program agnostic: it runs one fixed command per browser
connection. It pairs with the Bun/OpenTUI frontend (`packages/hya-tui`,
[ADR-0019](adr/0019-adopt-opentui-frontend.md)). The suite has two parts. An
OpenTUI probe fixture (`e2e/fixtures/opentui-probe.ts`) checks the renderer
itself. `e2e/hya-tui.spec.ts` drives the real TUI against an isolated
`hya serve` running the offline echo model. The backend stays unaware of
the host, because rendering never moves into `hya serve` (see
[ADR-0018](adr/0018-browser-rendered-tui-test-environment.md)).

The offline echo model only ever echoes the prompt back, so it cannot
exercise streamed text chunks, reasoning, tool calls, or a busy/working state.
For specs that need those, `e2e/fake-model.ts` runs a small scriptable OpenAI
model server (Chat Completions and Responses API) in the Playwright process
itself, and the `backend` fixture can wire the isolated `hya serve` to it
instead of the offline model. See "The fake model" below.

## Usage

Requires Bun 1.4.2 (native `Bun.spawn({ terminal })` PTYs; macOS and Linux).

```sh
cd packages/hya-tui-web
bun install --frozen-lockfile
```

Serve a terminal program, then open the printed URL:

```sh
bun src/main.ts --port 7681 -- bun e2e/fixtures/opentui-probe.ts
# hya-tui-web listening on http://127.0.0.1:7681/
```

To serve the hya TUI against a running backend (`hya serve --bind 127.0.0.1:8080`):

```sh
bun packages/hya-tui-web/src/main.ts -- \
  bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"
```

Each browser tab gets its own process. Closing the tab sends the process
SIGHUP. When the process exits, the page shows
`[process exited with code N]`.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--host ADDR` | `127.0.0.1` | Bind address. The host spawns processes for any same-origin client, so bind only loopback unless it runs behind an authenticating proxy. |
| `--port N` | `7681` | Bind port; `0` picks a free port. |
| `--cwd DIR` | current directory | Working directory of the spawned command. |
| `-- <command...>` | required | argv spawned for every connection. |

Page query parameters: `font` (CSS font family) and `fontSize` (pixels).

### Running the tests

```sh
cd packages/hya-tui-web
bun run typecheck
bun test ./test          # codec + host unit tests (bun run test)
bunx playwright test     # browser E2E (bun run test:e2e)
```

`e2e/hya-tui.spec.ts` needs a built backend. Run
`cargo build -p hya-backend --bin hya` first, or set `HYA_BIN` to another
`hya` binary. The `backend` fixture (`e2e/hya.ts`) starts `hya serve` on a
free port. It uses a throwaway `--db` and a temporary `HOME` and `XDG_*`
directories, so your own config and keys are never read. `hyaTui(backend)`
returns the argv that runs `packages/hya-tui` against that backend. The TUI
dependencies must be installed (`bun install` in `packages/hya-tui`).

Playwright 1.63.0 uses the Chromium 1243 browser build. Run
`bunx playwright install chromium` once if it is not already cached.
Test results go to `test-results/`, and the HTML report goes to
`playwright-report/`. Every test writes its final screen to
`test-results/<test>/final-screen.png` and `final-screen.txt` and attaches
both to the report. Open the PNG to review a visual change. The repository's
TUI preview and test rules are in `AGENTS.md` ("TUI Preview & Browser Test
Rule").

### Writing a TUI test

```ts
import { expect, fixture, test } from "./harness"

test("echoes a prompt", async ({ tui }) => {
  const term = await tui(fixture("opentui-probe.ts"), { viewport: { width: 900, height: 500 } })
  await term.waitForText("type here")
  await term.type("hello web")
  await term.press("Enter")
  await term.waitForText("echo:hello web")
  const at = (await term.find("accent"))!
  expect((await term.cell(at.row, at.col))?.fg).toBe("#73c8e8")
})
```

**What xterm.js sends.** Keys reach the program as xterm.js 6.0 encodes
them, and xterm.js does not implement the kitty keyboard protocol or
modifyOtherKeys. So `press("Shift+Enter")` sends a plain CR, the same as
Enter; `press("Alt+Enter")` sends ESC CR; `press("Control+j")` sends LF. A
program that needs a distinct "newline" key in the browser must accept LF or
ESC CR (the hya TUI does; see [tui.md](tui.md#composer)). To paste, call
`page.evaluate(() => window.hyaTerm.term.paste(text))`: xterm.js turns line
feeds into CRs and wraps the text in bracketed-paste markers when the program
enabled mode 2004 (OpenTUI does). `type()` types key by key and is not a paste.

### The fake model

`e2e/fake-model.ts` (`startFakeModel(steps)`) serves scripted SSE, one `Step`
consumed per model request, on two routes. The request path picks the wire
protocol, so the same steps work on either:

| Route | Protocol | hya provider kind | What hya decodes |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | `chat` (default) | `openai-compatible` | `delta.content`, `delta.tool_calls`, `finish_reason`, trailing `usage` (`crates/hya-provider/src/openai/decoder.rs`). No reasoning: `reasoning_content` is ignored. |
| `POST /v1/responses` | `responses` | `openai-response` | `response.reasoning_summary_text.delta`, `response.output_item.added/done` (reasoning and `function_call` items), `response.function_call_arguments.delta`, `response.output_text.delta/done`, and the typed terminal `response.completed` or `response.incomplete` (`crates/hya-provider/src/openai/response_decoder.rs`). The stream never sends `[DONE]`. |

It mirrors the process-level Rust reference (`crates/hya-e2e/src/fake_llm.rs`)
but runs inside the Playwright/Node process, so a spec needs no extra binary.
Reasoning only reaches the TUI on the `responses` protocol; on `chat` a
`reasoningStep` streams just its answer text.

Step types (`e2e/fake-model.ts`):

| Step | Fields | Effect |
| --- | --- | --- |
| `textStep(text, opts?)` | `text: string`; `opts.chunkSize?: number` (default: whole string); `opts.delayMs?: number` (default `0`); `opts.finish?: "stop" \| "length"` (default `stop`) | Streams `text` in chunks, then finishes. `length` ends it as if it hit the output limit (`finish_reason: "length"` / `response.incomplete`), so hya records `FINISH_REASON_LENGTH`. |
| `reasoningStep(reasoning, text, opts?)` | `reasoning: string`, `text: string`; `opts.chunkSize?`, `opts.delayMs?` as above | On `responses`: streams `reasoning` as reasoning-summary deltas (a reasoning item at output index 0), then `text` (index 1), then `response.completed`. hya records a reasoning part before the text part. On `chat`: only `text`. |
| `toolStep(name, args)` / `toolsStep(calls)` | `name: string`, `args: unknown`; or `calls: { name, arguments }[]` | Streams one or more tool calls (chat: `delta.tool_calls`; responses: `function_call` items with argument deltas), then finishes with tool calls. |
| `httpErrorStep(status)` | `status: number` | Responds with that HTTP status before any stream opens. |
| `hangStep(ms?)` | `ms?: number` | Holds the connection open with no bytes written (observe a busy/working state, or test cancellation) until `fake.release()` is called, or `ms` elapses and it finishes as an empty `stop` reply. |

When the steps run out, a request gets an empty `stop` reply.

`startFakeModel(initial: Step[] = [])` returns:

| Field | Type | Meaning |
| --- | --- | --- |
| `baseUrl` | `string` | `http://127.0.0.1:<port>/v1`, for a provider's `base_url`. |
| `requests()` | `unknown[]` | Recorded request bodies (unrouted, or matched by no route), in arrival order. |
| `push(steps)` | `(Step[]) => void` | Append more steps to the shared (unrouted) queue. |
| `route(marker, steps)` | `(string, Step[]) => void` | Pin `steps` to requests whose system text contains `marker` (chat: `system`-role messages; responses: `instructions` and `system` input items), so independent flows can be scripted for concurrent agents. An exhausted route does not fall back to the shared queue. |
| `routeRequests(marker)` | `(string) => unknown[] \| undefined` | Recorded bodies for one route, or `undefined` if never registered. |
| `setUsage({ prompt, completion, reasoning })` | `(Usage) => void` | Attach a `usage` object to every finishing chunk from now on. |
| `release()` | `() => void` | Release the oldest pending `hangStep`. |
| `pendingHangs()` | `() => number` | Count of hangs currently holding a connection open. |
| `stop()` | `() => Promise<void>` | Stop the server. |

The `backend` fixture (`e2e/hya.ts`) gains a `model` test option that wires
the isolated `hya serve` to a fake model instead of the offline echo model:

```ts
import { expect, hyaTui, textStep, test } from "./hya"

test.describe("streamed reply", () => {
  test.use({ model: { steps: [textStep("hi from the fake model", { chunkSize: 4, delayMs: 20 })] } })

  test("shows the reply", async ({ tui, backend, fakeModel }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.type("hello")
    await term.press("Enter")
    await term.waitForText("hi from the fake model")
    expect(fakeModel!.requests().length).toBe(1)
  })
})
```

`model` takes `{ steps: Step[]; protocol?: "chat" | "responses"; permission?: "default" | "allow" | "danger" } | undefined`
(wrapped, not a bare array — Playwright's fixture-option machinery
parametrizes a test per array element for a bare array "option" value,
silently dropping steps past the first). `protocol` defaults to `chat`. `permission` is the backend's `permission.model`
(default `default`, under which `bash`, `edit`, and `write` ask first and
leave a pending permission request); specs that run those tools without
answering a prompt use `allow`.
Leaving `model` unset keeps the existing offline echo model, so specs that
predate the fake model are unaffected. When `model` is set, the `backend`
fixture starts the fake model before `hya serve` and writes
`$XDG_CONFIG_HOME/hya/config.yaml` selecting it. `kind` is
`openai-compatible` for `chat` and `openai-response` for `responses`:

```yaml
default_model: fake/model
providers:
  fake:
    kind: openai-compatible   # or openai-response
    base_url: http://127.0.0.1:<port>/v1
    api_key: e2e-test-key
    models:
      - id: model
mcp: {}
plugins: {}
permission:
  model: default              # the `permission` option
  rules: []
```

To script a subagent, route the parent's and the child's requests by their
system prompts: the main agent's contains ``NEVER call `report` `` and a
subagent's ``Finish your task with `report` `` (see
`e2e/hya-tui-tools.spec.ts`):

```ts
test.use({ model: { steps: [] } })
test("subagent", async ({ fakeModel }) => {
  fakeModel!.route("NEVER call `report`", [toolStep("task", { description: "survey", prompt: "list files", subagent_type: "general" }), textStep("spawned")])
  fakeModel!.route("Finish your task with `report`", [toolStep("read", { path: "notes.txt" }), hangStep(20_000)])
  // …
})
```

To script thinking, select the Responses route:

```ts
test.use({
  model: { protocol: "responses", steps: [reasoningStep("weigh the options", "The answer is 7.")] },
})
```

A spec that only needs to assert on the v1 HTTP API (no TUI rendering) can
skip `tui()` and use `fetch` directly against `backend.url` with the
`x-hya-directory: <backend.dir>` header; see `e2e/fake-model.spec.ts`.

## Interfaces

### WebSocket `GET /pty?cols=C&rows=R`

The host upgrades `/pty` to a WebSocket and spawns the command on a `C`×`R`
PTY. Both values must be integers from 1 to 4096; invalid or missing values
fall back to 80×24. A request whose `Origin` host differs from its `Host`
header gets `403`. Any other path besides `/` answers `404`.

Messages are text frames in the protojson form of the `hya.v1`
`PtyClientFrame` / `PtyServerFrame` messages (`proto/hya/v1/pty.proto`).
`bytes` fields are standard base64, so these are the same shapes as
`/v1/pty/{id}/connect`:

| Direction | Frame | Effect |
| --- | --- | --- |
| client → host | `{"input": "<base64>"}` | Write bytes to the PTY. |
| client → host | `{"resize": {"cols": C, "rows": R}}` | Resize the PTY (the child gets SIGWINCH). |
| client → host | `{"ping": true}` | Host answers `{"pong": true}`. |
| host → client | `{"output": "<base64>"}` | PTY output bytes. |
| host → client | `{"exit": N}` | Child exit code; the host then closes the socket (1000). |

Unknown or malformed client frames are ignored. `attach` is not used.

### Page test hook `window.hyaTerm`

| Field | Type | Meaning |
| --- | --- | --- |
| `term` | xterm.js `Terminal` | Live terminal; read `term.buffer.active`, `cols`, `rows`. |
| `connected` | `boolean` | WebSocket open. |
| `exitCode` | `number \| null` | Child exit code once reported. |
| `frames` | `number` | Output frames written so far. |

### Playwright fixture (`e2e/harness.ts`)

`tui(command: string[], options?: { cwd?, env?, viewport? }) => Promise<Tui>`
starts a host on a free port, opens the page, and waits for the connection.
Teardown attaches the final screen and stops the host. `fixture(name)` returns
`["bun", <e2e/fixtures/name>]`.

| `Tui` method | Returns |
| --- | --- |
| `lines()` / `text()` | Visible rows, right-trimmed / joined with `\n`. |
| `waitForText(pattern, timeout?)` | Resolves when the screen matches a string or RegExp. |
| `find(needle)` | `{ row, col }` of the first match, or `null`. |
| `cell(row, col)` | `{ char, fg, bg, bold, italic, underline, inverse, width }`; colors are `#rrggbb`, `palette:N`, or `default`. |
| `size()` | `{ cols, rows }`. |
| `type(text)` / `press(key)` | Playwright keyboard input (`"Enter"`, `"Control+C"`, …). For a bracketed paste use `window.hyaTerm.term.paste(text)` (see "What xterm.js sends"). |
| `resize(width, height)` | Resizes the viewport, waits for a new grid size, and returns it. |
| `waitForExit(timeout?)` | Child exit code. |
| `attach(testInfo, name)` | Writes `<name>.png` and `<name>.txt` to the test output dir and attaches both. |

### Library

`src/host.ts` exports `startHost({ command, cwd?, env?, hostname?, port? })`.
It returns `{ url, stop() }`. The spawned command gets
`TERM=xterm-256color` and `COLORTERM=truecolor`. `src/frames.ts` exports the
frame codec (`encodeClientFrame`, `decodeClientFrame`, `encodeServerFrame`,
`decodeServerFrame`).
