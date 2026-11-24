// `tui.toml` diagnostics and commands (ADR-0036, docs/tui-config.md): a
// syntax error or a bad item never blocks startup and is listed at the start
// of the transcript and by `/config`; `/config reload` applies an edited file
// without losing the draft; `/set` changes the structure for the session only;
// `/config init` writes a template once.

import { readFile, writeFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { expect, hyaTui, test } from "./hya"
import type { Backend } from "./hya"
import type { Tui } from "./harness"

const input = "Message, !shell, or @file"
const tomlPath = (backend: Backend) => join(dirname(backend.dir), "config", "hya", "tui.toml")

/** The row below the input box; `anchor` is the text in the input (its placeholder while empty). */
async function belowInput(term: Tui, anchor = input): Promise<string> {
  const lines = await term.lines()
  const at = lines.findLastIndex((line) => line.includes(anchor))
  return at < 0 ? "" : lines[at + 2] ?? ""
}

async function command(term: Tui, text: string): Promise<void> {
  await term.press("Control+x")
  await term.type(text)
  await term.press("Enter")
  await expect.poll(() => term.find("Commands")).toBeNull()
}

test("a syntax error ignores the file with its line, and startup uses the default preset", async ({ tui, backend }, testInfo) => {
  await writeFile(tomlPath(backend), 'preset = "minimal"\n[statusline\nenabled = false\n')
  const term = await tui(hyaTui(backend))
  await term.waitForText(input)
  await term.waitForText(/\[Config\] .*tui\.toml · 1 warning/)
  await term.waitForText(/\(line 2\)/)
  // The default preset: the status line is there.
  await expect.poll(() => belowInput(term), { timeout: 15_000 }).toMatch(/manual/)
  await term.attach(testInfo, "syntax-error")
})

test("unknown keys and wrong types are ignored item by item and listed by /config", async ({ tui, backend }, testInfo) => {
  await writeFile(tomlPath(backend), '[statusline]\ncolour = "red"\nenabled = "yes"\nsegments = ["mode", "nope"]\n[theme]\nname = "ember"\n')
  const term = await tui(hyaTui(backend))
  await term.waitForText(input)
  await term.waitForText(/\[Config\] .*4 warnings/)
  await term.waitForText("Unknown key statusline.colour; ignored")
  await term.waitForText("statusline.enabled: expected true or false; ignored")
  await term.waitForText("statusline.segments: unknown segment nope; ignored")
  await term.waitForText("[theme] is not supported in this version; ignored")
  // The valid item applies: only the mode segment.
  await expect.poll(() => belowInput(term), { timeout: 15_000 }).toMatch(/^ manual\s*$/)
  await command(term, "/config")
  await term.waitForText("TUI config")
  await term.waitForText(/statusline\.segments\s+\["mode"\]\s+tui\.toml/)
  await term.waitForText(/statusline\.enabled\s+true\s+preset statusline/)
  await term.waitForText("Warnings (4)")
  await term.attach(testInfo, "config-view")
})

test("/config reload applies an edited file and keeps the draft; /set is for this session only", async ({ tui, backend }, testInfo) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText(input)
  await expect.poll(() => belowInput(term), { timeout: 15_000 }).toMatch(/manual/)
  await term.type("kept draft")
  await writeFile(tomlPath(backend), 'preset = "minimal"\n')
  await command(term, "/config reload")
  await term.waitForText(/Preset\s+minimal \(tui\.toml\)/)
  await expect.poll(() => belowInput(term, "kept draft")).not.toMatch(/manual/)
  await term.waitForText("kept draft")

  await command(term, "/set statusline on")
  await expect.poll(() => belowInput(term, "kept draft")).toMatch(/manual/)
  await command(term, "/config")
  await term.waitForText(/statusline\.enabled\s+true\s+this session/)
  await term.attach(testInfo, "session-override")
  expect(await readFile(tomlPath(backend), "utf8")).toBe('preset = "minimal"\n')

  // An unknown component or preset changes nothing.
  await command(term, "/set bogus on")
  await command(term, "/set preset nope")
  await command(term, "/config")
  await term.waitForText(/Preset\s+minimal \(tui\.toml\)/)
  await term.waitForText("kept draft")
})

test("/config init writes the template once", async ({ tui, backend }) => {
  const term = await tui(hyaTui(backend))
  await term.waitForText(input)
  await command(term, "/config init")
  await term.waitForText(/File\s+\S*tui\.toml\s/)
  const written = await readFile(tomlPath(backend), "utf8")
  expect(written).toContain("version = 1")
  expect(written).toContain('# preset = "statusline"')
  // Still the default structure: every key in the template is commented out.
  await expect.poll(() => belowInput(term)).toMatch(/manual/)
  await command(term, "/config init")
  expect(await readFile(tomlPath(backend), "utf8")).toBe(written)
})
