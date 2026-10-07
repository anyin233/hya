# TUI Extension SDK

`@hya/tui-sdk` (`packages/hya-tui-sdk`) is the authoring SDK for bundle TUI
extensions ([Bundle-owned TUI extensions](tui-extensions.md)). Its version
(`TUI_EXTENSION_SDK_VERSION`, now `1.0.0`) is independent of the frontend and
backend releases; the wire generation is `TUI_EXTENSION_API_VERSION` (`1`).
The frontend release ships the SDK next to the TUI (`lib/hya/tui-sdk`); the
extension process bundles it with the extension, so extensions import it
without installing anything.

## Usage

```ts
import { defineTuiExtension } from "@hya/tui-sdk"

export default defineTuiExtension({
  activate(api, context) {
    api.registerPanel({ id: "hello", title: "Hello", render: ({ width }) => `Hello (${width} columns)` })
    api.registerInterceptor({
      id: "no-secrets", target: "submit",
      intercept: ({ text }) => /BEGIN [A-Z ]*PRIVATE KEY/.test(text) ? { decision: "block", message: "looks like a private key" } : undefined,
    })
  },
  deactivate() { /* release resources */ },
})
```

- `PanelOptions` supports `replaces` (including `context_line` and
  `project_view`), `refreshMs`, `onAction`, `onKey`, and `onResult`. `onKey`
  requires the `tui.keys` manifest permission; `onResult` receives a
  tokenized host-command result.
- Render callbacks receive `{ context, width, height, now }`; `now` is host
  epoch milliseconds. Actions include `button: "left" | "right"`.
- Handlers return a notice, an invalidation decision, and/or ordered host
  commands. Commands include session/project controls, `fs.complete`,
  `ui.release`, `ui.close`, and `ui.open` (only for `project_view`);
  tokenized commands produce `CommandResult`. Action, key, and result handlers
  all process returned commands, and result handlers may continue a sequence.
- `ExtensionContext` has permission-scoped `sessions`, `projects`, `todos`, and
  redesigned `status` sections. Sessions and Projects expose the host's shared
  `ready` flag; Projects also expose a last-read `error`.
- `RenderNode` boxes are passive and borderless; titles render as plain headings. The legacy `border` field is accepted but ignored by the host.
- `RenderNode` style colors include the `success` and `border` theme tokens.
- Row text never wraps, other text wraps at words, and clickable text is an
  underlined accent link unless an explicit style is supplied.

## Runtime

Extensions run in QuickJS (ES2023) compiled to WebAssembly, not in Bun: only
ECMAScript built-ins, `console`, and `api.fs` exist — no `process`, `fetch`,
`require`, timers, `Bun`, or Node modules. TypeScript is fine. Imports: relative
paths within the bundle and `@hya/tui-sdk`; anything else fails the start with
`cannot import "<name>"`. Each request has 2 s of execution (`VM_DEADLINE_MS`;
waiting for `api.fs` does not count) and the whole extension 64 MiB of memory;
exceeding either answers that request with an error. After a deadline error
(`VM_STOPPED_ERROR_CODE`) the extension refuses further requests and the TUI
restarts it.

A trusted extension (`/extensions trust`) runs the same bundle on
JavaScriptCore's JIT in a `node:vm` realm with the same globals, deadline, and
imports, but no memory cap. Code must not depend on which engine runs it.

`api.fs` (permission `fs.read`) reads the active Project: `read(path)`,
`list(path)` (`FsEntry[]`), `stat(path)` (`FsStat | null`), and
`watch(path, handler, { recursive? })`, which returns an unsubscribe function.
Handlers receive `FsEvent[]` batches and return a `HandlerResult`; a `closed`
event ends the watch. Limits and path rules: `docs/tui-extensions.md`, File
access.

## Interface

| Export | Purpose |
| --- | --- |
| `defineTuiExtension(extension)` | Typed identity for the default export (`activate`, optional `deactivate`). |
| `TuiExtensionApi` | `registerPanel`, `registerStatusItem`, `registerRenderer`, `registerFormatter`, `registerInterceptor`, `onContext`, `fs`. |
| `TuiFsApi`, `FsEntry`, `FsStat`, `FsEvent` | Read-only Project file access. |
| `PanelOptions`, `KeyEvent`, `HostCommand`, `CommandResult`, `HandlerResult` | Panel callbacks, captured keys, host commands, token results, and handler results. |
| `RenderNode`, `TextStyle`, `NodeAction`, `RENDER_LIMITS` | Render trees, including `success` and `border` theme tokens, and host limits. |
| `ExtensionContext`, `ToolCallInput`, `FormatInput`, `InterceptDecision` | Permission-scoped callback inputs and interceptor decisions. |
| `TuiMethodMap`, `Contributions`, `parseFrame`, `encodeFrame` | The JSON-RPC contract, including `tui/key` and `tui/command_result`. |
| `TuiExtensionRunner` | Dispatches host requests to one extension; runs inside the VM. |
| `ExtensionVm`, `bundleExtension` (`src/host.ts`) | The host side: bundle an entry and run it in a capped QuickJS VM. |

`src/main.ts` is the shared host the TUI starts (`bun main.ts`): it loads
extensions on request (`host/load`), runs each in its own VM on its VM thread
(`src/worker.ts`) or, for `jit` loads, in a realm on its own thread
(`src/jitThread.ts`, `src/jitWorker.ts`), relays `api.fs` calls to the TUI,
and sends `console.*` output to stderr as `{ ext, line }`, which `/extensions`
shows.

Development: `cd packages/hya-tui-sdk && bun run typecheck && bun test`.
