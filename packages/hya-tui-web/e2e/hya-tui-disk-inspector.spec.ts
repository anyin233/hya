// Package the actual standalone plugin; no copy of its implementation in fixtures.
import { execFileSync } from "node:child_process"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { expect, hyaTui, test } from "./hya"

const repo = fileURLToPath(new URL("../../..", import.meta.url))

test("disk inspector package exposes discovery and an opt-in pane", async ({ tui, backend }, testInfo) => {
  const directory = await mkdtemp(join(tmpdir(), "disk-inspector-package-"))
  try {
    const path = join(directory, "disk-inspector.hyabundle")
    execFileSync(join(repo, "target/debug/xtask"), ["package-bundle", join(repo, "plugins/disk-inspector"), path])
    const install = await fetch(`${backend.url}/v1/bundles:install`, {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ directory: backend.dir, path, project: false }),
    })
    expect(install.ok, await install.text()).toBe(true)
    const info = await fetch(`${backend.url}/v1/bundles/hya-extra%2Fdisk-inspector/api/info`)
    expect(info.ok, await info.clone().text()).toBe(true)
    expect(await info.json()).toMatchObject({ contractVersion: 1, capabilities: { scans: false, volumes: false, cancel: false } })

    const term = await tui(hyaTui(backend), { viewport: { width: 1100, height: 640 }, env: { XDG_CACHE_HOME: join(directory, "cache") } })
    await term.waitForText("Message, !shell, or @file")
    expect(await term.find("Disk inspector is not connected.")).toBeNull()
    // Give the optional pane room at both desktop and narrow viewport widths.
    for (const pane of ["projects", "sessions", "todos", "context"]) {
      await term.type(`/layout close ${pane}`)
      await term.press("Enter")
    }
    await term.type("/layout split left extension hya-extra/disk-inspector#disk")
    await term.press("Enter")
    await term.waitForText("Disk inspector is not connected.", 20_000)
    await term.attach(testInfo, "desktop-screen")
    await term.resize(690, 640)
    await term.waitForText("Disk inspector")
    await term.attach(testInfo, "narrow-screen")
    await term.resize(1100, 640)
    await term.waitForText("Disk inspector is not connected.")
    await term.type("/layout close extension")
    await term.press("Enter")
    await expect.poll(() => term.find("Disk inspector is not connected.")).toBeNull()
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})
