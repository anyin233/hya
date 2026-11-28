# TUI Extensions (`@hya/tui-sdk` v2)

## 1. Introduction

Bundles customize Hya's terminal UI and browser WebUI in TypeScript through **`@hya/tui-sdk` v2** (`api_version: 2`, `sdk: "2.0.0"`). Under the clean-sheet `hya.v2` modular architecture ([ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)), domain UI surfaces—such as subagent panes and viewers, workflow dashboards, channel renderers, and workspace sidebar panels—live in bundles rather than being hardcoded into `packages/hya-tui`.

`@hya/tui-sdk` v2 adds four core capabilities on top of the isolated execution model:

- **Bundle-confined `BundleApi` access (`tui.bundle_api`):** Call your own bundle's backend `/v2/bundles/<bundleId>/api/...` (or session-scoped `/v2/sessions/<sessionId>/bundles/<bundleId>/...`) endpoints via `api.bundleApi` or `{ command: "bundle.api", ... }`, strictly confined to your bundle ID.
- **Bundle-confined extension state (`tui.extension_state.read`):** Read folded session projection state (`Projection::extensions[bundleId]`) synchronously from `context.extension_state` / `api.extensionState.get()` or asynchronously via `api.extensionState.read()`, strictly confined to your bundle ID.
- **Dynamic named panes & slash commands:** Register multi-instance named panes (`api.registerPane`) with default layout slots/sizes, control them via `pane.open` / `pane.pin` / `pane.close` / `pane.focus`, and contribute slash commands (`api.registerCommand`).
- **Host-virtualized streaming transcripts (`tui.transcript.embed`):** Embed `{ type: "transcript", sessionId }` nodes inside extension trees so the host renders live streaming subsession transcripts directly without cloning transcript trees over IPC.

Every TUI extension runs inside an isolated **QuickJS WebAssembly VM** (64 MiB hard memory cap, 2-second per-call execution deadline, plus OS-level Landlock/seccomp on Linux or `sandbox_init` on macOS) with zero access to the network, environment variables, or shell. Trusted local extensions (such as first-party bundles) run on JavaScriptCore's JIT inside a null-prototype realm and the same OS sandbox.

---

## 2. Usage

### Declaring a v2 TUI extension in `bundle.yaml`

Add a `tui:` block with `api_version: 2` and `sdk: "2.0.0"` to your `schema_version: 2` `bundle.yaml` and list your TypeScript sources under `assets:`:

```yaml
kind: Bundle
schema_version: 2
identity:
  id: acme/subagent-ui
  version: 1.0.0
  publisher: acme
assets:
  - { id: tui-entry, path: tui/main.ts }
tui:
  api_version: 2
  entry: tui/main.ts
  sdk: "2.0.0"
  permissions:
    - tui.panel
    - tui.action
    - tui.keys
    - tui.bundle_api
    - tui.extension_state.read
    - tui.transcript.embed
```

### Worked example

Import `defineTuiExtension` and `transcriptNode` from `@hya/tui-sdk` (bundled automatically by the extension host—no `npm install` needed at runtime):

```ts
import { defineTuiExtension, transcriptNode } from "@hya/tui-sdk"

export default defineTuiExtension({
  activate(api) {
    // 1. Dynamic named pane with a host-virtualized streaming transcript
    api.registerPane({
      id: "viewer",
      title: "Worker Viewer",
      defaultSlot: "right",
      defaultSize: { width: "40%", minWidth: 32 },
      pinnable: true,
      closable: true,
      render: ({ instance }) => {
        const state = api.extensionState.get<{ workers?: { id: string; handle: string }[] }>()
        const sessionId = instance ?? state?.workers?.[0]?.id
        if (!sessionId) return { kind: "text", text: "No active worker", style: { color: "muted" } }
        return {
          kind: "column",
          children: [
            { kind: "text", text: `Worker ${sessionId}`, style: { color: "heading", bold: true } },
            transcriptNode(sessionId, { follow: true, compact: true }),
          ],
        }
      },
      onKey: (key, _ctx, meta) => {
        if (key.name === "p") return { commands: [{ command: "pane.pin", pane: "viewer", instance: meta?.instance, pinned: true }] }
        if (key.name === "q") return { commands: [{ command: "pane.close", pane: "viewer", instance: meta?.instance }] }
      },
    })

    // 2. Slash command that queries the bundle's own API and opens the dynamic pane
    api.registerCommand({
      name: "worker",
      description: "Inspect a worker session",
      argumentHint: "<session-id>",
      execute: async ({ args }) => {
        const id = args.trim()
        if (!id) return "Usage: /worker <session-id>"
        await api.bundleApi.post("/inspect", { sessionId: id })
        return {
          notice: `Opened worker ${id}`,
          commands: [{ command: "pane.open", pane: "viewer", instance: id, slot: "right", focus: true }],
        }
      },
    })
  },
})
```

Package and install the bundle (`hya bundle install ...`), or place the folder under `<project>/.hya/bundles/<name>/` and run **`/reload-bundles`**.

### Managing extensions, trust tiers, and `--bare`

- **`/extensions` / `/bundles`:** Inspect loaded extensions, warnings, permissions, and logs.
- **Default VM tier:** Third-party extensions run in QuickJS WebAssembly (64 MiB memory cap). Extensions from a **remote** backend start `blocked` until explicitly enabled with `/extensions enable <bundle-id>`.
- **Trusted JIT tier (`/extensions trust <bundle-id>`):** Runs trusted code on JavaScriptCore's JIT inside a null-prototype realm and OS sandbox (default for local first-party bundles).
- **Manual panel placement:** In the `workspace` layout preset, place a panel with `/layout split left extension <bundle-id>#<panel-id>`.
- **Bare mode (`hya --bare`):** When the backend advertises zero TUI extensions, `ExtensionManager.settled()` resolves immediately, no extension host process runs, and the TUI uses the minimal single-column chat layout while keeping core overlays (`/key`, `/mcp`, `/skills`, `/bundles`, `/rules`, `/diff`) available.
- **Browser-safe keybindings:** Keep custom `onKey` bindings browser-safe—do not rely exclusively on shortcuts reserved by browsers (`Ctrl/Cmd+W`, `T`, `N`, `L`, `Tab`, `Ctrl+Tab`).

---

## 3. Interface Definitions (`api_version: 2`)

### Version constants

| Constant | Value | Meaning |
| --- | --- | --- |
| `TUI_EXTENSION_API_VERSION` | `2` | Wire protocol major version enforced by `tui/initialize` and catalog validation. |
| `TUI_EXTENSION_SDK_VERSION` | `"2.0.0"` | Independent `@hya/tui-sdk` semantic version. |
| `TUI_EXTENSION_SDK_MAJOR` | `2` | SDK contract major version validated by `sdkCompatible`. |
| `MAX_FRAME_BYTES` | `2_097_152` | Maximum UTF-8 byte length of one LF-delimited JSON-RPC frame. |
| `MAX_CONTRIBUTIONS` | `32` | Maximum contributions of each kind per extension activation. |
| `RENDER_LIMITS` | `{ maxDepth: 32, maxNodes: 2000, maxStringBytes: 32768 }` | Validation caps per render tree. |

### Permissions (`TuiExtensionPermission`) & Confinement

An extension only receives context sections, contributions, render nodes, and host commands whose permissions are declared in `tui.permissions`:

| Permission | Scope & behavior |
| --- | --- |
| `tui.panel` | Register `panels` and dynamic `panes`; emit `pane.open`, `pane.pin`, `pane.close`, `pane.focus`. |
| `tui.status_item` | Register `status_items` shown in the Context box and bottom status line. |
| `tui.render` | Register `renderers` (`tool_call`, `composer`) and output `formatters`. |
| `tui.action` | Emit clickable `NodeAction`s, register `interceptors`, and register slash `commands`. |
| `tui.keys` | Receive `tui/key` events on registered panels and dynamic panes (`onKey`). |
| `tui.session.read` | Read `context.session` (active session ID, title, agent, model, mode, workdir, busy). |
| `tui.transcript.read` | Read `context.transcript` (up to 50 recent messages) and tool call `input`/`output`/`error`. |
| `tui.transcript.embed` | Return host-virtualized `{ type: "transcript", sessionId, ... }` nodes in render trees. |
| `tui.extension_state.read` | Read `Projection::extensions[bundleId]` via `context.extension_state`, `api.extensionState.get()`, and `api.extensionState.read()`. **Strictly confined to caller's `bundleId`** unless explicitly authorized by the host. |
| `tui.bundle_api` | Invoke bundle endpoints via `api.bundleApi.*` or `{ command: "bundle.api", ... }`. **Strictly confined to caller's `bundleId`** (`/v2/bundles/<encodeURIComponent(bundleId)>/api<path>` or `/v2/sessions/<sessionId>/bundles/<encodeURIComponent(bundleId)><path>`) unless explicitly authorized by the host. |
| `tui.workspace.read` | Read `context.workspace` (directory and active Project ID/name). |
| `workspace.git.read` | Read `context.git` (branch, HEAD, dirty, ahead, behind). |
| `tui.sessions.read` / `tui.projects.read` | Read `context.sessions` / `context.projects`. |
| `tui.todos.read` / `tui.status.read` | Read `context.todos` / `context.status`. |
| `tui.session.control` / `tui.project.control` | Emit `session.*` / `project.*` and `fs.complete` host commands. |
| `fs.read` | Read-only `api.fs.{read, list, stat, watch}` strictly inside active Project roots (`read` <= 1 MiB, `list` <= 2,000 entries, <= 16 watches). |

**Bundle-ID confinement invariant:** Both the SDK runner and the TUI host enforce bundle-ID confinement independently. The shared host process overwrites `params.ext` with the authoritative extension ID on every host-bound RPC (`bundle/api`, `extension_state/read`, `fs/*`), so VM code cannot spoof another bundle's identity. Path traversal (`..`, `%2e%2e`, `%2f`, `\`, `?`, `#`, control characters, or empty segments) is rejected on `bundle.api` paths.

### `TuiExtensionApi`

```ts
export interface TuiExtensionApi {
  readonly bundleId: string
  readonly registerPanel: (panel: PanelOptions) => void
  readonly registerPane: (pane: PaneOptions) => void
  readonly registerCommand: (command: SlashCommandOptions) => void
  readonly registerStatusItem: (item: StatusItemOptions) => void
  readonly registerRenderer: (renderer: RendererOptions) => void
  readonly registerFormatter: (formatter: FormatterOptions) => void
  readonly registerInterceptor: (interceptor: InterceptorOptions) => void
  readonly onContext: (handler: (context: ExtensionContext) => MaybePromise<boolean | void>) => void
  readonly fs: TuiFsApi
  readonly bundleApi: TuiBundleApi
  readonly extensionState: TuiExtensionStateApi
}
```

- **`api.registerPane(pane: PaneOptions)`:** Registers a dynamic named pane with `{ id, title, defaultSlot?, defaultSize?, pinnable?, closable?, refreshMs?, render, onAction?, onKey?, onResult? }`. `defaultSlot` is one of `"left" | "right" | "top" | "bottom" | "main" | "overlay"`. `defaultSize` accepts `{ width?: number | "${1..100}%", height?: number | "${1..100}%", minWidth?: number, minHeight?: number }`. `render` receives `{ context, width, height, now, instance?, params? }`.
- **`api.registerCommand(command: SlashCommandOptions)`:** Registers a slash command `{ name, description, argumentHint?, execute, onResult? }`. `name` must match `/^[a-z0-9][a-z0-9._:-]{0,63}$/` and must not start with `skill:`.
- **`api.bundleApi`:** Provides `request({ method, path, body?, query?, sessionId?, bundleId? })` and shorthand methods `get`, `post`, `put`, `patch`, `delete`. `method` is `"GET" | "POST" | "PUT" | "PATCH" | "DELETE"`.
- **`api.extensionState`:** Provides `get<T>(bundleId?): T | undefined` (synchronous read from the latest pushed `context.extension_state`) and `read<T>({ sessionId?, bundleId? }?): Promise<T | undefined>` (asynchronous host query, optionally targeting a specific session).

### `Contributions` wire schema (`tui/activate` result)

```ts
export interface Contributions {
  readonly panels: readonly {
    readonly id: string
    readonly title: string
    readonly placement: "sidebar" | "pane"
    readonly replaces?: ReplaceableSurface
    readonly default_slot?: PaneSlot
    readonly default_size?: PaneSize
    readonly refresh_ms?: number
    readonly keys?: true
  }[]
  readonly panes: readonly {
    readonly id: string
    readonly title: string
    readonly default_slot?: PaneSlot
    readonly default_size?: PaneSize
    readonly pinnable?: boolean
    readonly closable?: boolean
    readonly refresh_ms?: number
    readonly keys?: true
  }[]
  readonly commands: readonly {
    readonly name: string
    readonly description: string
    readonly argument_hint?: string
  }[]
  readonly status_items: readonly { readonly id: string; readonly label: string; readonly priority: number }[]
  readonly renderers: readonly { readonly id: string; readonly target: "tool_call" | "composer"; readonly mode: "replace" | "decorate"; readonly priority: number }[]
  readonly formatters: readonly { readonly id: string; readonly priority: number }[]
  readonly interceptors: readonly { readonly id: string; readonly target: "submit"; readonly priority: number }[]
}
```

### Declarative `RenderNode` schema

```ts
export type RenderNode =
  | { readonly kind: "text"; readonly text: string; readonly style?: TextStyle; readonly action?: NodeAction }
  | { readonly kind: "row" | "column"; readonly children: readonly RenderNode[]; readonly gap?: number }
  | { readonly kind: "box"; readonly children: readonly RenderNode[]; readonly title?: string; readonly border?: boolean; readonly padding?: number }
  | { readonly kind: "table"; readonly columns: readonly string[]; readonly rows: readonly (readonly string[])[] }
  | { readonly kind: "progress"; readonly value: number; readonly total: number; readonly label?: string }
  | { readonly kind: "slot" }
  | {
      readonly type: "transcript" // (also accepts `kind: "transcript"`)
      readonly sessionId: string
      readonly follow?: boolean        // default true
      readonly scrollOffset?: number   // non-negative integer, used when follow is false
      readonly maxMessages?: number    // integer 1..1000
      readonly compact?: boolean
    }
```

Supported theme color tokens: `fg`, `muted`, `focus`, `heading`, `link`, `activity`, `success`, `warning`, `error`, `border` (or hex `#rrggbb`).

### `HostCommand` union

Returned in `{ commands?: HostCommand[] }` from `onAction`, `onKey`, `onResult`, or slash command `execute`:

```ts
export type HostCommand =
  | { readonly command: "bundle.api"; readonly method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE"; readonly path: string; readonly body?: JsonValue; readonly query?: Readonly<Record<string, string>>; readonly sessionId?: string; readonly bundleId?: string; readonly token?: string }
  | { readonly command: "pane.open"; readonly pane: string; readonly instance?: string; readonly title?: string; readonly slot?: PaneSlot; readonly size?: PaneSize; readonly params?: JsonValue; readonly focus?: boolean; readonly token?: string }
  | { readonly command: "pane.pin"; readonly pane: string; readonly instance?: string; readonly pinned?: boolean; readonly token?: string }
  | { readonly command: "pane.close"; readonly pane: string; readonly instance?: string; readonly token?: string }
  | { readonly command: "pane.focus"; readonly pane: string; readonly instance?: string; readonly token?: string }
  | { readonly command: "session.open"; readonly id: string; readonly token?: string }
  | { readonly command: "session.menu"; readonly id: string }
  | { readonly command: "session.new_temporary"; readonly token?: string }
  | { readonly command: "project.switch"; readonly id: string; readonly token?: string }
  | { readonly command: "project.menu"; readonly id: string }
  | { readonly command: "project.create"; readonly name: string; readonly roots: readonly string[]; readonly token?: string }
  | { readonly command: "project.rename"; readonly id: string; readonly name: string; readonly token?: string }
  | { readonly command: "project.set_roots"; readonly id: string; readonly roots: readonly string[]; readonly token?: string }
  | { readonly command: "project.delete"; readonly id: string; readonly token?: string }
  | { readonly command: "fs.complete"; readonly input: string; readonly token: string }
  | { readonly command: "ui.release" }
  | { readonly command: "ui.close" }
  | { readonly command: "ui.open" }
```

Commands carrying a `token` deliver their outcome back to the originating surface or slash command via `tui/command_result` (`{ token, ok, value?, error? }`).

### JSON-RPC methods

- **Host → Extension (`TuiMethodMap`):** `tui/initialize` (`{ api_version: 2, sdk_version, extension_id, permissions, authorized_bundles? }`), `tui/activate`, `tui/render` (`surface.kind: "panel" | "pane" | "status" | "renderer"`, optional `surface.instance`), `tui/command` (`{ name, args, raw }`), `tui/action`, `tui/key`, `tui/command_result`, `tui/context`, `tui/format`, `tui/intercept`, `tui/fs_event`, `tui/shutdown`.
- **Extension Host → TUI (`HostChannel`):** `fs/read`, `fs/list`, `fs/stat`, `fs/watch`, `fs/unwatch` (`fs.read`), `bundle/api` (`tui.bundle_api`), and `extension_state/read` (`tui.extension_state.read`). Every request is tagged with the host's authoritative `ext` bundle ID.
