# Bundle-owned TUI extensions

A bundle can ship TypeScript that customizes the TUI: new panels (or a
replacement for a built-in pane), rows in the Context box, renderers for tool
cards and the composer, tool-output formatters, and interceptors for submitted
prompts. Each extension runs in an isolated JavaScript VM inside its own
process, confined by the operating system's sandbox where available; the TUI
talks to it over JSON-RPC and draws what it returns with its own widgets.

The extension is part of its bundle (a `tui:` manifest section), and its
authoring SDK, `@hya/tui-sdk`, has its own version, independent of the frontend
and backend releases ([TUI Extension SDK](tui-extension-sdk.md)).

## Usage

Declare the extension in the bundle manifest and package its files as
`extensions.files` (or `extensions.js`) resources:

```yaml
kind: Plugin
identity: { id: acme/git-ui, version: 1.0.0, publisher: acme }
extensions:
  files:
    - { id: tui, path: tui/main.ts }
    - { id: tui-lib, path: tui/git.ts }
tui:
  api_version: 1
  entry: tui/main.ts
  sdk: 1.0.0
  permissions: [tui.panel, tui.status_item, tui.render, workspace.git.read, tui.action]
```

```ts
// tui/main.ts
import { defineTuiExtension } from "@hya/tui-sdk"

export default defineTuiExtension({
  activate(api) {
    api.registerPanel({
      id: "git", title: "Git",
      render: ({ context }) => ({ kind: "column", children: [
        { kind: "text", text: `⎇ ${context.git?.branch ?? "detached"}`, style: { color: "accent", bold: true } },
        { kind: "text", text: `${context.git?.dirty ?? 0} changed · ↑${context.git?.ahead ?? 0} ↓${context.git?.behind ?? 0}` },
      ] }),
    })
    api.registerStatusItem({ id: "changes", label: "Changes", priority: 6, render: ({ context }) => String(context.git?.dirty ?? 0) })
  },
})
```

Install the bundle as usual (project bundles in `<project>/.hya/bundles/`, or
`hya bundle install`). When the TUI connects to a backend, or switches Project or
server, it reads the scope's extensions (`GET /v1/tui-extensions`) and starts
them. `/extensions` shows each one: state (`starting`, `running`, `failed`,
`disabled`, `blocked`), where it runs (`VM` or `JIT (trusted)`, each `+ OS sandbox` or `only`), its permissions, its
contributions, recent warnings, and the last lines of its stderr.

| Command | Effect |
| --- | --- |
| `/extensions` | List the scope's extensions and their contributions. |
| `/extensions disable <bundle id>` | Stop it and remember the choice (`extensionEnabled` in the TUI preferences). |
| `/extensions enable <bundle id>` | Start it and remember the choice; required once for a remote backend's extension. |
| `/extensions reload <bundle id>` | Restart it (resets its restart budget). |
| `/extensions trust <bundle id>` | Run it on the JIT tier and remember the choice (`extensionTrusted`); see Security. |
| `/extensions untrust <bundle id>` | Run it in the VM and remember the choice. |
| `/bundles` | The full-screen [Bundles view](tui.md#bundles): install, uninstall, enable, disable, and trust whole bundles (backend components and TUI extension together). |
| `/extensions sandbox <required\|best-effort\|disabled>` | Set the OS sandbox policy (`extensionSandbox`) and restart running extensions. The VM always applies. |
| `/layout split <up\|left> extension <bundle id>#<panel id>` | Show a panel in a new pane; `/layout assign extension <key>` puts it in the active pane. |

## Manifest contract

The top-level `tui` object is optional; unknown fields are errors, and the
declaration is covered by the prepared bundle's digest.

| Field | Type | Rules |
| --- | --- | --- |
| `api_version` | integer | Required; `1`. Selects the wire contract below. |
| `entry` | string | Required; a canonical relative `.ts`, `.js`, or `.mjs` path naming a non-binary extension resource of the bundle. |
| `sdk` | string | Required; `major.minor[.patch]` with major `1`. The TUI runs the extension only when it ships an SDK with the same major and a minor at least this one. |
| `permissions` | array of strings | Optional (default empty); each from the allowlist below, no duplicates. |

| Permission | Grants |
| --- | --- |
| `tui.panel` | Panels. |
| `tui.status_item` | Context box rows. |
| `tui.render` | Tool-card and composer renderers, tool-output formatters. |
| `tui.action` | Clickable nodes (`action`) and submit interceptors. |
| `tui.session.read` | `context.session`: id, title, agent, model, permission mode, directory, busy. |
| `tui.transcript.read` | `context.transcript` (the open session's last 50 messages as role + text) and tool input/output/error in tool-card renderers. |
| `tui.workspace.read` | `context.workspace`: the directory and Project (id, name). |
| `workspace.git.read` | `context.git`: branch, head, changed-file count, ahead/behind (the backend's `GetVcsStatus`). |
| `tui.sessions.read` | `context.sessions`: readiness, selected session, and sidebar-scope session rows (including nesting, busy/waiting, archive, and member handles). |
| `tui.projects.read` | `context.projects`: readiness, active Project, roots, busy state, session counts, and `error` (`<code>: <message>`) from the last failed list read. Sessions and Projects currently share the host's single `ready` flag. |
| `tui.todos.read` | `context.todos`: the open session's ordered todo items and status. |
| `tui.status.read` | `context.status`: readiness, Vim state, mode, session/model/context/token facts, branch, todo counts, extension items, server, WebUI, versions, and connection state behind the Context box and narrow status line. |
| `tui.keys` | `PanelOptions.onKey`; the panel receives captured key events. |
| `tui.session.control` | Session host commands (`session.open`, `session.menu`, `session.new_temporary`). |
| `tui.project.control` | Project and filesystem host commands (`project.*`, `fs.complete`). |
| `fs.read` | `api.fs`: read, list, stat, and watch files under the active Project's roots (see File access). |

Context always includes `terminal` dimensions. The other sections are omitted
unless their permission is present: `session`, `transcript`, `workspace`, `git`,
`sessions`, `projects`, `todos`, and `status`. Context updates are delivered at
most every 200 ms and only when the permission-scoped JSON changes.

A contribution without its permission is dropped with a warning in
`/extensions`; context sections without their permission are never sent.
`context.status` contains `ready`, optional `vim` (`normal`, `pending`),
`mode` (`text`, `tone`), optional `session` (id, title, agent, fork source,
model and short model, message count, workdir, context percent/tokens/limit,
and billed tokens), branch, todo counts, extension `items`, server host,
optional WebUI `{ host?, label }`, versions, and connection
(`connected`, `disconnected`, or `stopped`). The bundle preserves the Context
box's field order; extension status items follow Todos and precede Server.

Rendering is asynchronous. While the catalog has not loaded, or while
`hya/basic-tui-components` is starting, built-in surfaces show `Loading…` and
the status line is empty. Afterwards, a surface without a replacement shows
`<Pane> needs hya/basic-tui-components (<reason>)`. Reasons are `not installed`,
`extension catalog unavailable: …`, or the extension state and its reason.
Any rendering failure — a timeout, a crash, or an invalid tree — falls back to
the built-in rendering; panels show the error text.

The render callback receives `{ context, width, height, now }`; `now` is the
host epoch time in milliseconds (or 0 when no clock is supplied). `onAction`
receives `{ name, data, button }`, where `button` is `left` or `right`.
Returning a string is a notice; an object may request invalidation and host
commands. `onKey` and `onResult` use the same result shape.

### Replaceable surfaces and missing replacements

In addition to replaceable panes, `project_view` is the full-screen Projects
view. `context_line` remains a supported SDK contribution target, but this
frontend does not draw an implicit status line above the conversation; place
a Context or Status pane explicitly for metadata.
The conversation pane itself is never replaceable. If a surface has no
replacement because `hya/basic-tui-components` is not installed or running,
the host displays `<Surface> needs hya/basic-tui-components (<reason>)`.

## What an extension can change

| Contribution | API | Where it shows |
| --- | --- | --- |
| Panel | `registerPanel({ id, title, render, placement?, replaces?, refreshMs?, onAction?, onKey?, onResult? })` | `placement: "sidebar"` (default): placed once as a selectable extension leaf at the right of the v4 layout root, without changing focus; the Layout editor can move, resize, or remove it. `placement: "pane"`: only where `/layout` puts it. `replaces` may name a pane or surface (`projects`, `sessions`, `todos`, `context`, `jobs`, `status`, `models`, `workflows`, `interactions`, `api`, `context_line`, `project_view`); the conversation pane cannot be replaced. `onKey` needs `tui.keys` and handles captured keys; `onResult` receives a result for a tokenized host command. `refreshMs` is at least 1000 ms. Among replacements, a non-first-party bundle wins over first-party bundles; ties are by bundle id. |
| Status item | `registerStatusItem({ id, label, render, priority? })` | A Context box row (`label  value`) before the connection rows, and a segment of the narrow status line; `priority` 1–9 is its drop order there. Returning nothing hides it. |
| Tool-card renderer | `registerRenderer({ id, target: "tool_call", mode, priority?, render(input) })` | `replace`: the highest priority (ties: the last bundle id) draws the card instead of the built-in one; returning `null` keeps the built-in card for that call. `decorate`: a tree with one `{ kind: "slot" }` wraps the card (or the replacement); higher priorities wrap outermost. |
| Composer decoration | `registerRenderer({ id, target: "composer", mode: "decorate", render })` | A `column` with `{ kind: "slot" }` as a direct child: rows before the slot are drawn above the composer, rows after it below. The composer itself, its keys, and its input always stay the host's. |
| Formatter | `registerFormatter({ id, priority?, format({ value, text }) })` | Tool output text in tool cards: formatters run in ascending priority, each receiving the previous text; returning nothing keeps it. |
| Submit interceptor | `registerInterceptor({ id, target: "submit", priority?, intercept({ text }) })` | Before a prompt is sent (not `/commands` or `!shell`): `{ decision: "replace", text }` rewrites it, `{ decision: "block", message }` refuses it (`Not sent · <bundle>: <message>`). Highest priority first. |
| Context handler | `onContext(handler)` | Called on every context change; return `false` to keep the rendered surfaces. Otherwise all of the extension's surfaces re-render. |


## Wire contract (api_version 1)

The TUI starts one shared extension host, `bun <sdk>/src/main.ts` (through the
OS launcher, see Security), for every extension. The host bundles each entry,
runs it in its own VM on the host's VM thread (a trusted extension: in its
own JavaScriptCore realm on its own thread, see Security), and speaks
newline-delimited JSON-RPC 2.0 on stdin/stdout. TUI → host: `host/load { ext,
entry, permissions, jit }`, `host/unload { ext }`, and `ext/call { ext, request }`, whose
`request` is one of the extension methods below and whose result is
`{ response }`. Host → TUI: `fs/read`, `fs/list`, `fs/stat`, `fs/watch`, and
`fs/unwatch` (see File access). Extension `console.*` output goes to stderr as
`{ ext, line }` JSON lines. Extension methods, all host → extension:

| Method | Parameters | Result |
| --- | --- | --- |
| `tui/initialize` | `{ api_version: 1, sdk_version, extension_id, permissions }` | `{ api_version: 1, sdk_version }` |
| `tui/activate` | `{ context }` | `{ contributions: { panels, status_items, renderers, formatters, interceptors } }` |
| `tui/render` | `{ surface: { kind: "panel" \| "status" \| "renderer", id }, width, height, now, input? }` | `{ root: RenderNode \| null }` |
| `tui/format` | `{ formatter, input: { value, text }, width }` | `{ text: string \| null }` |
| `tui/intercept` | `{ interceptor, input: { text } }` | `{ decision: "continue" } \| { decision: "replace", text } \| { decision: "block", message }` |
| `tui/context` | `{ context }` | `{ invalidate: boolean }` |
| `tui/action` | `{ surface: { kind: "panel" \| "renderer", id }, action: { name, data, button } }` | `{ invalidate: boolean, notice?, commands? }` |
| `tui/key` | `{ surface: { kind: "panel", id }, key: { name, sequence, ctrl, shift, meta } }` | `{ invalidate: boolean, notice?, commands? }` |
| `tui/command_result` | `{ surface: { kind: "panel", id }, result: { token, ok, error?, value? } }` | `{ invalidate: boolean, notice?, commands? }` |
| `tui/shutdown` | `{}` | `null`; the TUI then unloads the extension |
| `tui/fs_event` | `{ watch, events: [{ path, kind: "change" \| "rename" \| "closed" }] }` | `{ invalidate: boolean, notice? }` |

### File access

Extensions with `fs.read` may read the active Project through `api.fs`:
`read(path)` (UTF-8 text, at most 1 MiB), `list(path)` (at most 2000 entries),
`stat(path)` (`null` when missing), and `watch(path, handler, { recursive })`.
Paths are relative to the Project's primary root, or absolute. The TUI resolves
each path through `realpath` and refuses any path outside every root,
including `..` and symlinks that lead out, with `outside the Project roots`;
without an open Project every call fails with `no Project is open`. Each
extension has at most 16 watches. Changes reach the handler in batches every
100 ms, at most 100 events per batch. A watch ends with a `closed` event when
the TUI refuses it, its path disappears, or the active Project changes so that
the path lies outside the new roots. Unloading an extension closes its watches.
There is no write access.

### Host commands

Handlers may return commands. The host validates each command, checks its
permission, and executes commands in the order returned; keyboard requests for
one panel are serialized. `ui.release`, `ui.close`, and `ui.open` are run by
the host itself. `ui.open` is restricted to the `project_view` replacement and
shows that panel as the overlay again. Commands with a `token` receive a later
`tui/command_result` with `{ token, ok, value? }`, or `{ token, ok: false,
error }` when execution fails. The `tui/action`, `tui/key`, and
`tui/command_result` handlers all apply notices, invalidation, and returned
commands; a command-result handler may therefore issue more commands.

| Command | Permission | Token result |
| --- | --- | --- |
| `session.open(id, token?)` | `tui.session.control` | optional |
| `session.menu(id)` | `tui.session.control` | none |
| `session.new_temporary` | `tui.session.control` | optional |
| `project.switch(id)` | `tui.project.control` | optional |
| `project.menu(id)` | `tui.project.control` | none |
| `project.create(name, roots)` | `tui.project.control` | optional |
| `project.rename(id, name)` | `tui.project.control` | optional |
| `project.set_roots(id, roots)` | `tui.project.control` | optional |
| `project.delete(id)` | `tui.project.control` | optional |
| `fs.complete(input)` | `tui.project.control` | required; value contains completion/candidates |
| `ui.release` | none | none |
| `ui.close` | none | none |
| `ui.open` | none; only `project_view` may use it | none |

### Keyboard capture

A panel declaring `onKey` with `tui.keys` can receive keys while its pane or
overlay owns focus. The built-in Projects sidebar is reached with Alt+Arrow or `/layout focus`;
captured keys are delivered through `tui/key`. A handler can return
`ui.release` to return focus to the message editor, or `ui.close` to close its overlay. Keys are not available without
`tui.keys`.

`RenderNode` is a tagged object (`kind`):

```json
{ "kind": "text", "text": "Review complete", "style": { "color": "accent", "bold": true }, "action": { "name": "open", "data": 3 } }
{ "kind": "row", "children": [], "gap": 1 }
{ "kind": "column", "children": [] }
{ "kind": "box", "title": "Summary", "children": [], "border": true, "padding": 1 }
{ "kind": "table", "columns": ["File", "Status"], "rows": [["a.rs", "M"]] }
{ "kind": "progress", "value": 3, "total": 4, "label": "Tests" }
{ "kind": "slot" }
```

`style.color` and `style.background` are `#rrggbb` or a theme token (`fg`,
`accent`, `muted`, `error`, `warning`, `success`, `border`). `gap` and `padding` are integers 0–100.
`action` (clickable text) needs `tui.action`; without it the text is drawn
inert. `slot` is allowed only in decorating renderers, exactly once. The host
rejects unknown kinds or fields, trees deeper than 32 or larger than 2000
nodes, and strings over 32 KiB, and strips terminal escape and control
characters.
Text in a `row` is clipped at the edge and never wraps; other text wraps at
word boundaries. An unstyled clickable text is rendered as an accented,
underlined link. A styled clickable text keeps exactly its supplied style.

## Catalog and files

`ListTuiExtensions` returns, for each bundle of the scope with a `tui:`
section, the bundle id, version, `firstParty` trust flag, prepared digest, the
declaration, and every non-binary extension resource as `{ path, sha256,
content }`.
TUI checks every file's sha256 and path (relative, no `..`) and writes the files
read-only under `$XDG_CACHE_HOME/hya/tui-extensions/<prepared digest>/` (else
`~/.cache/…`). It sends the digests already complete there as `known=<digest>,…`
(at most 64); for those the catalog returns `cached: true` and no files. The
host bundles the files with `Bun.build`: relative
imports between the bundle's files and `@hya/tui-sdk` (the SDK shipped next to
the TUI) resolve; any other import (`node:*`, packages) fails the start with
`cannot import "<name>"`.

### Startup

The TUI starts the host while the catalog request is in flight and stops it
when no extension runs. The sandbox probe result is cached in
`<cache>/sandbox-probe.json`, keyed by the Bun binary, the launcher, and the OS.
`bun run bench` in `packages/hya-tui` measures 20 extensions with full panels,
10 of them reading and watching files, in the VM and on the JIT tier. On macOS
arm64, with the catalog arriving 100 ms after the fetch starts, every panel
renders 95–98 ms after it in the VM and 51–57 ms on the JIT tier.

### `hya/basic-tui-components`

The trusted `hya/basic-tui-components` bundle provides the Sessions, Todos,
Projects sidebar, Context pane, and full-screen
Projects view. It is an ordinary TUI extension: another bundle can replace one
of those panels or surfaces by registering a panel with the matching
`replaces` value. Replacement precedence favors a non-first-party bundle.

On a remote backend, catalog entries marked `firstParty` are trusted and may
start without `/extensions enable`; other extensions remain blocked until
explicitly enabled. The catalog flag reflects the backend's trusted
first-party inventory, not the bundle id alone.

### Trust tiers

An extension runs in the QuickJS VM unless it is trusted. Trusted extensions
run on JavaScriptCore with its JIT: rendering is 6–10x faster (a 300-session
Sessions render takes 0.44 ms instead of 4.45 ms) and startup shorter, at the
cost of the limits under Security. `/extensions trust|untrust`
decides per bundle; without a preference, first-party bundles of a local
backend are trusted and everything else is not (a remote backend's first-party
bundles stay in the VM).

## Security

- **Extension VM** (untrusted extensions). The bundled extension and the SDK runner execute
  in QuickJS compiled to WebAssembly (`packages/hya-tui-sdk/src/host.ts`). The
  VM has only ECMAScript built-ins, a `console`, and `api.fs` (served by the
  TUI): no network, processes, timers, environment, or Bun/Node APIs. Its
  memory is one 64 MiB `WebAssembly.Memory` (a hard cap; QuickJS's own limit
  is not), and each request has a 2 s deadline for VM execution (time waiting
  for `api.fs` does not count). A handler past the deadline is answered with
  error code `-32001`; from then on that VM answers every request with the
  same error, and the TUI fails the extension and restarts it within its
  budget, so a runaway stalls the shared VM thread at most once per start. A
  promise that cannot settle (nothing outstanding) is answered with an error
  at once.
- **JIT tier** (trusted extensions only). The bundle runs in a fresh
  `node:vm` realm on its own thread (`src/jitWorker.ts`): no `Bun`, `process`,
  `require`, `fetch`, timers, or module loader, and the realm's global is
  built from a null-prototype object so no host `Function` is reachable; the
  host only exchanges strings with it. The 2 s deadline (with `api.fs` waits
  excluded) is enforced by terminating the thread, after which the extension
  answers `-32001` until restarted. There is **no memory cap**: a trusted
  extension can exhaust the host's memory, which stops every extension until
  the host restarts. The boundary is the JavaScriptCore realm plus the OS
  sandbox, not WebAssembly; trust only code you would run unsandboxed.
- **Host isolation.** One host process for all extensions, started with an
  empty environment (no tokens, keys, or paths); each extension has its own VM
  and heap. A crash or hang never stops the TUI: requests time out after 5 s
  (the extension is unloaded and restarted; the host keeps running), lines are
  limited to 2 MiB, at most 512 requests are pending, and a malformed frame
  stops the host. When the host dies, every extension fails and restarts after
  1 s, at most 3 starts per minute each; then it stays `failed` until
  `/extensions reload`. An extension that fails to start (a bundle error) is
  not restarted; its error is the reason shown.
- **OS sandbox** (no helper binary). The process starts through
  `packages/hya-tui/src/extensions/confine.ts`, which restricts itself and
  then `execve`s the extension runtime, so every thread inherits the
  restriction. Linux: `no_new_privs`, Landlock (read-only access to system
  libraries, the Bun install, the SDK, and the extension files; execute only
  in the Bun binary's directory and its ELF interpreter; its own `/proc/<pid>`
  only; no writes except `/dev/null`; no TCP; signals and abstract sockets
  scoped) and a seccomp filter refusing `socket`/`socketpair` (x64, arm64). macOS:
  `sandbox_init` (libsandbox) with a deny-default profile: the same reads, exec
  of the Bun binary only, no writes, no network. Both run unprivileged and in
  Docker's default container. Before first use the TUI runs the launcher with
  `bun --version`; where it cannot confine (a kernel without Landlock, another
  platform) `best-effort` runs with the VM only and a warning, `required`
  refuses to start, and `disabled` never confines.
- **Capabilities.** Extensions see only the context their permissions grant
  and can only return data. They cannot call the backend, change sessions, or
  replace keys, quitting, cancelling, or permission prompts.
- **Remote backends.** When the TUI is connected to a backend on another
  machine (`hya --connect`, `/connect-remote`, or a non-loopback `--server`),
  its extensions are `blocked` until `/extensions enable <bundle id>`.

## Development

| Path | Role |
| --- | --- |
| `crates/hya-bundle` (`SourceTuiExtension`, `PreparedTuiExtension`, `prepare_tui`) | Manifest parsing, validation, digest. |
| `crates/hya-server/src/v1/catalog.rs` (`list_tui_extensions`) | The catalog route. |
| `packages/hya-tui-sdk` | Authoring SDK, the extension VM (`src/host.ts`), and the shared host (`src/main.ts`, `src/worker.ts`; JIT tier `src/jitThread.ts`, `src/jitWorker.ts`). |
| `packages/hya-tui/src/extensions/` | Host: `install.ts` (verify, cache), `sandbox.ts` (OS sandbox plan and probe), `confine.ts` (self-confining launcher), `hostChannel.ts` (JSON-RPC to the shared host), `files.ts` (`api.fs` service), `wire.ts` (validation), `manager.ts` (lifecycle and contributions), `context.ts`, `renderTree.tsx`, `Host.tsx`. |

Tests: `cargo test -p hya-bundle --test tui`, the hya-server catalog test,
`packages/hya-tui-sdk` `bun test` (the VM: no ambient APIs, deadline, memory
cap, import rules), `packages/hya-tui` `bun test test/extensions.test.ts` (real
SDK and Bun processes, including the OS launcher where it can confine), and the
WebUI e2e `e2e/hya-tui-extension.spec.ts`. Linux: run the same files in Docker's
default container, e.g. `docker run --rm -v "$PWD":/hya:ro -w
/hya/packages/hya-tui oven/bun:1.4.2 bun test test/extensions.test.ts`.
