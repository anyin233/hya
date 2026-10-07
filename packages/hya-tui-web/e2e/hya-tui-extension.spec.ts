// Process integration of bundle TUI extensions: the backend's project bundle
// catalog lists the extension with its files, the TUI verifies and runs it in
// its isolated VM inside a confined Bun child (docs/tui-extensions.md), and its contributions reach
// the real OpenTUI surfaces: a sidebar panel, a Context field, a composer
// decoration, and a submit interceptor.
import { dirname, join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test, textStep, wideViewport } from "./hya"

/** Center of one terminal cell in page pixels (see hya-tui-projects.spec.ts). */
async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = (await term.page.locator(".xterm-screen").boundingBox())!
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

const bundle = `kind: Plugin
identity: { id: e2e/tui-panel, version: 1.0.0, publisher: e2e }
extensions:
  files:
    - { id: panel, path: tui/main.ts }
tui:
  api_version: 1
  entry: tui/main.ts
  sdk: 1.0.0
  permissions: [tui.action, tui.panel, tui.render, tui.session.read, tui.status_item, tui.keys]
`
const extension = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({
  activate(api) {
    let clicks = 0
    let keys = 0
    api.registerPanel({
      id: "hello", title: "E2E Panel",
      render: ({ context }) => ({ kind: "column", children: [
        { kind: "text", text: "E2E_EXTENSION_PANEL " + (context.session ? "with-session" : "no-session") },
        { kind: "text", text: "CLICKS " + clicks, action: { name: "click" } },
        { kind: "text", text: "KEYS " + keys },
      ] }),
      onAction: () => { clicks += 1 },
    onKey: () => { keys += 1 },
    })
    api.registerStatusItem({ id: "flag", label: "E2E", priority: 1, render: () => "E2E_STATUS" })
    api.registerRenderer({ id: "banner", target: "composer", render: () => ({ kind: "column", children: [{ kind: "text", text: "E2E_COMPOSER_BANNER" }, { kind: "slot" }] }) })
    api.registerInterceptor({ id: "guard", target: "submit", intercept: ({ text }) =>
      text.includes("forbidden") ? { decision: "block", message: "E2E_BLOCKED" } : text.startsWith("rewrite:") ? { decision: "replace", text: "REWRITTEN_PROMPT" } : undefined })
  },
})
`

test.describe("TUI extension process integration", () => {
  test.use({ projectBundles: { "tui-panel": { "bundle.yaml": bundle, "tui/main.ts": extension } }, model: { steps: [textStep("E2E_REPLY")] } })

  test("the catalog extension runs and reaches panels, the Context box, the composer, and submit", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport, env: { XDG_CACHE_HOME: join(dirname(backend.dir), "cache") } })
    await term.waitForText("Message, !shell, or @file")
    await term.waitForText("E2E_EXTENSION_PANEL with-session", 20_000)
    // The TUI ensured the Project of --dir: its project bundles are in scope, next to the first-party panes.
    const catalog = await (await fetch(`${backend.url}/v1/tui-extensions?directory=${encodeURIComponent(backend.dir)}`)).json() as { extensions?: { bundleId: string; firstParty?: boolean; files: { path: string }[] }[] }
    expect(catalog.extensions?.map((row) => [row.bundleId, row.firstParty === true, row.files.map((file) => file.path)]), JSON.stringify(catalog)).toEqual([
      ["e2e/tui-panel", false, ["tui/main.ts"]],
      ["hya/basic-tui-components", true, ["tui/context.ts", "tui/main.ts", "tui/projectView.ts", "tui/projects.ts", "tui/sessions.ts", "tui/text.ts", "tui/todos.ts"]],
    ])
    // Passive Context is scrollable and follows its configured rectangle. Give
    // its full field list room before asserting the extension's status item.
    await term.type("/layout close sessions")
    await term.press("Enter")
    await expect.poll(() => term.find("─Sessions")).toBeNull()
    // The Context row: label, then the (width-cut) value.
    await term.waitForText(/E2E\s+E2E_STAT/)
    await term.waitForText("E2E_COMPOSER_BANNER")
    // A decoration with content only above its slot must not reserve a blank
    // container below the editor. Its banner stays adjacent to the border.
    await expect.poll(async () => {
      const banner = await term.find("E2E_COMPOSER_BANNER")
      const input = await term.find("Message, !shell, or @file")
      return banner && input ? input.row - banner.row : -1
    }).toBe(2)
    await expect.poll(async () => (await term.find("Message, !shell, or @file"))?.row).toBe((await term.size()).rows - 2)

    // Submit interceptors: a blocked prompt is never sent; a rewritten one is sent as rewritten.
    await term.type("this is forbidden")
    await term.press("Enter")
    await term.type("rewrite: original")
    await term.press("Enter")
    await term.waitForText("E2E_REPLY", 20_000)
    await term.waitForText("REWRITTEN_PROMPT")
    expect(await term.find("this is forbidden")).toBeNull()
    expect(await term.find("rewrite: original")).toBeNull()

    const button = (await term.find("CLICKS 0"))!
    const point = await cellPoint(term, button.row, button.col + 2)
    await term.page.mouse.click(point.x, point.y)
    await term.waitForText("CLICKS 1")
    await term.type("/layout focus pane-8")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await term.type("abc")
    await term.waitForText("KEYS 3")
    expect(await term.find("abc")).toBeNull()

    await term.type("/extensions")
    await term.press("Enter")
    await term.waitForText("e2e/tui-panel 1.0.0 · running")
    // A third-party bundle runs in the VM; the local backend's first-party bundle is trusted (JIT tier).
    await term.waitForText(process.platform === "darwin" ? "e2e/tui-panel 1.0.0 · running · VM + OS sandbox" : /e2e\/tui-panel 1\.0\.0 · running · VM /)
    await term.waitForText(process.platform === "darwin" ? "running · JIT (trusted) + OS sandbox" : /running · JIT \(trusted\) /)

    // Automatically placed extension panels are ordinary layout leaves. A
    // named close removes one and asynchronous context updates don't reopen it.
    await term.type("/layout show")
    await term.press("Enter")
    await term.waitForText("E2E_EXTENSION_PANEL")
    await term.type("/layout close extension")
    await term.press("Enter")
    await expect.poll(() => term.find("E2E_EXTENSION_PANEL")).toBeNull()
    await term.type("/layout tree")
    await term.press("Enter")
    await term.waitForText("Layout tree")
    expect(await term.find("E2E_EXTENSION_PANEL")).toBeNull()
  })
})
