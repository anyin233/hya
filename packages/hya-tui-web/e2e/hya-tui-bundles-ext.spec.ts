// Real-process integration for bundle TUI extensions and `.hyabundle` packages:
// 1. Project-scoped TUI extension running in the confined Bun VM sidecar
//    reaches sidebar panels, Context fields, composer decorations, submit
//    interceptors, click/key events, and `/extensions`.
// 2. `.hyabundle` install/disable/enable/trust/uninstall lifecycle through
//    `/bundles` against a real `hya serve`.
// 3. `xtask package-bundle plugins/disk-inspector` real package install,
//    bundle HTTP route `/api/info`, and opt-in extension pane split/resize/close.

import { execFileSync } from "node:child_process"
import { existsSync } from "node:fs"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { Tui } from "./harness"
import { expect, hyaBin, hyaTui, test, textStep, wideViewport } from "./hya"

test.use({ preset: "workspace" })

const repo = fileURLToPath(new URL("../../..", import.meta.url))
const bundleFixture = join(import.meta.dirname, "fixtures", "bundle-panel.hyabundle")

/** Resolve `xtask` next to `hyaBin` (supports `CARGO_TARGET_DIR` / `HYA_BIN`), falling back to `target/debug/xtask`. */
function locateXtask(): string {
  const beside = join(dirname(hyaBin), process.platform === "win32" ? "xtask.exe" : "xtask")
  if (existsSync(beside)) return beside
  return join(repo, "target/debug/xtask")
}

async function cellPoint(term: Tui, row: number, col: number): Promise<{ x: number; y: number }> {
  const box = await term.screenBox()
  const { cols, rows } = await term.size()
  return { x: box.x + ((col + 0.5) / cols) * box.width, y: box.y + ((row + 0.5) / rows) * box.height }
}

const projectBundleYaml = `kind: Bundle
schema_version: 2
identity: { id: e2e/tui-panel, version: 1.0.0, publisher: e2e }
assets:
  - { id: panel, path: tui/main.ts }
tui:
  api_version: 2
  entry: tui/main.ts
  sdk: "2.0.0"
  permissions: [tui.action, tui.panel, tui.render, tui.session.read, tui.status_item, tui.keys]
`

const projectBundleMain = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({
  activate(api) {
    let clicks = 0
    let keys = 0
    api.registerPanel({
      id: "hello", title: "E2E Panel",
      render: ({ context }) => ({ kind: "column", children: [
        { kind: "box", title: "E2E_PASSIVE_NOTICE", border: true, children: [{ kind: "text", text: "Plain plugin facts" }] },
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

test.describe("project bundle TUI extension sidecar", () => {
  test.use({
    projectBundles: { "tui-panel": { "bundle.yaml": projectBundleYaml, "tui/main.ts": projectBundleMain } },
    model: { steps: [textStep("E2E_REPLY")] },
  })

  test("runs in the isolated VM and reaches panels, Context, composer, submit interceptors, and /extensions", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport, env: { XDG_CACHE_HOME: join(dirname(backend.dir), "cache") } })
    await term.waitForText("Message, !shell, or @file")
    await term.waitForText("E2E_EXTENSION_PANEL with-session", 20_000)
    await term.waitForText("E2E_COMPOSER_BANNER")

    await term.type("/layout close sessions")
    await term.press("Enter")
    await expect.poll(() => term.find("─Sessions")).toBeNull()
    await term.waitForText(/E2E\s+E2E_STAT/)

    // Submit interceptors: blocked never sends; rewritten sends replacement.
    await term.type("this is forbidden")
    await term.press("Enter")
    await term.type("rewrite: original")
    await term.press("Enter")
    await term.waitForText("E2E_REPLY", 20_000)
    await term.waitForText("REWRITTEN_PROMPT")
    expect(await term.find("this is forbidden")).toBeNull()

    // Pointer action + focused key delivery.
    const button = (await term.find("CLICKS 0"))!
    const point = await cellPoint(term, button.row, button.col + 2)
    await term.mouse.click(point.x, point.y)
    await term.waitForText("CLICKS 1")
    await term.type("/layout focus pane-8")
    await term.press("Enter")
    await expect.poll(() => term.find("Commands")).toBeNull()
    await term.type("abc")
    await term.waitForText("KEYS 3")

    // `/extensions` reports third-party VM tier and first-party JIT tier.
    await term.type("/extensions")
    await term.press("Enter")
    await term.waitForText(/e2e\/tui-panel 1\.0\.0 · running · VM /)
    await term.waitForText(/running · JIT \(trusted\) /)
  })
})

test.describe(".hyabundle package install and disk-inspector", () => {
  async function openBundles(term: Tui): Promise<void> {
    await term.type("/bundles")
    await term.press("Enter")
    await term.waitForText(/\d+ bundles · \d+ enabled/)
  }

  async function selectFixture(term: Tui): Promise<void> {
    await term.press("/")
    await term.type("bundle-panel")
    await term.press("Enter")
    await term.waitForText(/▸ e2e\/bundle-panel/)
  }

  test("installs, disables, enables, trusts (VM -> JIT), and uninstalls a .hyabundle with a TUI panel", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport, env: { XDG_CACHE_HOME: join(dirname(backend.dir), "cache") } })
    await term.waitForText("Message, !shell, or @file")
    await openBundles(term)
    await term.waitForText(/hya\/basic-tui-components\s+\S+\s+first-party\s+active\s+JIT running/)

    await term.press("i")
    await term.waitForText("Package (.hyabundle) path:")
    await term.type(bundleFixture)
    await term.press("Enter")
    await term.waitForText("Scope: [user]")
    await term.press("Enter")
    await term.waitForText(/e2e\/bundle-panel\s+1\.0\.0\s+user\s+active\s+VM running\s+TUI/, 20_000)
    await term.press("Escape")
    await term.waitForText("BUNDLES_E2E_PANEL", 20_000)

    await openBundles(term)
    await selectFixture(term)
    await term.press("e")
    await term.waitForText("e2e/bundle-panel disabled")
    await term.press("e")
    await term.waitForText("e2e/bundle-panel enabled")
    await term.press("t")
    await term.waitForText("e2e/bundle-panel trusted")
    await term.waitForText(/e2e\/bundle-panel\s+1\.0\.0\s+user\s+active\s+JIT running/, 20_000)
    await term.press("x")
    await term.waitForText("Uninstall e2e/bundle-panel? Enter confirms")
    await term.press("Enter")
    await term.waitForText("Uninstalled e2e/bundle-panel")
  })

  test("packages and installs standalone plugins/disk-inspector and opens its opt-in pane", async ({ tui, backend }) => {
    const directory = await mkdtemp(join(tmpdir(), "disk-inspector-package-"))
    try {
      const archive = join(directory, "disk-inspector.hyabundle")
      execFileSync(locateXtask(), ["package-bundle", join(repo, "plugins/disk-inspector"), archive])
      const install = await fetch(`${backend.url}/v2/bundles:install`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ directory: backend.dir, path: archive, project: false }),
      })
      const installed = await install.text()
      expect(install.ok, installed).toBe(true)
      const change = JSON.parse(installed)
      expect(change.refresh?.errors ?? [], installed).toEqual([])
      expect(change.refresh?.bundles?.map((bundle: { id: string }) => bundle.id), installed).toContain("hya-extra/disk-inspector")

      const info = await fetch(`${backend.url}/v2/bundles/hya-extra%2Fdisk-inspector/api/info`)
      expect(info.ok).toBe(true)
      expect(await info.json()).toMatchObject({ contractVersion: 1, capabilities: { scans: false, volumes: false, cancel: false } })

      const term = await tui(hyaTui(backend), { viewport: { width: 1100, height: 640 }, env: { XDG_CACHE_HOME: join(directory, "cache") } })
      await term.waitForText("Message, !shell, or @file")
      expect(await term.find("Disk inspector is not connected.")).toBeNull()
      for (const pane of ["projects", "sessions", "todos", "context"]) {
        await term.type(`/layout close ${pane}`)
        await term.press("Enter")
      }
      await term.type("/layout split left extension hya-extra/disk-inspector#disk")
      await term.press("Enter")
      await term.waitForText("Disk inspector is not connected.", 20_000)
      await term.resize(690, 640)
      await term.waitForText("Disk inspector")
      await term.resize(1100, 640)
      await term.type("/layout close extension")
      await term.press("Enter")
      await expect.poll(() => term.find("Disk inspector is not connected.")).toBeNull()
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })
})
