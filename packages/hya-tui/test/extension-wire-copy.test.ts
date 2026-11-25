import { expect, test } from "bun:test"
import { readFile } from "node:fs/promises"

test("packaged TUI extension wire declarations match the SDK source", async () => {
  const [source, packaged] = await Promise.all([
    readFile(new URL("../../hya-tui-sdk/src/wire.ts", import.meta.url)),
    readFile(new URL("../src/extensions/wire.generated.ts", import.meta.url)),
  ])
  expect(packaged.equals(source), "Run bun packages/hya-tui-sdk/scripts/copy-wire.ts from the repo root").toBe(true)
})
