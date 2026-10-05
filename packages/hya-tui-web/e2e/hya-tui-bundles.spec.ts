// The Bundles view (`/bundles`, docs/tui.md "Bundles") against a real backend:
// install a package, watch its TUI panel appear, disable and enable it,
// trust its extension (JIT tier), and uninstall it — each change reaching
// both the backend's bundle list and the TUI's extension catalog.
import { dirname, join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test, wideViewport } from "./hya"

const fixture = join(import.meta.dirname, "fixtures", "bundle-panel.hyabundle")
const panel = "BUNDLES_E2E_PANEL"

async function openBundles(term: Tui): Promise<void> {
  await term.type("/bundles")
  await term.press("Enter")
  await term.waitForText(/\d+ bundles · \d+ enabled/)
}

/** Highlight the fixture's row with the filter. */
async function selectFixture(term: Tui): Promise<void> {
  await term.press("/")
  await term.type("bundle-panel")
  await term.press("Enter")
  await term.waitForText(/▸ e2e\/bundle-panel/)
}

test.describe("Bundles view", () => {
  test("installs, disables, enables, trusts, and uninstalls a bundle with a TUI extension", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend), { viewport: wideViewport, env: { XDG_CACHE_HOME: join(dirname(backend.dir), "cache") } })
    await term.waitForText("Message, !shell, or @file")
    await openBundles(term)
    // The first-party panes' bundle, trusted by default on a local backend.
    await term.waitForText(/hya\/basic-tui-components\s+\S+\s+first-party\s+active\s+JIT running/)

    await term.press("i")
    await term.waitForText("Package (.hyabundle) path:")
    await term.type(fixture)
    await term.press("Enter")
    await term.waitForText("Scope: [user]")
    await term.press("Enter")
    await term.waitForText(`Installed ${fixture}`, 20_000)
    await term.waitForText(/e2e\/bundle-panel\s+1\.0\.0\s+user\s+active\s+VM running\s+TUI/)
    const listed = async () => (await (await fetch(`${backend.url}/v1/bundles`)).json() as { bundles?: { id: string; state?: string }[] }).bundles?.find((row) => row.id === "e2e/bundle-panel")?.state
    expect(await listed()).toBe("active")
    await term.press("Escape")
    // Its sidebar panel runs right away.
    await term.waitForText(panel, 20_000)

    await openBundles(term)
    await selectFixture(term)
    await term.press("e")
    await term.waitForText("e2e/bundle-panel disabled")
    await term.waitForText(/e2e\/bundle-panel\s+1\.0\.0\s+user\s+disabled\s+off/)
    expect(await listed()).toBe("disabled")
    // The first Esc clears the filter, the second closes the view. Wait for each
    // effect: an Esc sent right after another reads as Alt+Esc and is lost.
    await term.press("Escape")
    await expect.poll(() => term.find("Filter bundle-panel")).toBeNull()
    await term.press("Escape")
    await term.waitForText("Message, !shell, or @file")
    await expect.poll(() => term.find(panel)).toBeNull()

    await openBundles(term)
    await selectFixture(term)
    await term.press("e")
    await term.waitForText("e2e/bundle-panel enabled")
    await term.press("t")
    await term.waitForText("e2e/bundle-panel trusted")
    await term.waitForText(/e2e\/bundle-panel\s+1\.0\.0\s+user\s+active\s+JIT running/, 20_000)

    await term.press("x")
    await term.waitForText("Uninstall e2e/bundle-panel? Enter confirms")
    await term.press("Enter")
    await term.waitForText("Uninstalled e2e/bundle-panel")
    await term.waitForText("No bundle matches the filter")
    expect(await listed()).toBeUndefined()

    // A first-party bundle cannot be uninstalled. (Esc clears the filter; a key sent right after an Esc would read as Alt+key.)
    await term.press("Escape")
    await expect.poll(() => term.find("Filter bundle-panel")).toBeNull()
    await term.press("/")
    await term.type("basic-tui")
    await term.press("Enter")
    await term.press("x")
    await term.waitForText("hya/basic-tui-components ships with hya and cannot be uninstalled")
  })
})
