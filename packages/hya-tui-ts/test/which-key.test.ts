import { expect, test } from "bun:test"
import { readFile } from "node:fs/promises"
import path from "node:path"

import { CommandMap } from "../src/upstream/config/keybind"
import { loadedBuiltinPlugins } from "../src/upstream/feature-plugins/builtins"
import WhichKey from "../src/upstream/feature-plugins/system/which-key"

const repoRoot = path.resolve(import.meta.dir, "../../..")

test("shipped which-key plugin loads in the static host with mapped commands", async () => {
  expect(WhichKey.id).toBe("which-key")
  expect(WhichKey.enabled).not.toBe(false)
  expect(loadedBuiltinPlugins().map((plugin) => plugin.id)).toContain("which-key")

  expect(CommandMap.which_key_toggle).toBe("which-key.toggle")
  expect(CommandMap.which_key_layout_toggle).toBe("which-key.layout.toggle")
  expect(CommandMap.which_key_pending_toggle).toBe("which-key.pending.toggle")

  const keyDocs = await readFile(path.join(repoRoot, "docs/tui-keybindings.md"), "utf8")
  expect(keyDocs).toContain("`which-key.toggle`")
  expect(keyDocs).not.toMatch(/\*\*Default off:\*\*/)
  expect(keyDocs).not.toContain("enabled: false")

  const tuiDocs = await readFile(path.join(repoRoot, "docs/architecture/tui.md"), "utf8")
  expect(tuiDocs).toContain("which-key")
  expect(tuiDocs).not.toContain("eleven start by default")
  expect(tuiDocs).not.toMatch(/default off/i)
})
