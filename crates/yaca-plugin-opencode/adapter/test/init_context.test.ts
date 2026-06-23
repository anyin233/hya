import { afterEach, expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { z } from "zod"

const AdapterResponseSchema = z.object({
  jsonrpc: z.literal("2.0"),
  id: z.number().int(),
  result: z.unknown().optional(),
})

const InitializeResultSchema = z.object({
  hooks: z.array(z.object({ name: z.string() })),
})

const tempDirs: string[] = []

afterEach(async () => {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
})

test("initialize passes OpenCode shell and project time to local plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "input-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "input",',
      "  server: async (input) => {",
      '    if (typeof input.$ !== "function") throw new Error("missing shell")',
      '    if (typeof input.project.time.created !== "number") throw new Error("missing project time")',
      "    return { event: async () => {} }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile)

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([{ name: "event" }])
})

test("initialize passes an OpenCode app log client to local plugins", async () => {
  const root = await makeTempDir()
  const pluginFile = path.join(root, "client-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "client",',
      "  server: async (input) => {",
      "    const logged = await input.client.app.log({",
      '      service: "test-plugin",',
      '      level: "info",',
      '      message: "loaded",',
      "      extra: { plugin: true },",
      "    })",
      '    if (logged.data !== true) throw new Error("log data mismatch")',
      '    if (logged.response.status !== 200) throw new Error("log status mismatch")',
      "    return { event: async () => {} }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile)

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([{ name: "event" }])
})

test("initialize passes OpenCode project and path clients to local plugins", async () => {
  const root = await makeTempDir()
  const configRoot = path.join(root, "config-home")
  const stateRoot = path.join(root, "state-home")
  const pluginFile = path.join(root, "project-path-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "project-path",',
      "  server: async (input) => {",
      "    const project = await input.client.project.current()",
      '    if (project.response.status !== 200) throw new Error("project status mismatch")',
      '    if (project.data.id !== "project-42") throw new Error("project id mismatch")',
      "    if (project.data.worktree !== input.worktree) throw new Error(\"project worktree mismatch\")",
      "    const projects = await input.client.project.list()",
      '    if (projects.response.status !== 200) throw new Error("project list status mismatch")',
      '    if (projects.data.length !== 1 || projects.data[0].id !== "project-42") throw new Error("project list mismatch")',
      "    const config = await input.client.config.get()",
      '    if (config.response.status !== 200 || typeof config.data !== "object") throw new Error("config mismatch")',
      "    const agents = await input.client.app.agents()",
      '    if (agents.response.status !== 200 || !Array.isArray(agents.data)) throw new Error("agents mismatch")',
      "    const skills = await input.client.app.skills()",
      '    if (skills.response.status !== 200 || !Array.isArray(skills.data)) throw new Error("skills mismatch")',
      "    const toolIDs = await input.client.tool.ids()",
      '    if (toolIDs.response.status !== 200 || !Array.isArray(toolIDs.data)) throw new Error("tool ids mismatch")',
      '    const authSet = await input.client.auth.set({ providerID: "test" })',
      '    if (authSet.response.status !== 400 || authSet.error?.name !== "BadRequest") throw new Error("auth set mismatch")',
      '    const authRemove = await input.client.auth.remove({ providerID: "test" })',
      '    if (authRemove.response.status !== 400 || authRemove.error?.name !== "BadRequest") throw new Error("auth remove mismatch")',
      "    const formatter = await input.client.formatter.status()",
      '    if (formatter.response.status !== 200 || formatter.data.length !== 0) throw new Error("formatter status mismatch")',
      "    const lsp = await input.client.lsp.status()",
      '    if (lsp.response.status !== 200 || lsp.data.length !== 0) throw new Error("lsp status mismatch")',
      "    const paths = await input.client.path.get()",
      '    if (paths.response.status !== 200) throw new Error("path status mismatch")',
      "    if (paths.data.directory !== input.directory) throw new Error(\"directory mismatch\")",
      "    if (paths.data.worktree !== input.worktree) throw new Error(\"worktree mismatch\")",
      `    if (paths.data.config !== ${JSON.stringify(path.join(configRoot, "opencode"))}) throw new Error("config mismatch")`,
      `    if (paths.data.state !== ${JSON.stringify(path.join(stateRoot, "opencode"))}) throw new Error("state mismatch")`,
      "    return { event: async () => {} }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile, {
    YACA_PROJECT_ID: "project-42",
    XDG_CONFIG_HOME: configRoot,
    XDG_STATE_HOME: stateRoot,
  })

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([{ name: "event" }])
})

test("initialize passes an OpenCode vcs client to local plugins", async () => {
  const root = await makeTempDir()
  await runCommand(root, ["git", "init", "--initial-branch", "main"])
  await runCommand(root, ["git", "config", "user.email", "test@example.com"])
  await runCommand(root, ["git", "config", "user.name", "Test User"])
  await runCommand(root, ["git", "commit", "--allow-empty", "-m", "initial"])
  const pluginFile = path.join(root, "vcs-plugin.ts")
  await writeFile(
    pluginFile,
    [
      "export default {",
      '  id: "vcs",',
      "  server: async (input) => {",
      "    const vcs = await input.client.vcs.get()",
      '    if (vcs.response.status !== 200) throw new Error("vcs status mismatch")',
      '    if (vcs.data.branch !== "main") throw new Error("branch mismatch")',
      '    if (vcs.data.default_branch !== "main") throw new Error("default branch mismatch")',
      "    return { event: async () => {} }",
      "  },",
      "}",
    ].join("\n"),
  )

  const responses = await runAdapter(root, pluginFile)

  const result = InitializeResultSchema.parse(responses[0]?.result)
  expect(result.hooks).toEqual([{ name: "event" }])
})

async function runAdapter(
  root: string,
  pluginFile: string,
  env?: Readonly<Record<string, string>>,
): Promise<readonly z.infer<typeof AdapterResponseSchema>[]> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: {
      ...process.env,
      YACA_OPENCODE_OPTIONS_JSON: JSON.stringify({
        plugin: [pathToFileURL(pluginFile).href],
      }),
      YACA_DIRECTORY: root,
      YACA_WORKTREE: root,
      ...env,
    },
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const stdin = proc.stdin
  if (stdin === undefined) {
    throw new Error("adapter stdin pipe was not created")
  }
  stdin.write(
    `${JSON.stringify({
      jsonrpc: "2.0",
      id: 51,
      method: "initialize",
      params: { protocol_version: 1, host: { name: "yaca", version: "0.0.0" } },
    })}\n`,
  )
  stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 52, method: "shutdown", params: {} })}\n`)
  stdin.end()

  const [stdout, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    proc.exited,
  ])
  expect(exitCode).toBe(0)
  return stdout
    .trim()
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => {
      const value: unknown = JSON.parse(line)
      return AdapterResponseSchema.parse(value)
    })
}

async function makeTempDir(): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), "yaca-opencode-"))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}

async function runCommand(cwd: string, command: string[]): Promise<void> {
  const proc = Bun.spawn(command, {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (exitCode !== 0) {
    throw new Error(`${command.join(" ")} failed: ${stdout}${stderr}`)
  }
}
