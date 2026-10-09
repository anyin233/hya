// Shared isolated backend setup for browser and direct-PTY TUI tests.
import { spawn, type ChildProcess } from "node:child_process"
import { existsSync } from "node:fs"
import { mkdir, writeFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { FakeModel, Protocol } from "./fake-model"

export const repoRoot = fileURLToPath(new URL("../../..", import.meta.url))

/** TUI entry under test: `HYA_TUI_MAIN` (e.g. an older checkout, to show a spec fails without a change), else this repository's. */
export const tuiMain = process.env.HYA_TUI_MAIN ?? join(repoRoot, "packages/hya-tui/src/main.ts")

/** `hya` binary under test: `HYA_BIN`, else the workspace debug build. */
export const hyaBin = process.env.HYA_BIN ?? join(repoRoot, "target/debug/hya")

export type Backend = {
  url: string
  /** Isolated workspace directory, passed to the TUI as `--dir`. */
  dir: string
}

/** Provider/model id the fake model is registered under when `model` is set. */
export const fakeModelRef = "fake/model"

/** hya provider kind that speaks each fake-model protocol. */
const providerKinds: Record<Protocol, string> = { chat: "openai-compatible", responses: "openai-response" }

/** `permission.model` written to the backend config when a fake model is used. */
export type PermissionModel = "default" | "allow" | "danger"

/** Files of one bundle source directory, by path relative to it (`bundle.yaml`, …). */
export type BundleFiles = Record<string, string>

/** The Bun adapter entry (`crates/hya-plugin-bun/adapter`) that hosts a bundle's JS extension. */
export const bunAdapterMain = join(repoRoot, "crates/hya-plugin-bun/adapter/src/main.ts")

/** Write project bundles into `<dir>/.hya/bundles/<name>/`; they load once a session's workdir (`dir`) ensures a Project rooted there. */
async function writeProjectBundles(dir: string, bundles: Record<string, BundleFiles>): Promise<void> {
  for (const [name, files] of Object.entries(bundles)) {
    for (const [path, content] of Object.entries(files)) {
      const target = join(dir, ".hya/bundles", name, path)
      await mkdir(dirname(target), { recursive: true })
      await writeFile(target, content)
    }
  }
}

/** Settings of the isolated backend (the `model` and `projectBundles` options). */
export type BackendSetup = {
  fakeModel: FakeModel | undefined
  protocol: Protocol
  permission: PermissionModel
  bundles: Record<string, BundleFiles> | undefined
  modelIds?: string[]
  contextLimit?: number
  /** `modalities.input` per model id (docs/configuration.md), e.g. `{ model: ["text"] }` to make a model refuse image attachments. */
  modelModalities?: Record<string, string[]>
  /** `agents.<id>.model` pins in the backend config, by agent id (docs/configuration.md). */
  agentModels?: Record<string, string>
  mcpServers?: Record<string, McpServerOption>
}

/** One stdio MCP server of the backend config's `mcp:` map (docs/configuration.md). */
export type McpServerOption = {
  /** argv of the server process, e.g. `mcpToolsServer(40)`. */
  command: string[]
}

/** argv of the fixture stdio MCP server (e2e/fixtures/mcp-tools-server.ts) listing `count` tools `tool_01`…. */
export function mcpToolsServer(count: number): string[] {
  return ["bun", fileURLToPath(new URL("./fixtures/mcp-tools-server.ts", import.meta.url)), String(count)]
}

/** The backend config's `agents:` map of pinned models; empty when no agent is pinned. */
function agentsYaml(models: Record<string, string> | undefined): string {
  const entries = Object.entries(models ?? {})
  if (entries.length === 0) return ""
  return "agents:\n" + entries.map(([agent, model]) => `  ${JSON.stringify(agent)}:\n    model: ${JSON.stringify(model)}\n`).join("")
}

/** The backend config's `mcp:` map; JSON arrays and strings are valid YAML flow values. */
function mcpYaml(servers: Record<string, McpServerOption> | undefined): string {
  const entries = Object.entries(servers ?? {})
  if (entries.length === 0) return "mcp: {}\n"
  return "mcp:\n" + entries.map(([name, server]) => `  ${JSON.stringify(name)}:\n    command: ${JSON.stringify(server.command)}\n`).join("")
}

/**
 * Create the isolated HOME/XDG directories, the workspace `dir`, project
 * bundles, and (with a fake model) the backend config under `root`; returns
 * the environment a `hya` process needs to use them.
 */
export async function prepareBackend(root: string, setup: BackendSetup): Promise<{ dir: string; env: Record<string, string> }> {
  const { fakeModel, protocol, permission, bundles, modelIds = ["model"], contextLimit, modelModalities, agentModels, mcpServers } = setup
  const dir = join(root, "work")
  const env: Record<string, string> = {}
  for (const name of ["home", "config", "data", "state", "cache"]) {
    env[name] = join(root, name)
    await mkdir(env[name]!, { recursive: true })
  }
  await mkdir(dir, { recursive: true })
  if (bundles) await writeProjectBundles(dir, bundles)
  if (fakeModel) {
    const hyaCfgDir = join(env.config!, "hya")
    await mkdir(join(hyaCfgDir, "auth"), { recursive: true })
    const limit = contextLimit ? `        limit: { context: ${contextLimit} }\n` : ""
    const modelsYaml = modelIds.map((id) => {
      const modalities = modelModalities?.[id]
      const modalitiesYaml = modalities ? `        modalities: { input: [${modalities.join(", ")}] }\n` : ""
      return `      - id: ${id}\n${limit}${modalitiesYaml}`
    }).join("")
    await writeFile(
      join(hyaCfgDir, "config.yaml"),
      `default_model: ${fakeModelRef}\n` +
        agentsYaml(agentModels) +
        "providers:\n" +
        "  fake:\n" +
        `    kind: ${providerKinds[protocol]}\n` +
        `    base_url: ${fakeModel.baseUrl}\n` +
        "    api_key: e2e-test-key\n" +
        "    models:\n" +
        modelsYaml +
        mcpYaml(mcpServers) +
        "plugins: {}\n" +
        "permission:\n" +
        `  model: ${permission}\n` +
        "  rules: []\n",
    )
    await writeFile(join(hyaCfgDir, "auth", "fake.yaml"), "token: e2e-test-key\n")
  } else if (mcpServers) {
    // No fake model: the offline echo model, plus the MCP servers.
    const hyaCfgDir = join(env.config!, "hya")
    await mkdir(hyaCfgDir, { recursive: true })
    await writeFile(join(hyaCfgDir, "config.yaml"), mcpYaml(mcpServers))
  }
  return {
    dir,
    env: { HOME: env.home!, XDG_CONFIG_HOME: env.config!, XDG_DATA_HOME: env.data!, XDG_STATE_HOME: env.state!, XDG_CACHE_HOME: env.cache! },
  }
}

export async function startBackend(root: string, setup: BackendSetup): Promise<{ child: ChildProcess; backend: Backend }> {
  const { dir, env } = await prepareBackend(root, setup)
  const { HYA_MODEL: _model, ...inherited } = process.env
  const child = spawn(hyaBin, ["serve", "--bind", "127.0.0.1:0", "--db", join(root, "hya.db")], {
    cwd: dir,
    env: { ...inherited, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  })
  return new Promise((resolve, reject) => {
    let output = ""
    const onData = (chunk: Buffer) => {
      output += chunk.toString()
      const match = /hya server listening on (\S+)/.exec(output)
      if (match) resolve({ child, backend: { url: match[1]!, dir } })
    }
    child.stdout!.on("data", onData)
    child.stderr!.on("data", (chunk: Buffer) => (output += chunk.toString()))
    child.once("error", reject)
    child.once("exit", (code) => reject(new Error(`hya serve exited (${code}): ${output.slice(-2000)}`)))
  })
}

export function requireHya(): void {
  if (!existsSync(hyaBin)) {
    throw new Error(`hya binary not found at ${hyaBin}; run \`cargo build -p hya-backend --bin hya\` or set HYA_BIN`)
  }
}


/** One v1 HTTP/JSON call against `backend` (a second client next to the TUI); a scoped rpc names `backend.dir` in its `directory` field. */
export async function api<T>(backend: Backend, method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${backend.url}${path}`, {
    method,
    headers: body === undefined ? {} : { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  })
  const text = await response.text()
  if (!response.ok) throw new Error(`${method} ${path}: HTTP ${response.status} ${text}`)
  return text ? (JSON.parse(text) as T) : (undefined as T)
}


/** argv that runs packages/hya-tui against `backend`. */
export function hyaTui(backend: Backend): string[] {
  return ["bun", tuiMain, "--server", backend.url, "--dir", backend.dir]
}

