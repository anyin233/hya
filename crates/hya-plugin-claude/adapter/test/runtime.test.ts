import { afterEach, describe, expect, test } from "bun:test"
import { mkdir, readFile, rm, writeFile } from "node:fs/promises"
import path from "node:path"

import {
  cleanupTempDirs,
  initializeRequest,
  makePluginDir,
  makeTempDir,
  runAdapterProcess,
} from "./helpers"
import { translatePlugin } from "../src/translate"

afterEach(cleanupTempDirs)

describe("runtime handshake", () => {
  test("initialize declares the claude kind, configured id, and skills", async () => {
    const dir = await makePluginDir({
      name: "handshake",
      agents: [{ name: "worker", body: "Work." }],
      hooksJson: {
        PreToolUse: [{ matcher: "", hooks: [{ type: "command", command: "true" }] }],
      },
    })
    const { responses, exitCode } = await runAdapterProcess(
      [initializeRequest(1), { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} }],
      { argv: ["--plugin-dir", dir, "--plugin-id", "handshake"] },
    )
    expect(exitCode).toBe(0)
    expect(responses).toHaveLength(2)
    const init = responses[0]?.result as Record<string, unknown>
    expect((init["plugin"] as Record<string, unknown>)["id"]).toBe("handshake")
    expect((init["plugin"] as Record<string, unknown>)["kind"]).toBe("claude")
    expect(init["protocol_version"]).toBe(1)
    expect(init["tools"]).toEqual([])
    const skills = init["skills"] as { id: string; digest: string }[]
    expect(skills).toEqual([])
    const hooks = init["hooks"] as { name: string }[]
    expect(hooks.map((hook) => hook.name)).toEqual(["tool.execute.before"])
  })

  test("initialize rejects a non-plugin directory", async () => {
    const { responses } = await runAdapterProcess(
      [initializeRequest(1), { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} }],
      { argv: ["--plugin-dir", "/tmp/not-a-plugin"] },
    )
    expect(responses[0]?.error?.code).toBe(-32602)
    expect(responses[0]?.error?.message).toContain("plugin.json")
  })

  test("initializes from a self-contained installed bundle runtime snapshot", async () => {
    const plugin = await makePluginDir({
      skills: [{ name: "scan", body: "Scan." }],
      hooksJson: { PreToolUse: [{ hooks: [{ type: "command", command: "true" }] }] },
    })
    const translation = translatePlugin(plugin)
    const snapshot = translation.files.find((file) => file.path === "runtime/claude-plugin.json")
    expect(snapshot).toBeDefined()
    const root = await makeTempDir()
    await mkdir(path.join(root, "runtime"), { recursive: true })
    const runtimeFile = path.join(root, "runtime/claude-plugin.json")
    await writeFile(runtimeFile, snapshot?.content ?? "")
    const { responses, exitCode } = await runAdapterProcess(
      [initializeRequest(1), { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} }],
      { argv: ["--bundle-runtime", runtimeFile, "--plugin-id", "installed"] },
    )
    expect(exitCode).toBe(0)
    const init = responses[0]?.result as Record<string, unknown>
    expect((init["skills"] as { id: string }[]).map((skill) => skill.id)).toEqual(["scan"])
    expect((init["hooks"] as { name: string }[]).map((hook) => hook.name)).toEqual(["tool.execute.before"])
  })

  test("runs native lifecycle hooks from a materialized bundle after source removal", async () => {
    const plugin = await makePluginDir({
      hooksJson: {
        SessionStart: [{ hooks: [{ type: "command", command: "printf started > ${CLAUDE_PLUGIN_ROOT}/marker" }] }],
      },
    })
    const translation = translatePlugin(plugin)
    const root = await makeTempDir()
    for (const file of translation.files) {
      const target = path.join(root, file.path)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, file.content)
    }
    await rm(plugin, { recursive: true, force: true })
    const runtimeFile = path.join(root, "runtime/claude-plugin.json")
    const run = await runAdapterProcess([
      initializeRequest(1),
      { jsonrpc: "2.0", method: "hook/session.start", params: { session: "ses_test" } },
      { jsonrpc: "2.0", id: 2, method: "shutdown", params: {} },
    ], { argv: ["--bundle-runtime", runtimeFile, "--plugin-id", "installed"] })
    expect(run.exitCode).toBe(0)
    expect(await readFile(path.join(root, "claude-plugin/marker"), "utf8")).toBe("started")
  })
})

describe("hook dispatch", () => {
  test("translates a CC block decision into a hya veto", async () => {
    const dir = await makePluginDir({
      name: "veto",
      hooksJson: {
        PreToolUse: [
          {
            matcher: "Bash",
            hooks: [
              {
                type: "command",
                command: `input=$(cat); printf '%s' "$input" | grep -q '"tool_name":"Bash"' && printf '{"decision":"block","reason":"denied by policy"}'`,
              },
            ],
          },
        ],
      },
    })
    const { responses } = await runAdapterProcess(
      [
        initializeRequest(1),
        {
          jsonrpc: "2.0",
          id: 2,
          method: "hook/tool.execute.before",
          params: {
            session: "s",
            message: "m",
            call: "c1",
            tool: "bash",
            input: { command: "ls" },
          },
        },
        {
          jsonrpc: "2.0",
          id: 3,
          method: "hook/tool.execute.before",
          params: {
            session: "s",
            message: "m",
            call: "c2",
            tool: "read",
            input: { path: "x" },
          },
        },
        { jsonrpc: "2.0", id: 4, method: "shutdown", params: {} },
      ],
      { argv: ["--plugin-dir", dir] },
    )
    expect(responses).toHaveLength(4)
    expect(responses[1]?.result).toEqual({ outcome: "veto", reason: "denied by policy" })
    expect(responses[2]?.result).toEqual({ outcome: "continue", input: { path: "x" } })
  })

  test("tool/call is not served in v1", async () => {
    const dir = await makePluginDir({ name: "notools" })
    const { responses } = await runAdapterProcess(
      [
        initializeRequest(1),
        { jsonrpc: "2.0", id: 5, method: "tool/call", params: { tool: "x", session: "s", call: "c", input: {} } },
        { jsonrpc: "2.0", id: 6, method: "shutdown", params: {} },
      ],
      { argv: ["--plugin-dir", dir] },
    )
    expect(responses[1]?.error?.code).toBe(-32601)
  })
})
