import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  initTuiConfig, loadTuiConfig, parseTuiConfig, presetNames, resolveTuiConfig, tomlErrorLine, tuiConfigPath, tuiConfigTemplate,
} from "../src/config/tuiConfig"
import { builtinPreset } from "../src/config/presets"

const parse = (source: string) => parseTuiConfig(source, "tui.toml").raw

test("tuiConfigPath: HYA_TUI_TOML, then next to HYA_TUI_CONFIG, then XDG, then ~/.config", () => {
  expect(tuiConfigPath({ HYA_TUI_TOML: "/x/a.toml", HYA_TUI_CONFIG: "/y/tui.json" })).toBe("/x/a.toml")
  expect(tuiConfigPath({ HYA_TUI_CONFIG: "/y/tui.json", XDG_CONFIG_HOME: "/xdg" })).toBe("/y/tui.toml")
  expect(tuiConfigPath({ XDG_CONFIG_HOME: "/xdg", HOME: "/home/u" })).toBe("/xdg/hya/tui.toml")
  expect(tuiConfigPath({ HOME: "/home/u" })).toBe("/home/u/.config/hya/tui.toml")
})

test("no file means the default preset and no warning", () => {
  const dir = mkdtempSync(join(tmpdir(), "tui-toml-"))
  try {
    const loaded = loadTuiConfig(join(dir, "tui.toml"))
    expect(loaded).toEqual({ path: join(dir, "tui.toml"), exists: false, warnings: [] })
    const resolved = resolveTuiConfig(loaded.raw)
    expect(resolved.preset).toBe("statusline")
    expect(resolved.presetSource).toBe("default")
    expect(resolved.structure).toEqual(builtinPreset("statusline"))
    expect(resolved.warnings).toEqual([])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test("a syntax error ignores the whole file and names the line", () => {
  const result = parseTuiConfig('preset = "minimal"\n[statusline\nenabled = false\n', "/c/tui.toml")
  expect(result.raw).toBeUndefined()
  expect(result.warnings).toHaveLength(1)
  expect(result.warnings[0]).toContain("/c/tui.toml (line 2)")
  expect(result.warnings[0]).toContain("using the statusline preset")
  expect(tomlErrorLine("a = 1\n\n\nb = x\n", "Strings must be quoted: \"x\"")).toBe(4)
})

test("top-level tables override the preset key by key; arrays replace", () => {
  const resolved = resolveTuiConfig(parse('preset = "minimal"\n[statusline]\nenabled = true\nsegments = ["model", "ctx"]\n'))
  expect(resolved.preset).toBe("minimal")
  expect(resolved.presetSource).toBe("tui.toml")
  expect(resolved.structure.statusline).toEqual({ enabled: true, segments: ["model", "ctx"], placement: "below" })
  // Untouched keys keep the preset's values.
  expect(resolved.structure.composer.border_info).toEqual(["model", "mode"])
  expect(resolved.sources["statusline.enabled"]).toBe("tui.toml")
  expect(resolved.sources["statusline.placement"]).toBe("preset minimal")
  expect(resolved.warnings).toEqual([])
})

test("named presets extend a built-in one; the chain applies outermost first", () => {
  const raw = parse([
    'preset = "focus2"',
    "[presets.focus]",
    'extends = "minimal"',
    'statusline = { enabled = true, segments = ["model", "mode", "ctx"] }',
    "[presets.focus2]",
    'extends = "focus"',
    'statusline = { segments = ["mode"] }',
  ].join("\n"))
  const resolved = resolveTuiConfig(raw)
  expect(resolved.chain).toEqual(["focus2", "focus", "minimal"])
  expect(resolved.structure.statusline.enabled).toBe(true)
  expect(resolved.structure.statusline.segments).toEqual(["mode"])
  expect(resolved.sources["statusline.enabled"]).toBe("preset focus")
  expect(resolved.sources["statusline.segments"]).toBe("preset focus2")
  expect(presetNames(raw)).toEqual(["minimal", "statusline", "workspace", "focus", "focus2"])
  // Without extends a named preset starts from the default.
  expect(resolveTuiConfig(parse('preset = "a"\n[presets.a]\nsidebar = { enabled = true }\n')).chain).toEqual(["a", "statusline"])
})

test("unknown presets, cycles, and chains deeper than 8 fall back to the default with a warning", () => {
  const unknown = resolveTuiConfig(parse('preset = "nope"\n'))
  expect(unknown.preset).toBe("statusline")
  expect(unknown.warnings).toEqual(["Unknown preset nope; using statusline"])
  const cycle = resolveTuiConfig(parse('preset = "a"\n[presets.a]\nextends = "b"\n[presets.b]\nextends = "a"\n'))
  expect(cycle.preset).toBe("statusline")
  expect(cycle.warnings[0]).toContain("extends cycle (a → b → a)")
  const deep = Array.from({ length: 10 }, (_, i) => `[presets.p${i}]\nextends = "${i === 9 ? "minimal" : `p${i + 1}`}"`).join("\n")
  const tooDeep = resolveTuiConfig(parse(`preset = "p0"\n${deep}\n`))
  expect(tooDeep.preset).toBe("statusline")
  expect(tooDeep.warnings[0]).toContain("more than 8 presets")
  // dock is reserved for the dock components.
  const dock = resolveTuiConfig(parse('preset = "dock"\n'))
  expect(dock.preset).toBe("statusline")
  expect(dock.warnings[0]).toContain("preset dock is not supported in this version")
  // A user preset cannot shadow a built-in one.
  const shadow = resolveTuiConfig(parse('[presets.minimal]\nsidebar = { enabled = true }\n'))
  expect(shadow.warnings[0]).toContain("presets.minimal: minimal is a built-in preset")
})

test("bad items are ignored one by one; later-version keys say so; the rest applies", () => {
  const resolved = resolveTuiConfig(parse([
    "version = 2",
    "colour = 1",
    "[statusline]",
    'enabled = "yes"',
    'placement = "band"',
    'segments = ["model", "nope", "model"]',
    "[composer]",
    'style = "fancy"',
    "[composer.paste]",
    "collapse_lines = 5",
    "collapse_chars = 0",
    "image_resize = false",
    "[composer.attachments]",
    'style = "cards"',
    "[startup]",
    "quiet = true",
    "[keys.workspace]",
    '"alt+s" = "/sessions"',
  ].join("\n")))
  expect(resolved.structure.statusline).toEqual({ enabled: true, segments: ["model"], placement: "below" })
  expect(resolved.structure.composer.style).toBe("box")
  expect(resolved.warnings).toEqual([
    "version 2 is newer than this TUI (1); reading the keys it knows",
    "Unknown key colour; ignored",
    "[startup] is not supported in this version; ignored",
    "[keys] is not supported in this version; ignored",
    "statusline.enabled: expected true or false; ignored",
    'statusline.placement = "band" is not supported in this version; ignored',
    "statusline.segments: unknown segment nope; ignored",
    'composer.style: expected "box" or "band"; ignored',
    "composer.paste.collapse_chars: expected an integer ≥ 1; ignored",
    "composer.paste.image_resize is not supported in this version; ignored",
    "composer.attachments is not supported in this version; ignored",
  ])
  expect(resolved.structure.composer.paste).toEqual({ collapse_lines: 5, collapse_chars: 1000, wrap: true })
  expect(resolved.sources["composer.paste.collapse_lines"]).toBe("tui.toml")
  expect(resolved.sources["composer.paste.wrap"]).toBe("preset statusline")
})

test("the session layer wins over tui.toml; a session preset starts its own values", () => {
  const raw = parse('preset = "minimal"\n[sidebar]\nenabled = false\n')
  const on = resolveTuiConfig(raw, { enabled: { sidebar: true } })
  expect(on.structure.sidebar.enabled).toBe(true)
  expect(on.sources["sidebar.enabled"]).toBe("this session")
  const workspace = resolveTuiConfig(raw, { preset: "workspace" })
  expect(workspace.preset).toBe("workspace")
  expect(workspace.presetSource).toBe("this session")
  // The top-level table still overrides the preset in effect.
  expect(workspace.structure.sidebar.enabled).toBe(false)
  expect(workspace.structure.statusline.enabled).toBe(true)
})

test("the template parses with no warnings and resolves to the default; init never overwrites", () => {
  const resolved = resolveTuiConfig(parse(tuiConfigTemplate()))
  expect(resolved.warnings).toEqual([])
  expect(resolved.structure).toEqual(builtinPreset("statusline"))
  const dir = mkdtempSync(join(tmpdir(), "tui-toml-"))
  try {
    const path = join(dir, "nested", "tui.toml")
    initTuiConfig(path)
    expect(readFileSync(path, "utf8")).toBe(tuiConfigTemplate())
    writeFileSync(path, 'preset = "minimal"\n')
    expect(() => initTuiConfig(path)).toThrow("already exists")
    expect(readFileSync(path, "utf8")).toBe('preset = "minimal"\n')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
