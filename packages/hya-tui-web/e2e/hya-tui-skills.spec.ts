// The Skills view (`/skills`; docs/tui.md "Skills view"): every skill of the
// scope under a divider per source group, Enter opens a skill's details
// (file, description, scrolling body), `u` closes the view and puts
// `/skill:<name> ` into the message input, `/` filters (state/skills.ts,
// `GET /v1/skills`).

import { mkdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { expect, hyaTui, test } from "./hya"

const placeholder = "Message, !shell, or @file · / commands"

async function writeSkill(dir: string, name: string, description: string, body: string): Promise<void> {
  const skillDir = join(dir, ".hya/skills", name)
  await mkdir(skillDir, { recursive: true })
  await writeFile(join(skillDir, "SKILL.md"), `---\nname: ${name}\ndescription: ${description}\n---\n${body}\n`)
}

async function command(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

/** The composer's text, from the box above the status line (trailing spaces trimmed). */
async function composerText(term: Tui): Promise<string> {
  const lines = await term.lines()
  const bottom = lines.findLastIndex((line) => line.startsWith("└"))
  let top = bottom - 1
  while (top >= 0 && !lines[top]!.startsWith("┌")) top--
  const text = lines.slice(top + 1, bottom).map((line) => line.slice(1, line.indexOf("│", 1) < 0 ? undefined : line.indexOf("│", 1)).trim()).join("\n")
  return text === placeholder ? "" : text
}

test.describe("hya TUI Skills view", () => {
  test("/skills lists the scope's skills by source; Enter shows a skill's body; u puts /skill:<name> into the input", async ({ tui, backend }, testInfo) => {
    await writeSkill(backend.dir, "greet", "Say hello to someone", "# Greeting\n\nSay hello warmly.\n\nSKILL_BODY_MARKER")
    await writeSkill(backend.dir, "reviewer", "Review the diff", "Read the diff and report.")
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await command(term, "/skills")
    await term.waitForText(/\d+ skills · /)
    await term.waitForText(/SKILL\s+SOURCE\s+DESCRIPTION/)
    await term.waitForText(/── project .*\.hya\/skills/)
    await term.waitForText(/▸ greet\s+project\s+Say hello to someone/)
    await term.waitForText(/ {2}reviewer\s+project\s+Review the diff/)
    await term.waitForText("── builtin")
    await term.waitForText(/agent-bundle-authoring\s+builtin/)
    await term.waitForText("↑↓ move · Enter details · u use · y copy path · r refresh · / filter · Esc close")
    // Groups follow the catalog's precedence: project skills above the built-ins.
    expect((await term.find("── project"))!.row).toBeLessThan((await term.find("── builtin"))!.row)
    await term.attach(testInfo, "skills-list")

    await term.press("Enter")
    await term.waitForText("Skills › greet")
    await term.waitForText("greet · project")
    await term.waitForText(/\.hya\/skills\/greet\/SKILL\.md/)
    await term.waitForText("# Greeting")
    await term.waitForText("SKILL_BODY_MARKER")
    await term.waitForText("↑↓ PgUp PgDn scroll · u use · y copy path · Esc back")
    await term.attach(testInfo, "skills-detail")
    await term.press("Escape")
    await term.waitForText(/▸ greet\s+project/)

    await term.press("ArrowDown")
    await term.waitForText(/▸ reviewer\s+project/)
    await term.press("u")
    await expect.poll(() => composerText(term)).toBe("/skill:reviewer")
    expect(await term.text()).not.toContain("── builtin")
  })

  test("/ filters by name or description; Esc clears the filter, then closes the view", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "greet", "Say hello to someone", "Say hello.")
    await writeSkill(backend.dir, "reviewer", "Review the diff", "Read the diff.")
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await command(term, "/skills")
    await term.waitForText(/▸ greet\s+project/)
    await term.type("/")
    await term.type("diff")
    await term.press("Enter")
    await term.waitForText(/▸ reviewer\s+project\s+Review the diff/)
    await expect.poll(async () => (await term.text()).includes("Say hello to someone")).toBe(false)
    await term.press("Escape")
    await term.waitForText(/greet\s+project\s+Say hello to someone/)
    await term.press("Escape")
    await term.waitForText(placeholder)
    await expect.poll(async () => (await term.text()).includes("── builtin")).toBe(false)
  })

  test("/skills <filter> opens filtered, and rows fit an 80-column terminal", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "greet", "Say hello to someone in a very long description that has to be cut at the edge of a narrow terminal", "Say hello.")
    const term = await tui(hyaTui(backend), { viewport: { width: 700, height: 640 } })
    await term.waitForText(placeholder)
    await command(term, "/skills greet")
    await term.waitForText(/▸ greet\s+project\s+Say hello/)
    await term.waitForText("Filter greet")
    const { cols } = await term.size()
    for (const line of await term.lines()) expect(Bun.stringWidth(line)).toBeLessThanOrEqual(cols)
  })

  test("a skipped skill file is a warning in the startup list and the view; a manual-only skill says so", async ({ tui, backend }) => {
    await writeSkill(backend.dir, "manual", "Run by hand", "Manual body.")
    await writeFile(join(backend.dir, ".hya/skills/manual/SKILL.md"), "---\nname: manual\ndescription: Run by hand\ndisable-model-invocation: true\n---\nManual body.\n")
    await mkdir(join(backend.dir, ".hya/skills/broken"), { recursive: true })
    await writeFile(join(backend.dir, ".hya/skills/broken/SKILL.md"), "---\nname: broken\n---\nno description\n")
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await term.waitForText(/broken\/SKILL\.md: no `description`; skipped · \/skills/)
    await command(term, "/skills manual")
    await term.waitForText(/\d+ skills · 1 warning/)
    await term.waitForText(/! .*broken\/SKILL\.md: no `description`; skipped/)
    await term.press("Enter")
    await term.waitForText("manual · project · only /skill:manual")
  })
})
