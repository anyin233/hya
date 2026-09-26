# Browser-rendered TUI (`packages/hya-tui-web`)

`packages/hya-tui-web` runs a terminal program on a real PTY and renders it in
a browser with xterm.js. It has two jobs:

- **TUI test environment.** Playwright drives Chromium against the rendered
  terminal. Tests type keys, resize the viewport, read the screen as text,
  check per-cell colors and glyph widths, and attach screenshots, with no
  tmux scraping.
- **WebUI.** The same host serves the TUI to any browser, so the TUI and the
  WebUI ship as one frontend. Running `hya` in a terminal starts it on
  `http://127.0.0.1:3250` (`hya --port <N>` to change) next to the terminal
  TUI; see [Served by `hya`](#served-by-hya).

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

## Served by `hya`

Bare `hya` on a terminal ([CLI reference](cli.md#bare-hya)) runs this host
from `lib/hya/tui-web` next to the binary (a release archive or `install.sh`
puts it there), else `HYA_TUI_WEB_DIR`, else the source checkout's
`packages/hya-tui-web`:

```text
bun <tui-web>/src/main.ts --host 127.0.0.1 --port <port> --cwd <cwd> -- \
  bun <tui>/src/main.ts --server <daemon URL> --dir <cwd> --db <db> --hya <hya> --web-tab
```

So every browser tab runs its own TUI against the database's backend daemon,
the same one (same database, same `--dir`) the terminal TUI uses: sessions,
turns, and permission prompts are shared, and a tab can resume a session the
terminal started and the other way round. `--db` and `--hya` let a tab find or
start the next daemon when that one stops, and let a new tab fall back to the
daemon when the URL in its command no longer answers
([tui.md](tui.md#when-the-server-goes-away)). With `hya --backend <url>` the
command carries only `--server <url> --dir <cwd> --web-tab`. `--web-tab`
tells the tab's TUI that closing the tab is how one leaves a session running:
it does not offer `/to-background`, and Ctrl+D shows `Close the tab to leave
this session running` instead of quitting
([tui.md](tui.md#quit-and-keep-running-or-archive)). The terminal TUI never
gets it. `hya` reads the host's `hya-tui-web listening on <url>` line (stdout)
to learn that it is up and passes the URL to the terminal TUI (`--web-url`),
or the reason it failed (`--web-error`, for example `port 3250 is in use`).
The host's output goes to `hya`'s log file (`[webui] ` lines). When the
terminal TUI exits, `hya` sends the host SIGTERM; the host then ends every
tab's process (below) before it exits. The daemon keeps running. The host stays generic: `hya` only
chooses the fixed command.

Open `http://127.0.0.1:3250` in a browser on the same machine. The host binds
loopback only; to reach it from elsewhere, tunnel it (for example
`ssh -L 3250:127.0.0.1:3250 <host>`) rather than binding a public address.

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

To serve the hya TUI, let it use the database's backend daemon (one-command
launch; the TUI finds `hya` through `--hya`, `HYA_BIN`, or `PATH`, starts the
daemon when none runs, and leaves it running when its tab closes — see
[tui.md](tui.md#start-it)). A host started this way uses the same default
database and so the same daemon and sessions as bare `hya` and a terminal TUI
in the same directory:

```sh
HYA_BIN=target/debug/hya bun packages/hya-tui-web/src/main.ts -- \
  bun packages/hya-tui/src/main.ts --dir "$PWD" --web-tab
```

Pass the TUI's `--web-tab` yourself in a command like this one, which only
runs in browser tabs: the host adds nothing to the command, so without it
the tab's TUI behaves like a terminal one (`/to-background` and Ctrl+D quit
the tab's process and leave the tab showing `[process exited]`).

Or against a backend you run yourself (`hya serve --bind 127.0.0.1:8080`):

```sh
bun packages/hya-tui-web/src/main.ts -- \
  bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD" --web-tab
```

The host stays generic either way: it runs the one fixed command, and the
TUI finds its backend.

Each browser tab gets its own process. Closing the tab sends the process
SIGHUP (the hya TUI then leaves its session running, not archived; the
backend daemon drops it a few seconds later if it is still unused and no
other tab or TUI shows it). When the process exits, the page shows
`[process exited with code N]`.

SIGINT, SIGTERM, or SIGHUP to the host stops it: it sends every tab's
process SIGHUP, SIGKILLs the process group of any still running after 3 s,
waits for all of them, lets each open tab receive its exit frame, then
closes the listener and exits 0. No tab process outlives the host.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--host ADDR` | `127.0.0.1` | Bind address. The host spawns processes for any same-origin client, so bind only loopback unless it runs behind an authenticating proxy. |
| `--port N` | `7681` | Bind port; `0` picks a free port. |
| `--cwd DIR` | current directory | Working directory of the spawned command. |
| `-- <command...>` | required | argv spawned for every connection. |

Page query parameters: `font` (CSS font family) and `fontSize` (pixels).

### Desktop notifications

The page (`web/client.ts`) maps two terminal escape sequences to a browser
`Notification`, for whatever program runs on the PTY — it has no notion of
hya (ADR-0021):

- **OSC 9** (`ESC ] 9 ; <message> BEL`): the payload is the notification
  body.
- **OSC 777** (`ESC ] 777 ; notify ; <title> ; <body> BEL`): only the
  `notify` subcommand is a notification; the title and body are the second
  and third `;`-separated fields (`src/notify.ts` `osc777Notification`).

A notification is shown only while the tab is not the one in front of the
user (`document.hidden`, or the window lacks focus). The `Notification`
permission prompt only opens on a user gesture (the page asks once, on the
first pointer or key event on the terminal); a denial or an unsupported
browser is silent. The hya TUI is one consumer of this (its own gate — the
terminal's focus reporting and the `/notifications` preference — is
documented in [tui.md](tui.md#desktop-notifications)); any other program run
through this host that sends these sequences gets the same browser
notification.

A program may send both sequences for one event (the hya TUI does, for
terminals that honor only one), which would otherwise show two OS
notifications for it. `src/notify.ts`'s `createNotificationDeduper` drops a
repeat with the same body within 250ms of the last one shown — generic:
keyed on the opaque body text, not on what it means — and `showNotification`
also passes `tag` (`notificationTag`, title+body) to `new Notification`, so
the browser's own notification center coalesces a duplicate the window
missed instead of stacking it.

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
`HYA_TUI_MAIN=<path to another checkout's packages/hya-tui/src/main.ts>`
runs the specs against that TUI instead (for example an older revision, to
show that a new spec fails without the change it covers).

### CI

`.github/workflows/ci.yml`'s `tui` job runs this suite on every push and pull
request, alongside the Rust `lint`, `test`, and `e2e` jobs. It installs the
pinned Bun (the same `bun-v1.4.2` install used by the release workflow), runs
`bun install --frozen-lockfile` in `packages/hya-tui`, `packages/hya-tui-web`,
and `crates/hya-plugin-bun/adapter`, then `bun run typecheck && bun test` in
`hya-tui` and the adapter and `bun run typecheck && bun test ./test` in
`hya-tui-web`. It builds `hya` (`cargo build --locked -p hya-backend --bin hya`)
with the same Rust toolchain/cache actions as the Rust jobs, sets `HYA_BIN` to
that binary, installs Chromium (`bunx playwright install --with-deps chromium`),
and runs `bunx playwright test` from `packages/hya-tui-web`, retrying a failed
test once (`retries` in `playwright.config.ts` when `CI` is set). On failure it
uploads `packages/hya-tui-web/test-results/` as the `tui-web-test-results`
artifact.

Reproduce it locally with the commands in
["Running the tests"](#running-the-tests) above; the only CI-specific pieces
are the pinned Bun install and the Chromium install for a clean runner (a
local checkout usually already has both).

**A TUI that starts its own backend.** `launchTest` (from `e2e/hya.ts`) is
`test` with a `workspace` fixture in place of `backend`: the same isolated
HOME/`XDG_*` directories, config (`model`, `projectBundles` options), and
workspace directory, but no running server. `selfLaunch(workspace, extra?,
options?)` returns the `tui()` arguments that run the TUI with no
`--server` and `HYA_BIN` set to the binary under test, so the TUI starts
(and must stop) `hya serve` itself; its default database lands under the
workspace's `XDG_STATE_HOME`, so a second launch in the same test sees the
first one's sessions (`--continue`). See `e2e/hya-tui-launch.spec.ts`:

```ts
import { expect, launchTest as test, selfLaunch, textStep } from "./hya"

test.use({ model: { steps: [textStep("hi")] } })
test("launches", async ({ tui, workspace }) => {
  const term = await tui(...selfLaunch(workspace))
  await term.waitForText("Connected to hya", 30_000)
  await term.type("/exit")
  await term.press("Enter")
  expect(await term.waitForExit()).toBe(0)
})
```

**A request log.** `startProxy(target)` (`e2e/proxy.ts`) is a logging HTTP
pass-through: point the TUI's `--server` at `proxy.url`
(`hyaTui({ ...backend, url: proxy.url })`) and read `proxy.log`
(`{ method, path, at }[]`) to assert which requests the TUI made — for
example that a live frame, not a re-read, updated the screen. SSE streams
pass through unbuffered.

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
Enter; `press("Alt+Enter")` sends ESC CR; `press("Control+j")` sends LF;
`press("Shift+Tab")` sends CSI Z (`ESC [ Z`) — xterm.js keeps the key, the
browser does not move focus — which OpenTUI reports as a shifted `tab`.
After `press("Escape")`, wait for its effect before the next key: a lone ESC
followed at once by another key can be read as Alt+that key. A
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
| `setUsage({ prompt, completion, reasoning })` | `(Usage) => void` | Attach a `usage` object to every finishing chunk from now on (title replies excepted). |
| `setTitleReply(title)` | `(string) => void` | Answer the backend's background session-title requests with `title` (default: an empty reply, so the session stays untitled). |
| `titleRequests()` | `() => unknown[]` | Title request bodies, in arrival order. |
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

`model` takes `{ steps: Step[]; protocol?: "chat" | "responses"; permission?: "default" | "allow" | "danger"; models?: string[]; contextLimit?: number } | undefined`
(wrapped, not a bare array — Playwright's fixture-option machinery
parametrizes a test per array element for a bare array "option" value,
silently dropping steps past the first). `protocol` defaults to `chat`. `permission` is the backend's `permission.model`
(default `default`, under which `bash`, `edit`, and `write` ask first and
leave a pending permission request, shown by the TUI as a permission
prompt); specs that run those tools without answering a prompt use `allow`,
and specs that answer the prompt (`e2e/hya-tui-prompts.spec.ts`: press `1`,
`2`, or `3`) keep `default`.
`models` lists the provider model ids registered (default `["model"]`, each
reachable as `fake/<id>`). `contextLimit` writes each model entry in object
form with `limit: { context: N }`, so `ModelSummary.contextLimit` is known
(the TUI's `ctx N%`); keep it well above the scripted prompts so no
automatic compaction fires. Leaving `model` unset keeps the existing offline
echo model, so specs that predate the fake model are unaffected.

**Session titles.** After a root session's first prompt the backend asks
the fixed `title` agent for a title in the background. The fake model
recognizes those requests by the title agent's system prompt
(`titleAgentMarker`, `"You are a title generator."`) and answers them apart
from the step queue and routes, with no usage, so a spec's steps are never
consumed by a title call whenever it happens. When `model` is set, the `backend`
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

**Project bundles.** A second option, `projectBundles`, installs bundles
into the isolated backend: `Record<string, BundleFiles>` (an object, not an
array, for the same fixture-option reason), mapping a directory name to the
bundle's files (`BundleFiles = Record<string, string>`, path relative to the
bundle root → content). The `backend` fixture writes each one to
`<backend.dir>/.hya/bundles/<name>/` before `hya serve` starts. `hya serve`
itself has no working directory (ADR-0024); these load as project bundles
because every session the TUI creates is given `workdir: backend.dir`, which
ensures (or reuses) a Project rooted there (ADR-0027) — the same place `hya
bundle install --project` puts a bundle. Leaving it unset writes nothing, so
existing specs are unaffected.

`approverBundle({ id, modes, approve })` (`e2e/hya.ts`) builds such a
bundle for [permission modes](tui.md#permission-modes): a `kind: Plugin`
bundle with `permission_modes:` (`modes: { id, title, description? }[]`), a
`permission.approve` hook resource, and an `extensions.process` of kind
`bun` that runs the repository's Bun adapter
(`crates/hya-plugin-bun/adapter/src/main.ts`, exported as `bunAdapterMain`)
with `approver.ts` as a `--bundle-extension`. `approve` is the JS source of
the hook handler; it gets `{ session, root_session, agent, mode, action,
resource }` and returns `allow_once`, `allow_always`, `reject`, or `defer`.
The modes are selectable as `<id>/<mode id>`:

```ts
import { approverBundle, test, textStep, toolStep } from "./hya"

test.use({
  projectBundles: {
    approver: approverBundle({
      id: "e2e/approver",
      modes: [{ id: "echo-only", title: "Echo only" }],
      approve: `async ({ mode, action, resource }) =>
        mode === "echo-only" && action === "bash" && /^echo /.test(String(resource?.value ?? "")) ? "allow_once" : "defer"`,
    }),
  },
  model: { steps: [toolStep("bash", { command: "echo hi" }), textStep("done")] },
})
// GET /v1/permission-modes now lists e2e/approver/echo-only (source "e2e/approver").
```

The bundle process needs `bun` on `PATH` (the same Bun that runs the
specs). See `e2e/hya-tui-permission-modes.spec.ts`.

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
| `term` | xterm.js `Terminal` | Live terminal; read `term.buffer.active`, `cols`, `rows`. A spec can also observe escape sequences the program writes, e.g. `term.parser.registerOscHandler(52, (data) => …)` records OSC 52 clipboard writes (`e2e/hya-tui-clipboard.spec.ts`), and `registerOscHandler(9, …)` / `registerOscHandler(777, …)` record desktop-notification writes (`e2e/hya-tui-notifications.spec.ts`, [Desktop notifications](#desktop-notifications)); return `true` to consume them. |
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

`src/host.ts` exports `startHost({ command, cwd?, env?, hostname?, port?, stopGraceMs? })`.
It returns `{ url, stop() }`; `stop()` resolves once every PTY process has
exited (SIGHUP, then SIGKILL to its process group after `stopGraceMs`,
default 3000). The spawned command gets
`TERM=xterm-256color` and `COLORTERM=truecolor`. `src/frames.ts` exports the
frame codec (`encodeClientFrame`, `decodeClientFrame`, `encodeServerFrame`,
`decodeServerFrame`).
