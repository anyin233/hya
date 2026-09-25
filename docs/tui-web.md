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
connection. It pairs with the Bun/OpenTUI frontend (`packages/hya-tui`). Until
that frontend lands in this repository, the test suite exercises an OpenTUI
probe fixture (`e2e/fixtures/opentui-probe.ts`). The backend stays unaware of
the host, because rendering never moves into `hya serve` (see
[ADR-0018](adr/0018-browser-rendered-tui-test-environment.md)).

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
| `type(text)` / `press(key)` | Playwright keyboard input (`"Enter"`, `"Control+C"`, …). |
| `resize(width, height)` | Resizes the viewport, waits for a new grid size, and returns it. |
| `waitForExit(timeout?)` | Child exit code. |
| `attach(testInfo, name)` | Writes `<name>.png` and `<name>.txt` to the test output dir and attaches both. |

### Library

`src/host.ts` exports `startHost({ command, cwd?, env?, hostname?, port? })`.
It returns `{ url, stop() }`. The spawned command gets
`TERM=xterm-256color` and `COLORTERM=truecolor`. `src/frames.ts` exports the
frame codec (`encodeClientFrame`, `decodeClientFrame`, `encodeServerFrame`,
`decodeServerFrame`).
