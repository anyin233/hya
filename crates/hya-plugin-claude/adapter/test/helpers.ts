import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"

export type AdapterResponse = {
  readonly jsonrpc: string
  readonly id?: number
  readonly result?: unknown
  readonly error?: { readonly code: number; readonly message: string }
}

const tempDirs: string[] = []

export async function makeTempDir(prefix = "hya-claude-"): Promise<string> {
  const created = await mkdtemp(path.join(tmpdir(), prefix))
  await mkdir(created, { recursive: true })
  tempDirs.push(created)
  return created
}

export async function cleanupTempDirs(): Promise<void> {
  for (const dir of tempDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
}

export type PluginFixture = {
  readonly name?: string
  readonly version?: string
  readonly description?: string
  readonly nested?: boolean
  readonly agents?: readonly { readonly name: string; readonly body: string; readonly tools?: readonly string[]; readonly model?: string }[]
  readonly skills?: readonly { readonly name: string; readonly body: string }[]
  readonly commands?: readonly { readonly name: string; readonly body: string }[]
  readonly mcpServers?: Record<string, unknown>
  readonly hooksJson?: unknown
}

/** Write one Claude Code plugin source tree into a fresh temp directory. */
export async function makePluginDir(fixture: PluginFixture = {}): Promise<string> {
  const dir = await makeTempDir()
  await writePluginDir(dir, fixture)
  return dir
}

/** Write one Claude Code plugin source tree into an existing directory. */
export async function writePluginDir(dir: string, fixture: PluginFixture): Promise<void> {
  const manifest = {
    name: fixture.name ?? "demo-plugin",
    ...(fixture.version === undefined ? {} : { version: fixture.version }),
    ...(fixture.description === undefined ? {} : { description: fixture.description }),
  }
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`
  if (fixture.nested === true) {
    await mkdir(path.join(dir, ".claude-plugin"), { recursive: true })
    await writeFile(path.join(dir, ".claude-plugin", "plugin.json"), manifestText)
  } else {
    await writeFile(path.join(dir, "plugin.json"), manifestText)
  }
  if (fixture.agents !== undefined) {
    await mkdir(path.join(dir, "agents"), { recursive: true })
    for (const agent of fixture.agents) {
      await writeFile(
        path.join(dir, "agents", `${agent.name}.md`),
        `---\nname: ${agent.name}\ndescription: ${agent.name} agent\n${agent.tools === undefined ? "" : `tools: [${agent.tools.join(", ")}]\n`}${agent.model === undefined ? "" : `model: ${agent.model}\n`}---\n${agent.body}\n`,
      )
    }
  }
  if (fixture.skills !== undefined) {
    for (const skill of fixture.skills) {
      const skillDir = path.join(dir, "skills", skill.name)
      await mkdir(skillDir, { recursive: true })
      await writeFile(
        path.join(skillDir, "SKILL.md"),
        `---\nname: ${skill.name}\ndescription: ${skill.name} skill\n---\n${skill.body}\n`,
      )
    }
  }
  if (fixture.commands !== undefined) {
    await mkdir(path.join(dir, "commands"), { recursive: true })
    for (const command of fixture.commands) {
      await writeFile(
        path.join(dir, "commands", `${command.name}.md`),
        `${command.body}\n`,
      )
    }
  }
  if (fixture.mcpServers !== undefined) {
    await writeFile(
      path.join(dir, ".mcp.json"),
      `${JSON.stringify({ mcpServers: fixture.mcpServers }, null, 2)}\n`,
    )
  }
  if (fixture.hooksJson !== undefined) {
    await mkdir(path.join(dir, "hooks"), { recursive: true })
    await writeFile(path.join(dir, "hooks", "hooks.json"), `${JSON.stringify(fixture.hooksJson, null, 2)}\n`)
  }
}

export type RunAdapterOptions = {
  readonly argv?: readonly string[]
  readonly env?: Readonly<Record<string, string>>
}

/** Spawn the adapter process, feed it NDJSON requests, and collect replies. */
export async function runAdapterProcess(
  requests: readonly unknown[],
  options: RunAdapterOptions = {},
): Promise<{ readonly responses: readonly AdapterResponse[]; readonly stderr: string; readonly exitCode: number }> {
  const scriptArgs = options.argv === undefined ? [] : ["--", ...options.argv]
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts", ...scriptArgs], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    env: {
      ...process.env,
      ...options.env,
    },
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const stdin = proc.stdin
  if (stdin === undefined) {
    throw new Error("adapter stdin pipe was not created")
  }
  for (const request of requests) {
    stdin.write(`${JSON.stringify(request)}\n`)
  }
  await stdin.flush()
  stdin.end()

  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  const responses = stdout
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line) as AdapterResponse)
  return { responses, stderr, exitCode }
}

export type EmitRun = {
  readonly stdout: string
  readonly stderr: string
  readonly exitCode: number
}

/** Run the adapter CLI with raw args, returning unparsed output streams. */
export async function runCli(args: readonly string[]): Promise<EmitRun> {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts", "--", ...args], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { stdout, stderr, exitCode }
}

/** Run `--emit-bundle-manifest` for one plugin directory. */
export async function runEmitManifest(pluginDir: string): Promise<EmitRun> {
  const proc = Bun.spawn(
    [process.execPath, "run", "src/main.ts", "--emit-bundle-manifest", "--plugin-dir", pluginDir],
    {
      cwd: import.meta.dir.replace(/\/test$/, ""),
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    },
  )
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  return { stdout, stderr, exitCode }
}

export function initializeRequest(id: number): unknown {
  return {
    jsonrpc: "2.0",
    id,
    method: "initialize",
    params: {
      protocol_version: 1,
      host: { name: "hya-test", version: "0.0.0" },
    },
  }
}
