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

const params = {
  session: "child",
  root_session: "root",
  agent: "build",
  mode: "careful",
  action: "bash",
  resource: { type: "command", value: "git status" },
}

async function extension(name: string, handler: string): Promise<string> {
  const root = await makeTempDir()
  const file = path.join(root, `${name}.ts`)
  await writeFile(
    file,
    [
      "export default {",
      `  id: "${name}",`,
      "  server: async () => ({",
      `    "permission.approve": ${handler},`,
      "  }),",
      "}",
    ].join("\n"),
  )
  return file
}

async function approve(extensions: readonly string[], hookParams: unknown = params) {
  const argv = extensions.flatMap((file) => ["--bundle-extension", file])
  return runAdapterProcess(
    [
      initializeRequest(1, { activation_id: "activation-approve", lifecycle: "transient" }),
      { jsonrpc: "2.0", id: 2, method: "hook/permission.approve", params: hookParams },
      shutdownRequest(3),
    ],
    { argv },
  )
}

test("permission.approve registers and receives the hya params", async () => {
  const file = await extension(
    "approver",
    [
      "async (params) => {",
      '      if (params.mode !== "careful" || params.root_session !== "root") return "defer"',
      '      if (params.agent !== "build" || params.session !== "child") return "defer"',
      '      return params.resource.value === "git status" ? "allow_once" : "reject"',
      "    }",
    ].join("\n"),
  )
  const responses = await approve([file])
  expect(responses[0]?.result).toMatchObject({ hooks: [{ name: "permission.approve" }] })
  expect(responses[1]?.result).toEqual({ outcome: "allow_once" })
})

test("permission.approve maps records and the first non-defer answer wins", async () => {
  const defers = await extension("defers", 'async () => "defer"')
  const throws = await extension("throws", 'async () => { throw new Error("boom") }')
  const silent = await extension("silent", "async () => undefined")
  const rejects = await extension(
    "rejects",
    'async () => ({ outcome: "reject", feedback: "not in careful mode" })',
  )
  const allows = await extension("allows", 'async () => "allow_always"')
  const responses = await approve([defers, throws, silent, rejects, allows])
  expect(responses[1]?.result).toEqual({
    outcome: "reject",
    feedback: "not in careful mode",
  })
})

test("permission.approve defers when nobody answers and rejects bad params", async () => {
  const defers = await extension("defers", 'async () => "defer"')
  expect((await approve([defers]))[1]?.result).toEqual({ outcome: "defer" })

  const { mode: _mode, ...missingMode } = params
  const invalid = await approve([defers], missingMode)
  expect(invalid[1]?.error?.code).toBe(-32602)
})
