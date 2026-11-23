import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createStructureController, statuslineDefaultNotice } from "../src/app/structure"
import { chatPaneLayout, defaultPaneLayout, paneLeaves } from "../src/state/panes"
import { createAppStore } from "../src/state/store"

function fixture(files: { toml?: string; json?: unknown } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "tui-structure-"))
  const path = join(dir, "tui.toml"), preferencesPath = join(dir, "tui.json")
  if (files.toml !== undefined) writeFileSync(path, files.toml)
  if (files.json !== undefined) writeFileSync(preferencesPath, JSON.stringify(files.json))
  const store = createAppStore()
  return { dir, path, preferencesPath, store, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

const kinds = (store: ReturnType<typeof createAppStore>) => paneLeaves(store.state.paneLayout.root).map((pane) => pane.kind)

test("the default structure uses the chat column; workspace uses the saved tree", () => {
  const saved = { ...defaultPaneLayout(), active: "pane-3" }
  const { store, path, preferencesPath, cleanup } = fixture({ json: { paneLayout: saved } })
  try {
    const structure = createStructureController({ store, path, preferencesPath })
    expect(store.state.structure.sidebar.enabled).toBe(false)
    expect(store.state.paneLayout).toEqual(chatPaneLayout())
    structure.setPreset("workspace")
    expect(kinds(store)).toContain("sessions")
    expect(store.state.paneLayout.active).toBe("pane-3")
    structure.setEnabled("sidebar", false)
    expect(store.state.paneLayout).toEqual(chatPaneLayout())
    expect(() => structure.setPreset("nope")).toThrow("Unknown preset nope")
  } finally {
    cleanup()
  }
})

test("a saved tiled layout without tui.toml shows the default-change notice once and records it", () => {
  const { store, path, preferencesPath, cleanup } = fixture({ json: { paneLayout: defaultPaneLayout(), theme: "ember" } })
  try {
    createStructureController({ store, path, preferencesPath })
    expect(store.state.startupNotices[0]).toContain("The default layout is now statusline")
    const saved = JSON.parse(readFileSync(preferencesPath, "utf8"))
    expect(saved.seenNotices).toEqual([statuslineDefaultNotice])
    expect(saved.theme).toBe("ember")
    const again = createAppStore()
    createStructureController({ store: again, path, preferencesPath })
    expect(again.state.startupNotices).toEqual([])
  } finally {
    cleanup()
  }
})

test("tui.toml warnings become start-of-transcript notices; reload applies an edited file", () => {
  const { store, path, preferencesPath, cleanup } = fixture({ toml: '[statusline]\ncolour = 1\n' })
  try {
    const structure = createStructureController({ store, path, preferencesPath })
    expect(store.state.startupNotices).toEqual([expect.stringMatching(/^\[Config\] .*tui\.toml · 1 warning · \/config lists them$/), "  Unknown key statusline.colour; ignored"])
    writeFileSync(path, 'preset = "workspace"\n')
    expect(structure.reload().preset).toBe("workspace")
    expect(store.state.startupNotices).toEqual([])
    expect(kinds(store)).toContain("projects")
    expect(structure.describe()).toMatch(/Preset {4}workspace \(tui\.toml\)/)
    expect(structure.describe()).toMatch(/sidebar\.enabled\s+true\s+preset workspace/)
  } finally {
    cleanup()
  }
})

test("init writes the template once and reloads it", () => {
  const { store, path, cleanup } = fixture()
  try {
    const structure = createStructureController({ store, path })
    structure.init()
    expect(readFileSync(path, "utf8")).toContain("version = 1")
    expect(() => structure.init()).toThrow("already exists")
    expect(structure.resolved().warnings).toEqual([])
  } finally {
    cleanup()
  }
})
