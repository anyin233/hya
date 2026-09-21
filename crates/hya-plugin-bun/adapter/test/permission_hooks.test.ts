import { afterEach, expect, test } from "bun:test"
import { writeFile } from "node:fs/promises"
import path from "node:path"

import {
  cleanupTempDirs,
  initializeRequest,
  makeTempDir,
  runAdapterProcess,
  shutdownRequest,
} from "./helpers"

afterEach(async () => {
  await cleanupTempDirs()
})

test("permission.ask passes hya params through and maps statuses verbatim", async () => {
  const root = await makeTempDir()
  const extensionFile = path.join(root, "permission.ts")
  await writeFile(
    extensionFile,
    [
      "export default {",
      '  id: "permission",',
      "  server: async () => ({",
      '    "permission.ask": async (params) => {',
      '      if (params.session !== "session-1") throw new Error("bad session")',
      '      if (params.action === "bash" && params.resource.value === "git status") {',
      '        return { outcome: "reject", feedback: "not today" }',
      "      }",
      '      if (params.action === "edit") return "allow_once"',
      '      if (params.action === "write") return "allow_always"',
      '      return "defer"',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(1, {
        activation_id: "activation-permission-test",
        lifecycle: "transient",
      }),
      permissionRequest(2, "bash", { type: "command", value: "git status" }),
      permissionRequest(3, "edit", { type: "path", value: "README.md" }),
      permissionRequest(4, "write", { type: "path", value: "README.md" }),
      permissionRequest(5, "read", { type: "path", value: "README.md" }),
      shutdownRequest(6),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[1]?.result).toEqual({
    outcome: "reject",
    feedback: "not today",
  })
  expect(responses[2]?.result).toEqual({ outcome: "allow_once" })
  expect(responses[3]?.result).toEqual({ outcome: "allow_always" })
  expect(responses[4]?.result).toEqual({ outcome: "defer" })
})

test("permission.ask defers when no handler answers", async () => {
  const responses = await runAdapterProcess([
    initializeRequest(11),
    permissionRequest(12, "bash", { type: "command", value: "ls" }),
    shutdownRequest(13),
  ])

  expect(responses[1]?.result).toEqual({ outcome: "defer" })
})

function permissionRequest(id: number, action: string, resource: unknown): unknown {
  return {
    jsonrpc: "2.0",
    id,
    method: "hook/permission.ask",
    params: {
      session: "session-1",
      action,
      resource,
    },
  }
}
