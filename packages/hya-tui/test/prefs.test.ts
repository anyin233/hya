import { afterEach, expect, test } from "bun:test"
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { loadPaneLayout, loadPreferences, preferencesPath, savePreferences } from "../src/prefs"
import { defaultPaneLayout, splitPane } from "../src/state/panes"

const dirs: string[] = []
function temp(): string {
  const dir = mkdtempSync(join(tmpdir(), "hya-tui-prefs-"))
  dirs.push(dir)
  return dir
}
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }) })

test("the file lives at HYA_TUI_CONFIG, else $XDG_CONFIG_HOME/hya/tui.json, else ~/.config/hya/tui.json", () => {
  expect(preferencesPath({ HYA_TUI_CONFIG: "/tmp/x/prefs.json", XDG_CONFIG_HOME: "/xdg", HOME: "/home/u" })).toBe("/tmp/x/prefs.json")
  expect(preferencesPath({ XDG_CONFIG_HOME: "/xdg", HOME: "/home/u" })).toBe("/xdg/hya/tui.json")
  expect(preferencesPath({ XDG_CONFIG_HOME: "", HOME: "/home/u" })).toBe("/home/u/.config/hya/tui.json")
})

test("a missing file loads as no preferences, without a warning", () => {
  const path = join(temp(), "missing", "tui.json")
  expect(loadPreferences(path)).toEqual({ preferences: {} })
})

test("custom command shortcuts round-trip and normalize while invalid bindings warn without discarding other preferences", () => {
  const path = join(temp(), "tui.json")
  savePreferences(path, { keybindings: { f6: { command: "/layout focus left", scope: "workspace" } } })
  expect(loadPreferences(path).preferences.keybindings).toEqual({ F6: { command: "/layout focus left", scope: "workspace" } })
  writeFileSync(path, JSON.stringify({ theme: "light", vim: true, keybindings: { F6: { command: "invalid", scope: "conversation" } } }))
  expect(loadPreferences(path).preferences).toEqual({ theme: "light", vim: true })
  expect(loadPreferences(path).warning).toContain("single slash command")
})

test("a corrupt file loads as no preferences, with a warning naming the file", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, "{ not json")
  const loaded = loadPreferences(path)
  expect(loaded.preferences).toEqual({})
  expect(loaded.warning).toContain(path)
  writeFileSync(path, "[1, 2]")
  expect(loadPreferences(path).preferences).toEqual({})
  expect(loadPreferences(path).warning).toContain(path)
})

test("keys of the wrong type are dropped; known keys are read", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, JSON.stringify({ theme: 42 }))
  expect(loadPreferences(path).preferences).toEqual({})
  writeFileSync(path, JSON.stringify({ theme: "light" }))
  expect(loadPreferences(path)).toEqual({ preferences: { theme: "light" } })
})

test("saving merges into the file atomically, creating its directory and keeping unknown keys", () => {
  const dir = temp()
  const path = join(dir, "nested", "hya", "tui.json")
  savePreferences(path, { theme: "light" })
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ theme: "light" })
  writeFileSync(path, JSON.stringify({ theme: "light", future: { a: 1 } }))
  savePreferences(path, { theme: "hya" })
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ theme: "hya", future: { a: 1 } })
  expect(loadPreferences(path).preferences).toEqual({ theme: "hya" })
  // No temporary file is left next to it.
  expect(readdirSync(join(dir, "nested", "hya"))).toEqual(["tui.json"])
})

test("notifications is a boolean preference; another type is ignored", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, JSON.stringify({ notifications: false, theme: "light" }))
  expect(loadPreferences(path).preferences).toEqual({ notifications: false, theme: "light" })
  writeFileSync(path, JSON.stringify({ notifications: "yes" }))
  expect(loadPreferences(path).preferences).toEqual({})
  savePreferences(path, { notifications: false })
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ notifications: false })
})

test("saving over a corrupt file replaces it", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, "garbage")
  savePreferences(path, { theme: "ember" })
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ theme: "ember" })
})

test("vim is a boolean preference; another type is ignored", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, JSON.stringify({ vim: true, theme: "light" }))
  expect(loadPreferences(path).preferences).toEqual({ vim: true, theme: "light" })
  writeFileSync(path, JSON.stringify({ vim: "yes" }))
  expect(loadPreferences(path).preferences).toEqual({})
  savePreferences(path, { vim: false })
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ vim: false })
})

test("permissionMode is a nonempty string and is saved alongside other preferences", () => {
  const path = join(temp(), "tui.json")
  writeFileSync(path, JSON.stringify({ permissionMode: " ", theme: "light" }))
  expect(loadPreferences(path).preferences).toEqual({ theme: "light" })
  writeFileSync(path, JSON.stringify({ permissionMode: false }))
  expect(loadPreferences(path).preferences).toEqual({})
  savePreferences(path, { permissionMode: "yolo" })
  savePreferences(path, { theme: "hya" })
  expect(loadPreferences(path).preferences).toEqual({ theme: "hya", permissionMode: "yolo" })
})

test("a valid tiled layout survives preferences round-trip; an invalid tree is ignored", () => {
  const path = join(temp(), "tui.json")
  const paneLayout = splitPane(defaultPaneLayout(), "vertical", "jobs")
  savePreferences(path, { theme: "light", paneLayout })
  expect(loadPreferences(path).preferences).toEqual({ theme: "light", paneLayout })
  writeFileSync(path, JSON.stringify({ theme: "light", paneLayout: { ...paneLayout, active: "missing" } }))
  expect(loadPreferences(path).preferences).toEqual({ theme: "light" })
})

test("a saved version-1 center split loads with editable outer side panes", () => {
  const path = join(temp(), "tui.json")
  const legacy = { version: 1, active: "pane-2", root: { type: "split", axis: "vertical", weight: 0.5,
    first: { type: "pane", id: "pane-1", kind: "conversation" }, second: { type: "pane", id: "pane-2", kind: "jobs" } } }
  writeFileSync(path, JSON.stringify({ paneLayout: legacy }))
  const loaded = loadPreferences(path).preferences.paneLayout!
  expect(loaded.version).toBe(4)
  expect(loaded.active).toBe("pane-2")
  expect(JSON.stringify(loaded.root)).toContain('"kind":"projects"')
  expect(JSON.stringify(loaded.root)).toContain('"kind":"sessions"')
})


test("disabled default bindings persist as explicit null overrides", () => {
  const path = join(temp(), "tui.json")
  savePreferences(path, { keybindings: { "Ctrl+C": null, "Ctrl+W": { command: "/layout close", scope: "workspace" } } })
  expect(loadPreferences(path).preferences.keybindings).toEqual({ "Ctrl+C": null, "Ctrl+W": { command: "/layout close", scope: "workspace" } })
})


test("explicit layout loading refuses missing/corrupt/invalid layouts and never rewrites preferences", () => {
  const path = join(temp(), "tui.json")
  expect(() => loadPaneLayout(path)).toThrow("not found")
  for (const [text, error] of [["bad json", "JSON object"], ["[]", "JSON object"], ["{}", "No paneLayout"], ['{"paneLayout":null}', "Invalid paneLayout"]]) {
    writeFileSync(path, text!)
    expect(() => loadPaneLayout(path)).toThrow(error!)
    expect(readFileSync(path, "utf8")).toBe(text!)
  }
  const layout = splitPane(defaultPaneLayout(), "vertical", "jobs")
  const text = JSON.stringify({ paneLayout: layout, theme: "light", unknown: 42 })
  writeFileSync(path, text)
  expect(loadPaneLayout(path)).toEqual(layout)
  expect(readFileSync(path, "utf8")).toBe(text)
  const legacy = { version: 2, active: "pane-1", root: { type: "pane", id: "pane-1", kind: "conversation" } }
  writeFileSync(path, JSON.stringify({ paneLayout: legacy }))
  expect(loadPaneLayout(path).version).toBe(4)
})
