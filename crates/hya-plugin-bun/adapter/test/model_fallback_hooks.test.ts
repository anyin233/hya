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
  agent: "explore",
  message: "m",
  model: "anthropic/claude-opus-5-5",
  error: { class: "retryable", message: "http status 529: overloaded" },
  attempt: 1,
  tried: ["anthropic/claude-opus-5-5"],
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
      `    "model.fallback": ${handler},`,
      "  }),",
      "}",
    ].join("\n"),
  )
  return file
}

async function consult(extensions: readonly string[], hookParams: unknown = params) {
  const argv = extensions.flatMap((file) => ["--bundle-extension", file])
  return runAdapterProcess(
    [
      initializeRequest(1, { activation_id: "activation-fallback", lifecycle: "transient" }),
      { jsonrpc: "2.0", id: 2, method: "hook/model.fallback", params: hookParams },
      shutdownRequest(3),
    ],
    { argv },
  )
}

test("model.fallback registers and a handler's retry reaches the host", async () => {
  const file = await extension(
    "chooser",
    [
      "async (params) => ({",
      '      outcome: "retry",',
      "      model: `${params.agent}/${params.root_session}/${params.error.class}/${params.attempt}/${params.tried.length}`,",
      "    })",
    ].join("\n"),
  )
  const responses = await consult([file])
  expect(responses[0]?.result).toMatchObject({ hooks: [{ name: "model.fallback" }] })
  expect(responses[1]?.result).toEqual({
    outcome: "retry",
    model: "explore/root/retryable/1/1",
  })
})

test("model.fallback: a bare model string is a retry; the first retry wins", async () => {
  const givesUp = await extension("gives-up", 'async () => ({ outcome: "give_up" })')
  const throws = await extension("throws", 'async () => { throw new Error("boom") }')
  const empty = await extension("empty", 'async () => ({ outcome: "retry", model: "" })')
  const silent = await extension("silent", "async () => undefined")
  const first = await extension("first", 'async () => "first/model"')
  const second = await extension("second", 'async () => ({ outcome: "retry", model: "second/model" })')
  const responses = await consult([givesUp, throws, empty, silent, first, second])
  expect(responses[1]?.result).toEqual({ outcome: "retry", model: "first/model" })
})

test("model.fallback gives up when no handler retries", async () => {
  const givesUp = await extension("gives-up", 'async () => ({ outcome: "give_up" })')
  const responses = await consult([givesUp])
  expect(responses[1]?.result).toEqual({ outcome: "give_up" })
})

test("model.fallback rejects malformed params", async () => {
  const file = await extension("chooser", 'async () => "x/y"')
  const responses = await consult([file], { ...params, tried: "nope" })
  expect(responses[1]?.error?.message).toBe("params.tried must be an array of strings")
})
