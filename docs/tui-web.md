# Browser-rendered TUI (`packages/hya-tui-web`)

`packages/hya-tui-web` runs a terminal program on a real PTY and renders it in
a browser with xterm.js. It has two jobs:

- **TUI test environment.** The required PTY/headless suite drives the real
  TUI directly; optional Playwright browser checks drive Chromium against the rendered
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
HYA_BIN=target/debug/hya bun packages/hya-tui-web/src/main.ts --shift-enter-lf -- \
  bun packages/hya-tui/src/main.ts --dir "$PWD" --web-tab
```

Pass the TUI's `--web-tab` yourself in a command like this one, which only
runs in browser tabs: the host adds nothing to the command, so without it
the tab's TUI behaves like a terminal one (`/to-background` and Ctrl+D quit
the tab's process and leave the tab showing `[process exited]`).

Or against a backend you run yourself (`hya serve --bind 127.0.0.1:8080`):

```sh
bun packages/hya-tui-web/src/main.ts --shift-enter-lf -- \
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

| `--host ADDR` | `127.0.0.1` | Bind address. The host spawns processes for any same-origin client, so bind only loopback unless it runs behind an authenticating proxy. |
| `--port N` | `7681` | Bind port; `0` picks a free port. |
| `--cwd DIR` | current directory | Working directory of the spawned command. |
| `--shift-enter-lf` | off | Translate browser Shift+Enter to LF for programs whose input editor treats LF as newline. |
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
bun test ./test                       # codec + host unit tests (bun run test)
bun run test:tui-exp:parity -- e2e/<spec>.ts  # terminal scenarios for changed screens
bun run test:tui-exp                         # full required terminal suite
bun run test:e2e                             # optional Chromium/WebUI checks
```

Locally, run the specs that cover your change: the new or changed spec and the
specs for the screens it touches. The whole terminal suite is the CI gate; run it
locally only when asked or when a change affects every screen.

#### Experimental direct-PTY suite (`tui-exp`)

The required terminal suite runs the actual Bun/OpenTUI executable on a Bun PTY and
feeds its bytes into `@xterm/headless` 6.0.0, aligned with the browser's xterm.js
version. It reads the resulting screen cells directly, without Chromium, a
DOM, the WebUI host, or a WebSocket. This tests terminal behavior with fewer
processes and records where time is spent. Backend isolation, provider config,
and scripted model responses are shared with the browser fixtures in
`e2e/backend.ts` and `e2e/fake-model.ts`.

Run it from the source checkout:

```sh
cargo build --locked -p hya-backend --bin hya
cd packages/hya-tui-web
bun install --frozen-lockfile
export HYA_TUI_EXP_OUTPUT_DIR="$HOME/data/hya-tui-exp/results"
mkdir -p "$HOME/data/hya-tui-exp/tmp"
export TMPDIR="$HOME/data/hya-tui-exp/tmp"
bun run test:tui-exp
```

Install `packages/hya-tui` dependencies too when using a fresh checkout. Set
`HYA_BIN` to a staged backend executable or `HYA_TUI_MAIN` to another checkout's
frontend entrypoint when comparing versions. For example, run
`bun test ./tui-exp/hya.test.ts --test-name-pattern 'command overlay' --timeout 30000`
to exercise only the command-overlay scenario. No real provider keys or model
calls are used.

GitHub's required `tui` job runs this suite without installing Chromium.
The command runs protocol checks followed by the shared terminal matrix:

```sh
bun run test:tui-exp:unit           # nine focused protocol and real-TUI tests
bun run test:tui-exp:parity         # all shared terminal scenarios, four workers
bun run test:tui-exp:parity -- e2e/hya-tui-layout-editor.spec.ts
HYA_TUI_EXP_WORKERS=2 bun run test:tui-exp:parity
```

The parity runner uses **Playwright's test runner and assertions under Bun**,
without launching Chromium. `tui-exp/playwright.config.ts` selects
`HYA_TUI_DRIVER=pty`; the default browser config selects the browser driver.
Both load the same `e2e/*.spec.ts` test bodies, backend fixtures, fake model
scripts, and assertions. New terminal scenarios automatically run on both
drivers. Requesting a browser `page` in PTY mode fails explicitly.

Coverage includes sessions and archive/resume, providers/models/effort, prompts
and permissions, shell/tools/MCP/bundles, extensions, projects, direct gRPC,
relay connections and remote attachments, subagent panes, every layout editor
flow, pane focus, clipboard OSC 52, notification OSC 9, terminal focus reporting,
Markdown streaming, scrolling, input/history/Vim, and daemon crash/restart and
multi-client synchronization. At introduction, the shared inventory contains
294 terminal scenarios and seven excluded browser-only scenarios;
the browser suite additionally contains two generic host tests.

| Optional Chromium-only coverage, excluded from CI | Why a direct PTY cannot provide equivalent assertions |
| --- | --- |
| `host.spec.ts` (two tests) | WebUI-injected exit notice and browser reload creating a new WebSocket/client process. Direct child exit, cleanup and real process spawning are covered separately by PTY tests. |
| `hya-bare.spec.ts` (five tagged cases) | Actual WebUI tabs alongside the terminal, tab teardown on exit/signals, and WebUI reconnect after daemon replacement. The three terminal-only bare-hya cases also run on PTY. |
| `hya-tui-archive.spec.ts`, WebUI tab case | Web-tab-only command restrictions and closing an actual tab while a turn runs. Cross-client resume runs on two real PTYs as well as terminal/WebUI in Chromium. |
| `hya-tui-notifications.spec.ts`, browser Notification case | Browser Notification API and document visibility. The three terminal OSC notification cases also run on PTY. |

These seven cases are tagged `@browser-only` and excluded via `grepInvert`
from the required gate. They remain available through the optional
`bun run test:e2e` browser suite. The two generic host tests are excluded by exact filename from
the PTY config. Browser clipboard permissions, DOM focus and WebSocket behavior
remain browser responsibilities. Terminal input uses legacy xterm encoding;
Kitty keyboard, pixel mouse coordinates and browser-specific event translation
are not emulated. Unsupported combinations and pixel mouse mode fail explicitly.

Each parity run writes `parity.json` under its output directory. Its contract is
`{ driver, status, durationMs, workers, selected, terminalScenarios,
browserOnlyExcludedTag, browserOnlyFiles, tests }`. Each test records
`{ file, line, title, browserOnly, outcome, runs }`; each run contains
`{ status, durationMs, retry, errors }`. This makes coverage and failures
inspectable without a browser report. Per-client artifacts contain
`final-screen.txt`, `frames.json` and `output.base64.txt`; the runner prints
per-test durations. The default parity output directory is
`~/data/hya-rust/tmp/tui-exp-parity`, overridden by `HYA_TUI_EXP_OUTPUT_DIR`.

##### Shared scenario driver interface

`e2e/harness.ts` exports `Tui` and `tui(command, options?)`. Existing `text`,
`lines`, `find`, `cell`, `size`, `type`, `press`, `resize`, `waitForText`,
`waitForIdle`, `waitForExit` and `attach` assertions work on both drivers.
The following methods keep scenarios independent of browser internals:

| Method / option | Contract |
| --- | --- |
| `paste(text: string)` | Send bracketed paste when enabled by the child. |
| `focus(focused: boolean)` | Browser textarea focus or negotiated CSI 1004 terminal focus reporting. |
| `exitStatus(): Promise<number | null>` | Child status; null while running, `128 + signal` after signal termination. |
| `screenBox()` | `{x, y, width, height}` coordinate space for `mouse`; CSS pixels in Chromium and cells on PTY. Calculate positions from `size()` and this box rather than assuming pixels. |
| `mouse.move/down/up/click/wheel` | Pointer input; move accepts `{steps?}`, click/down accept `{button?: "left" | "middle" | "right"}`. PTY wheel maps 100 delta units to one terminal wheel event. |
| `inspect(callback, arg?)` | Self-contained callback `(terminal, state, arg)` runs against the actual xterm emulator. `state` retains per-client OSC/cursor observations. No DOM, application internals or captured outer variables. |
| `disconnect()` | Browser navigation away or SIGHUP to the PTY process group. |
| `close()` | Close the browser page or reap/dispose the PTY client. |
| `LaunchOptions.independent?: boolean` | Default false replaces the primary client, matching browser navigation/relaunch. True keeps it alive and launches another client for multi-client tests. |
| `LaunchOptions.viewport` | PTY uses deterministic conversion: `cols = floor((width - 15) / 8.43)`, `rows = floor(height / 17)`, bounded below by 2/1. Historical 690×640 narrow cases become 80×37 cells; default 1100×640 becomes 128×37. This checks terminal geometry, not browser font measurement. |
| `LaunchOptions.hostArgs` | PTY supports `--shift-enter-lf`; other host flags fail explicitly. |

The browser `page` property is available only on the browser driver. Shared
scenarios wait for visible state transitions before the next input: command
pane opening, wizard steps, changed draft text, ready status and completed
reconnect. They do not depend on Chromium's incidental input latency.

##### Harness interface

`tui-exp/terminal.ts` exports `PtyTerminal.launch(command: string[], options?)`.
Options are `cols?: number` (default 120), `rows?: number` (default 36),
`cwd?: string`, `env?: Record<string, string>`, and `shiftEnterLf?: boolean`
(default false, matching ordinary browser Shift+Enter's CR). Sizes must be
integer cell counts in 1..4096. Coordinates are zero-based cells.

| Method / property | Contract |
| --- | --- |
| `text()`, `lines()`, `find(text)` | Current visible screen; `find` returns `{row, col}` or null, accounting for wide glyphs. |
| `cell(row, col)` | `{char, width, fg, bg, bold, italic, underline, inverse}` or null; color is `#rrggbb`, `palette:N`, or `default`. |
| `size()`, `resize(cols, rows)` | Emulator and PTY dimensions; resize signals the real child. |
| `type(text)`, `press(shortcut)`, `paste(text)` | UTF-8 text, explicit legacy xterm key encoding, or paste respecting the application's bracketed-paste mode. |
| `click(point)`, `drag(from, to)`, `mouse(action, point, modifiers?, button?)` | Application-enabled mouse reporting; actions are `down`, `up`, `move`, `wheel-up`, `wheel-down`. Modifiers are `{shift?, alt?, ctrl?}`; button defaults to left (0). |
| `waitForText(pattern, timeout?)`, `waitFor(predicate, description, timeout?)` | Poll parsed screen state, default 10 seconds, with screen diagnostics on failure. |
| `flush()` | Wait for already-received output to be parsed; does not assert application idle. |
| `pid`, `exitCode`, `waitForExit(timeout?)` | Real child PID and settled status (`128 + signal` for signal termination, null while running); `waitForExit` also drains PTY EOF. Default exit timeout 10 seconds. |
| `screen`, `osc52` | Underlying headless terminal and captured OSC 52 payloads for protocol assertions. |
| `save(directory)`, `close()` | Save bounded diagnostics; terminate/reap the child, escalating to process-group kill after one second, and dispose the emulator. |

The real-TUI cases save `final-screen.txt`, `frames.json` (last 100 parsed output
batches with elapsed milliseconds), `output.base64.txt` (bounded raw output
diagnostics), and `timings.json` under `$HYA_TUI_EXP_OUTPUT_DIR/<scenario>/`.
The default output root is `~/data/hya-rust/tmp/tui-exp-results`; temporary
workspaces follow `TMPDIR`. Timing fields are `backendSetupMs`,
`launchToReadyMs`, `actionsMs`, `teardownMs`, and `totalMs`, plus `name` and
`passed`. Incomplete phases are omitted on failure. Compare matched scenarios
and worker counts with the browser report before claiming a speedup.

#### Full local TUI CI gate

From the repository root, run:

```sh
./scripts/check-tui.sh
```

This runs frozen installs, type checks and unit tests for the TUI, WebUI,
Bun adapter and TUI SDK, builds `hya` and `xtask`, then runs the complete
PTY suite with four workers and no retries. Use the CI-pinned Bun version
(currently 1.4.2) and repository Rust toolchain. No browser installation is
needed. The script stops at the first failed command.

`HYA_TUI_CHECK_DIR` sets the artifact directory (default
`~/data/hya-tui-check`): temporary workspaces go in `tmp/`, terminal artifacts
and `parity.json` in `results/`, and Bun's cache in `bun-cache/`.
`HYA_TUI_TEST_WORKERS` sets the worker count (default `4`, matching GitHub).
Existing `BUN_INSTALL_CACHE_DIR` overrides are honored. Cargo uses the normal
target configuration; keep `target` on the data disk. For example:

```sh
HYA_TUI_CHECK_DIR="$HOME/data/hya-tui-prepush" ./scripts/check-tui.sh
```

Run this gate before pushing when requested or when validating a broad TUI
integration. Focused specs remain the normal development loop.

#### Keeping the terminal gate aligned with the TUI

Update the shared specs in the same change as a UI contract change. The
conversation has no permanent metadata heading: read session, model, server,
and permission fields through `/status`, and assert workflow results in the
transcript, prompt dock, picker, or backend API. Do not reintroduce removed
headings to satisfy a test. CI runs `bun run test:tui-exp` with every shared terminal scenario.

The shared helpers in `e2e/hya.ts` define these test interfaces:

| Helper | Contract |
| --- | --- |
| `showStatusView(term): Promise<void>` | Open `/status` and wait for its version field. |
| `statusField(term, field): Promise<string>` | Read a named field from the explicit status snapshot; leave that view open. |
| `showConversation(term): Promise<void>` | Run `/layout show` and wait for the status view to close. |
| `statusSessionId(term, timeout?): Promise<string>` | Read an untitled selected `hysec_…` session ID, then return to Conversation. |
| `expectStatus(term, field, expected): Promise<void>` | Wait for a string or regular expression to match a metadata field, refreshing the snapshot on mismatch, then return to Conversation. |
| `createSession(term): Promise<string>` | Run `/new`, wait for a different selected session ID, and return it. |
| `wideViewport` | `{ width: 1500, height: 640 }`, enough for the right sidebar's 150-column minimum. |

For example, a model switch test can run `/model fake/slow`, then
`await expectStatus(term, "Model", "fake/slow")` before sending a prompt and
checking the reply's model label. Helpers that run commands add entries to
command history; history tests must account for that. Read named sessions
with `expectStatus(term, "Session", title)` instead of `statusSessionId`.

Use the default 1100×640 viewport for the main conversation and command
overlays, `wideViewport` for sidebar assertions, and about 80 columns for
narrow-layout checks. Assert actual terminal glyphs and colors with the
`Tui` fixture; wait for modal disappearance or workflow completion with
`expect.poll` rather than reading the screen immediately after a key.
Vim tests observe DECSCUSR cursor-shape sequences through xterm's public
parser API and verify editing behavior, without depending on a mode banner.

The GitHub `tui` job always uploads `tui-exp-results`, containing `parity.json`,
terminal screen text, parsed frames and raw output. Inspect failed tests in
that artifact. Browser-only cases are explicitly excluded; terminal tests
are not excluded to accommodate UI changes.

`e2e/hya-tui.spec.ts` needs a built backend. Run
`cargo build -p hya-backend --bin hya -p xtask --bin xtask` first, or set
`HYA_BIN` to another `hya` binary. The disk-inspector install spec also invokes
`target/debug/xtask package-bundle <source> <archive>` to test the real package;
building only `hya` does not provide that executable. The `backend` fixture
(`e2e/hya.ts`) starts `hya serve` on a free port. It uses a throwaway `--db` and a temporary `HOME` and `XDG_*`
directories, so your own config and keys are never read. `hyaTui(backend)`
returns the argv that runs `packages/hya-tui` against that backend. The TUI
dependencies must be installed (`bun install` in `packages/hya-tui`).
`HYA_TUI_MAIN=<path to another checkout's packages/hya-tui/src/main.ts>`
runs the specs against that TUI instead (for example an older revision, to
show that a new spec fails without the change it covers).

Daemon reconnect and launch specs read related `/status` fields from one
terminal-buffer snapshot and reopen the status view while waiting for a PID.
Launch specs use the shared session-id reader so startup cannot leave them
waiting on a status view that has already returned to Conversation.
Reconnection restores the conversation asynchronously, so a prior successful
text wait does not guarantee a later screen read still contains the field.

### CI

`.github/workflows/ci.yml`'s `tui` job runs this suite on every push and pull
request, alongside the Rust `lint`, `test`, and `e2e` jobs. It installs the
pinned Bun (the same `bun-v1.4.2` install used by the release workflow), runs
`bun install --frozen-lockfile` in `packages/hya-tui`, `packages/hya-tui-web`,
and `crates/hya-plugin-bun/adapter`, then `bun run typecheck && bun test` in
`hya-tui` and the adapter and `bun run typecheck && bun test ./test` in
`hya-tui-web`. It builds `hya` and the bundle packager
(`cargo build --locked -p hya-backend --bin hya -p xtask --bin xtask`)
with the same Rust toolchain/cache actions as the Rust jobs, sets `HYA_BIN` to
that binary, and runs `bun run test:tui-exp` from `packages/hya-tui-web`.
The TUI SDK also receives frozen installation, typecheck and unit tests.
The required matrix uses four workers and no retries. Artifacts go under
`runner.temp` and upload as `tui-exp-results`, even on failure. There is no
Chromium installation or separate non-blocking experimental job.

Reproduce the complete job locally with `./scripts/check-tui.sh`, or use the
focused commands in [the terminal suite section](#experimental-direct-pty-suite-tui-exp).

**A TUI that starts its own backend.** `launchTest` (from `e2e/hya.ts`) is
`test` with a `workspace` fixture in place of `backend`: the same isolated
HOME/`XDG_*` directories, config (`model`, `projectBundles` options), and
workspace directory, but no running server. `selfLaunch(workspace, extra?,
options?)` returns the `tui()` arguments that run the TUI with no
`--server` and `HYA_BIN` set to the binary under test, so the TUI starts
a detached `hya serve` daemon; the fixture stops it on cleanup. Its default database lands under the
workspace's `XDG_STATE_HOME`, so a second launch in the same test sees the
first one's sessions (`--continue`). See `e2e/hya-tui-launch.spec.ts`:

```ts
import { expect, launchTest as test, selfLaunch, statusSessionId, textStep } from "./hya"

test.use({ model: { steps: [textStep("hi")] } })
test("launches", async ({ tui, workspace }) => {
  const term = await tui(...selfLaunch(workspace))
  await statusSessionId(term)
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
them. xterm.js does not implement the kitty keyboard protocol or
modifyOtherKeys, so a host started with `--shift-enter-lf` translates
`press("Shift+Enter")` to LF before forwarding it to the PTY. Plain Enter sends
CR; `press("Alt+Enter")` sends ESC CR; `press("Control+j")` sends LF;
`press("Shift+Tab")` sends CSI Z (`ESC [ Z`) — xterm.js keeps the key, the
browser does not move focus — which OpenTUI reports as a shifted `tab`.
After `press("Escape")`, wait for its effect before the next key: a lone ESC
followed at once by another key can be read as Alt+that key. To paste, call
`page.evaluate(() => window.hyaTerm.term.paste(text))`: xterm.js turns line
feeds into CRs and wraps the text in bracketed-paste markers when the program
enabled mode 2004 (OpenTUI does). `type()` types key by key and is not a paste.

### The fake model

`e2e/fake-model.ts` (`startFakeModel(steps)`) serves scripted SSE, one `Step`
consumed per model request, on two routes. The request path picks the wire
protocol, so the same steps work on either:

| Route | Protocol | hya provider kind | What hya decodes |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | `chat` (default) | `openai-compatible` | `delta.content`, `delta.reasoning_content`, `delta.tool_calls`, `finish_reason`, trailing `usage` (`crates/hya-provider/src/openai/decoder.rs`). |
| `POST /v1/responses` | `responses` | `openai-response` | `response.reasoning_summary_text.delta`, `response.output_item.added/done` (reasoning and `function_call` items), `response.function_call_arguments.delta`, `response.output_text.delta/done`, and the typed terminal `response.completed` or `response.incomplete` (`crates/hya-provider/src/openai/response_decoder.rs`). The stream never sends `[DONE]`. |

It mirrors the process-level Rust reference (`crates/hya-e2e/src/fake_llm.rs`)
but runs inside the Playwright/Node process, so a spec needs no extra binary.
The fake model emits reasoning only on the `responses` protocol; on `chat` a
`reasoningStep` streams just its answer text. A Chat provider that sends
`delta.reasoning_content` produces reasoning parts in the TUI.

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
  fakeModel!.route("NEVER call `report`", [toolStep("task", { description: "survey", prompt: "list files", subagent_type: "hya-task" }), textStep("spawned")])
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
skip `tui()` and use `fetch` directly against `backend.url`; a scoped rpc
names `backend.dir` in its `directory` field (query parameter on GET, body
field otherwise; the server refuses the removed `x-hya-directory` header);
see `e2e/fake-model.spec.ts`.

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
| `type(text)` / `press(key)` | Keyboard input (`"Enter"`, `"Control+C"`, …), encoded for the selected driver. Use `paste(text)` for bracketed paste. |
| `resize(width, height)` | Resizes the viewport, waits for a new grid size, and returns it. |
| `waitForExit(timeout?)` | Child exit code. |
| `attach(testInfo, name)` | Browser driver attaches PNG/text; PTY driver saves screen text, frames and raw output, attaching the text. |

### Library

`src/host.ts` exports `startHost({ command, cwd?, env?, hostname?, port?, stopGraceMs? })`.
It returns `{ url, stop() }`; `stop()` resolves once every PTY process has
exited (SIGHUP, then SIGKILL to its process group after `stopGraceMs`,
default 3000). The spawned command gets
`TERM=xterm-256color` and `COLORTERM=truecolor`. `src/frames.ts` exports the
frame codec (`encodeClientFrame`, `decodeClientFrame`, `encodeServerFrame`,
`decodeServerFrame`).
