import { expect, test } from "bun:test"
import { readdir, readFile } from "node:fs/promises"
import path from "node:path"
import { Definitions } from "../src/upstream/config/keybind"

const repoRoot = path.resolve(import.meta.dir, "../../..")

test("shipped keybind inventory matches command docs and omits pruned move-session", async () => {
  const names = Object.keys(Definitions)
  expect(names).toContain("agent_models")
  expect(names.filter((name) => name.startsWith("dialog.move_session"))).toEqual([])

  const keyDocs = await readFile(path.join(repoRoot, "docs/tui-keybindings.md"), "utf8")
  expect(keyDocs).not.toContain("dialog.move_session")
  const overrideSection = keyDocs.split("Map of every accepted config key")[1]?.split("## Binding collisions")[0]
  expect(overrideSection).toBeTruthy()
  for (const name of names) {
    expect(overrideSection, name).toContain("`" + name + "`")
  }

  const cli = await readFile(path.join(repoRoot, "docs/cli.md"), "utf8")
  expect(cli).toMatch(new RegExp(String.raw`Definitions\`: \*\*${names.length}\*\* named entries`))
  expect(cli).toMatch(/\/agent-models[\s\S]{0,220}Session override/)
  expect(cli).toMatch(/\/agent-models[\s\S]{0,280}Ctrl\+S/)

  const testFiles = (await readdir(import.meta.dir)).filter((file) => /\.tsx?$/.test(file))
  const readme = await readFile(path.join(import.meta.dir, "README.md"), "utf8")
  expect(readme).toContain(`${testFiles.length} Bun test files`)
})
