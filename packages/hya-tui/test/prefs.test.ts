import { afterEach, expect, test } from "bun:test"
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { loadPreferences, preferencesPath, savePreferences } from "../src/prefs"

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
