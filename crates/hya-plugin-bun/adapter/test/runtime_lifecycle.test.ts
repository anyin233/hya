import { afterEach, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"

import {
  initializeRequest,
  runAdapterProcess,
  shutdownRequest,
} from "./helpers"

afterEach(async () => {
  await cleanup()
})

const tempRoots: string[] = []

async function cleanup(): Promise<void> {
  for (const dir of tempRoots.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
}

test("shutdown runs extension dispose hooks", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "hya-bun-dispose-"))
  tempRoots.push(root)
  const marker = path.join(root, "disposed.txt")
  const extensionFile = path.join(root, "dispose-extension.ts")
  await writeFile(
    extensionFile,
    [
      'import { writeFile } from "node:fs/promises"',
      "export default {",
      '  id: "dispose-test",',
      "  server: async () => ({",
      "    dispose: async () => {",
      '      await writeFile(process.env.HYA_DISPOSE_MARKER, "disposed")',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(1, {
        activation_id: "activation-dispose-test",
        lifecycle: "transient",
      }),
      shutdownRequest(2),
    ],
    {
      argv: ["--bundle-extension", extensionFile],
      env: { HYA_DISPOSE_MARKER: marker },
    },
  )

  expect(responses[1]?.result).toEqual({})
  await expect(readFile(marker, "utf8")).resolves.toBe("disposed")
})
