import { afterEach, expect, test } from "bun:test"
import { writeFile } from "node:fs/promises"
import path from "node:path"

import { runToolExecuteAfterHooks, runToolExecuteBeforeHooks } from "../src/hooks"
import { loadExtensionContributions } from "../src/loader/init"
import type { ExtensionHooks } from "../src/loader/init"
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

async function loadHooks(body: string): Promise<readonly ExtensionHooks[]> {
  const root = await makeTempDir()
  const file = path.join(root, "extension.ts")
  await writeFile(file, body)
  const loaded = await loadExtensionContributions([file], {})
  return loaded.hooks
}

const beforeParams = {
  session: "session-1",
  message: "message-1",
  call: "call-1",
  tool: "bash",
}

const afterBase = {
  session: "session-1",
  message: "message-1",
  call: "call-1",
  tool: "read",
  input: { filePath: "README.md" },
}

test("tool.execute.before handlers rewrite input by returning a replacement", async () => {
  const hooks = await loadHooks(
    [
      "export default {",
      '  id: "before",',
      "  server: async () => ({",
      '    "tool.execute.before": async (params) => {',
      '      if (params.tool !== "bash") throw new Error("wrong tool")',
      '      if (params.session !== "session-1") throw new Error("wrong session")',
      '      return { input: { command: `${params.input.command} --safe` } }',
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const outcome = await runToolExecuteBeforeHooks(hooks, {
    ...beforeParams,
    input: { command: "ls" },
  })

  expect(outcome).toEqual({
    outcome: "continue",
    input: { command: "ls --safe" },
  })
})

test("tool.execute.before handlers veto by returning an outcome", async () => {
  const hooks = await loadHooks(
    [
      "export default {",
      '  id: "veto",',
      "  server: async () => ({",
      '    "tool.execute.before": async () => ({ outcome: "veto", reason: "blocked" }),',
      "  }),",
      "}",
    ].join("\n"),
  )

  const outcome = await runToolExecuteBeforeHooks(hooks, {
    ...beforeParams,
    input: { command: "rm -rf /tmp/nope" },
  })

  expect(outcome).toEqual({ outcome: "veto", reason: "blocked" })
})

test("tool.execute.before throws map to veto", async () => {
  const hooks = await loadHooks(
    [
      "export default {",
      '  id: "throw",',
      "  server: async () => ({",
      '    "tool.execute.before": async () => { throw new Error("blocked") },',
      "  }),",
      "}",
    ].join("\n"),
  )

  const outcome = await runToolExecuteBeforeHooks(hooks, {
    ...beforeParams,
    input: { command: "ls" },
  })

  expect(outcome).toEqual({ outcome: "veto", reason: "blocked" })
})

test("tool.execute.after handlers fold ok results", async () => {
  const hooks = await loadHooks(
    [
      "export default {",
      '  id: "after",',
      "  server: async () => ({",
      '    "tool.execute.after": async (params) => ({',
      "      ...params.result,",
      '      output: {',
      '        title: "After",',
      '        output: `${params.result.output.output}!`,',
      "        metadata: { ...params.result.output.metadata, extra: true },",
      "      },",
      "    }),",
      "  }),",
      "}",
    ].join("\n"),
  )

  const outcome = await runToolExecuteAfterHooks(hooks, {
    ...afterBase,
    result: {
      status: "ok",
      output: { title: "Before", output: "hello", metadata: { base: true } },
      time_ms: 7,
    },
  })

  expect(outcome).toEqual({
    outcome: "continue",
    result: {
      status: "ok",
      output: {
        title: "After",
        output: "hello!",
        metadata: { base: true, extra: true },
      },
      time_ms: 7,
    },
  })
})

test("tool.execute.after preserves hya error results", async () => {
  const hooks = await loadHooks(
    [
      "export default {",
      '  id: "after-error",',
      "  server: async () => ({",
      '    "tool.execute.after": async () => ({',
      '      status: "ok",',
      '      output: { title: "Success", output: "masked success", metadata: {} },',
      "    }),",
      "  }),",
      "}",
    ].join("\n"),
  )

  const outcome = await runToolExecuteAfterHooks(hooks, {
    ...afterBase,
    result: {
      status: "err",
      message: "permission denied: edit on README.md",
    },
  })

  expect(outcome).toEqual({
    outcome: "continue",
    result: {
      status: "err",
      message: "permission denied: edit on README.md",
    },
  })
})

test("wire hook methods dispatch through the adapter process", async () => {
  const root = await makeTempDir()
  const extensionFile = path.join(root, "wired-extension.ts")
  await writeFile(
    extensionFile,
    [
      "export default {",
      '  id: "wired",',
      "  server: async () => ({",
      '    "message.user.before": async (params) => `${params.text} (checked)`,',
      '    "experimental.text.complete": async (params) => `${params.text}!`,',
      '    "command.execute.before": async () => "rewritten",',
      '    "chat.params": async (params) => ({ ...params.request, temperature: 0 }),',
      "  }),",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapterProcess(
    [
      initializeRequest(1, {
        activation_id: "activation-wired-test",
        lifecycle: "transient",
      }),
      {
        jsonrpc: "2.0",
        id: 2,
        method: "hook/message.user.before",
        params: { session: "s", text: "hello" },
      },
      {
        jsonrpc: "2.0",
        id: 3,
        method: "hook/experimental.text.complete",
        params: { session: "s", message: "m", part: "p", text: "done" },
      },
      {
        jsonrpc: "2.0",
        id: 4,
        method: "hook/command.execute.before",
        params: { session: "s", command: "review", arguments: "commit", text: "original" },
      },
      {
        jsonrpc: "2.0",
        id: 5,
        method: "hook/chat.params",
        params: {
          session: "s",
          message: "m",
          request: { model: "test-model", messages: [], tools: [], temperature: 1 },
        },
      },
      shutdownRequest(6),
    ],
    { argv: ["--bundle-extension", extensionFile] },
  )

  expect(responses[1]?.result).toEqual({ outcome: "continue", text: "hello (checked)" })
  expect(responses[2]?.result).toEqual({ outcome: "continue", text: "done!" })
  expect(responses[3]?.result).toEqual({ outcome: "continue", text: "rewritten" })
  expect(responses[4]?.result).toEqual({
    outcome: "continue",
    request: {
      model: "test-model",
      messages: [],
      tools: [],
      temperature: 0,
    },
  })
})
