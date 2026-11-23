import { expect, test } from "bun:test"
import { availablePresets, builtinPreset, builtinPresetNames, defaultPreset, defaultSegments, isBuiltinPreset, presetDescriptions } from "../src/config/presets"
import { chatPaneLayout, paneLeaves, parsePaneLayout } from "../src/state/panes"

test("statusline is the default; dock is reserved until the dock components exist", () => {
  expect(defaultPreset).toBe("statusline")
  expect(builtinPresetNames).toEqual(["minimal", "statusline", "dock", "workspace"])
  expect(availablePresets).toEqual(["minimal", "statusline", "workspace"])
  expect(isBuiltinPreset("workspace")).toBe(true)
  expect(isBuiltinPreset("focus")).toBe(false)
  for (const name of builtinPresetNames) expect(presetDescriptions[name].length).toBeGreaterThan(0)
})

test("the built-in presets differ only in the components ADR-0036 lists", () => {
  expect(builtinPreset("statusline")).toEqual({
    startup: { enabled: true, resources: "compact", sections: ["context", "skills", "commands", "mcp", "bundles", "extensions"], quiet: false },
    statusline: { enabled: true, segments: [...defaultSegments], placement: "below" },
    composer: { style: "box", border_info: [], paste: { collapse_lines: 10, collapse_chars: 1000, wrap: true } },
    sidebar: { enabled: false },
  })
  expect(defaultSegments).toEqual(["model", "mode", "cwd", "git", "ctx", "tokens"])
  expect(builtinPreset("minimal").statusline.enabled).toBe(false)
  expect(builtinPreset("minimal").composer.border_info).toEqual(["model", "mode"])
  expect(builtinPreset("workspace").sidebar.enabled).toBe(true)
  expect(builtinPreset("workspace").statusline.enabled).toBe(true)
  // Every call is a fresh copy.
  const one = builtinPreset("statusline")
  one.statusline.segments.push("session")
  expect(builtinPreset("statusline").statusline.segments).toEqual([...defaultSegments])
})

test("the chat column is a valid layout with the default tree's ids", () => {
  const layout = chatPaneLayout()
  expect(parsePaneLayout(layout)).toEqual(layout)
  expect(paneLeaves(layout.root).map((pane) => [pane.id, pane.kind])).toEqual([["pane-6", "conversation"], ["pane-7", "activity"], ["pane-1", "composer"]])
  expect(layout.active).toBe("pane-1")
})
