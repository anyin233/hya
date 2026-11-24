// `/reload` and `/reload-<target>` (docs/tui.md "Reload"; app/reload.ts):
// resources added or edited after the TUI started are read again without a
// restart — skills and commands into the command menu, `tui.toml` into the
// structure, and the open session's recorded context at its next turn.

import { mkdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { backendConfigDir, hyaTui, test, textStep } from "./hya"

const placeholder = "Message, !shell, or @file · / commands"

async function command(term: Tui, text: string): Promise<void> {
  await term.type(text)
  await term.waitForText(text)
  await term.press("Enter")
}

test.describe("hya TUI reload", () => {
  test.use({ model: { steps: [textStep("Reply one.")] } })

  test("/reload reads a skill and a command added after start into the menu and the resource list", async ({ tui, backend }, testInfo) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await mkdir(join(backend.dir, ".hya/skills/fresh-skill"), { recursive: true })
    await writeFile(join(backend.dir, ".hya/skills/fresh-skill/SKILL.md"), "---\nname: fresh-skill\ndescription: Added after start\n---\nFresh.\n")
    await mkdir(join(backend.dir, ".hya/commands"), { recursive: true })
    await writeFile(join(backend.dir, ".hya/commands/fresh-command.md"), "---\ndescription: Added after start\n---\nRun $ARGUMENTS\n")

    await command(term, "/reload")
    await term.waitForText(/Reloaded tui\.toml/, 20_000)
    await term.waitForText(/\d+\s+commands?,\s+\d+\s+skills/)
    await term.waitForText(/\[Skills\].*fresh-skill/)
    await term.attach(testInfo, "reloaded")

    await term.type("/fresh")
    await term.waitForText("▸ /fresh-command")
    await term.waitForText("/skill:fresh-skill")
  })

  test("/reload-context after a turn re-records the session's context at the next turn", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await command(term, "hello")
    await term.waitForText("Reply one.", 20_000)
    await term.waitForIdle(20_000)
    await command(term, "/reload-context")
    await term.waitForText("Reloaded context (re-recorded at the next turn)")
  })

  test("/reload-config applies an edited tui.toml", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText(placeholder)
    await writeFile(join(backendConfigDir(backend), "tui.toml"), "preset = \"minimal\"\n")
    await command(term, "/reload-config")
    await term.waitForText("Reloaded tui.toml (preset minimal)")
  })
})
