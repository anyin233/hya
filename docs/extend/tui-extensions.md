# TUI Extensions (`@hya/tui-sdk`)

A Bundle can customize Hya's terminal UI and browser WebUI with TypeScript—adding new sidebar/workspace panels, custom status rows, custom tool-card renderers, output formatters, or prompt interceptors.

Every TUI extension runs inside an isolated **QuickJS WebAssembly VM** (capped at 64 MiB of memory and 2 seconds per call, plus OS-level Landlock/seccomp on Linux or `sandbox_init` on macOS) with zero access to the network, environment variables, or shell. Even if an extension crashes or loops forever, your TUI never freezes.

---

## 1. Quick example

Add a `tui:` section to your `bundle.yaml` and list your TypeScript entry file under `extensions.files`:

```yaml
kind: Bundle
identity:
  id: acme/git-status-ui
  version: 1.0.0
  publisher: acme
extensions:
  files:
    - { id: tui-entry, path: tui/main.ts }
tui:
  api_version: 1
  entry: tui/main.ts
  sdk: 1.0.0
  permissions:
    - tui.panel
    - tui.status_item
    - workspace.git.read
```

Write `tui/main.ts` importing `defineTuiExtension` from `@hya/tui-sdk` (the SDK is bundled automatically by Hya—no `npm install` needed at runtime):

```ts
import { defineTuiExtension } from "@hya/tui-sdk"

export default defineTuiExtension({
  activate(api) {
    api.registerPanel({
      id: "git-summary",
      title: "Git Status",
      render: ({ context }) => ({
        kind: "column",
        children: [
          {
            kind: "text",
            text: `⎇ ${context.git?.branch ?? "detached"}`,
            style: { color: "heading", bold: true },
          },
          {
            kind: "text",
            text: `${context.git?.dirty ?? 0} modified · ↑${context.git?.ahead ?? 0} ↓${context.git?.behind ?? 0}`,
          },
        ],
      }),
    })

    api.registerStatusItem({
      id: "dirty-files",
      label: "Dirty",
      priority: 6,
      render: ({ context }) => (context.git?.dirty ? `${context.git.dirty} files` : undefined),
    })
  },
})
```

Package and install the bundle (`hya bundle install ...`), or place the folder under `<project>/.hya/bundles/git-status-ui/` and run **`/reload-bundles`**.

---

## 2. What a TUI extension can register

| API call | Required permission | What it adds |
| --- | --- | --- |
| `api.registerPanel({ id, title, render, placement?, replaces?, onAction?, onKey? })` | `tui.panel` (+ `tui.keys` for `onKey`) | Custom sidebar or workspace pane (can even replace built-in panes like `projects`, `sessions`, `todos`, or `context`). |
| `api.registerStatusItem({ id, label, priority?, render })` | `tui.status_item` | A row in the Context box and a segment on the bottom status line. |
| `api.registerRenderer({ id, target: "tool_call" \| "composer", mode, render })` | `tui.render` | Replace or wrap (`{ kind: "slot" }`) tool call cards or decorate above/below the composer. |
| `api.registerFormatter({ id, priority?, format })` | `tui.render` | Transform tool output text displayed inside tool cards. |
| `api.registerInterceptor({ id, target: "submit", intercept })` | `tui.action` | Inspect prompts before submission (`continue`, `replace` text, or `block` with a reason—e.g. catching pasted private keys!). |
| `api.fs.{read, list, stat, watch}` | `fs.read` | Read-only access and live file watching strictly inside the active Project's roots. |

---

## 3. Declarative `RenderNode` UI trees

Extensions return plain JSON render trees that Hya draws using its own theme-aware terminal widgets:

```ts
{ kind: "text", text: "Tests passed", style: { color: "success", bold: true }, action: { name: "open", data: 1 } }
{ kind: "row", gap: 2, children: [...] }
{ kind: "column", children: [...] }
{ kind: "box", title: "Section Heading", padding: 1, children: [...] }
{ kind: "table", columns: ["File", "Changes"], rows: [["src/lib.rs", "+12 -3"]] }
{ kind: "progress", value: 4, total: 5, label: "Checks" }
{ kind: "slot" } // Used inside `decorate` renderers to place the wrapped host widget
```

Supported theme color tokens: `fg`, `muted`, `focus`, `heading`, `link`, `activity`, `success`, `warning`, `error`, `border` (or hex `#rrggbb`).

---

## 4. Permissions & context sections

An extension sees **only** the `context` fields whose permissions are listed in `tui.permissions`:

| Permission | Grants access to |
| --- | --- |
| `tui.session.read` | `context.session` (ID, title, agent, model, mode, busy flag) |
| `tui.transcript.read` | `context.transcript` (last 50 messages) + tool call inputs/outputs in card renderers |
| `tui.workspace.read` | `context.workspace` (directory and active Project ID/name) |
| `workspace.git.read` | `context.git` (branch, HEAD, dirty count, ahead/behind) |
| `tui.sessions.read` / `tui.projects.read` | `context.sessions` / `context.projects` lists |
| `tui.todos.read` / `tui.status.read` | `context.todos` / `context.status` |
| `tui.session.control` / `tui.project.control` | Returning host commands (`session.open`, `project.switch`, `fs.complete`, etc.) |
| `fs.read` | Read-only `api.fs` (`read` ≤ 1 MiB, `list` ≤ 2,000 entries, `stat`, up to 16 `watch`es inside Project roots) |

---

## 5. Managing extensions & trust tiers (`/extensions`)

Inside Hya, run **`/extensions`** (or open **`/bundles`**):

- **Default VM tier:** Untrusted extensions run in QuickJS WebAssembly (64 MiB hard memory cap, no Node/Bun/OS APIs). Extensions from a **remote** backend start `blocked` until you run `/extensions enable <bundle-id>`.
- **Trusted JIT tier (`/extensions trust <bundle-id>`):** Runs trusted code on JavaScriptCore's fast JIT (6–10x faster rendering for heavy panels, used by Hya's own `hya/basic-tui-components`), still inside an empty-global realm and OS sandbox, without the 64 MiB WASM memory cap.
- **Placing panels manually:** In the `workspace` layout preset, open any extension panel in a split with:
  ```text
  /layout split left extension acme/git-status-ui#git-summary
  ```
- **Running without TUI bundles (`hya --bare`):** When the backend advertises zero TUI extensions, `ExtensionManager.settled()` resolves immediately, no extension host process is kept running, and the TUI falls back to the single-column chat layout (`conversation` + `activity` + `composer` + status line) without `"Loading…"` or missing-pane placeholders. Core overlays (`/key`, `/mcp`, `/skills`, `/bundles`, `/rules`, `/diff`) remain available without any TUI bundle installed.
