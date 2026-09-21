import { afterEach, describe, expect, test } from "bun:test"

import {
  cleanupTempDirs,
  initializeRequest,
  makePluginDir,
  runAdapterProcess,
} from "./helpers"

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
    expect(skills.map((skill) => skill.id)).toEqual(["worker"])
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
                command: `printf '{"decision":"block","reason":"denied by policy"}'`,
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
            tool: "Bash",
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
            tool: "Read",
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
