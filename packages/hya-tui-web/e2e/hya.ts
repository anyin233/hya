// hya-specific fixtures: an isolated `hya serve` on the offline echo model
// (or, when a spec opts in, a scripted fake OpenAI model on the Chat
// Completions or Responses protocol) and the
// argv that runs packages/hya-tui against it. The host itself stays generic;
// only these specs know about hya.

import { execFileSync, spawn, type ChildProcess } from "node:child_process"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { startFakeModel, type FakeModel, type Protocol, type Step } from "./fake-model"
import { expect, test as base, type LaunchOptions, type Tui } from "./harness"

import {
  api, hyaTui, bunAdapterMain, fakeModelRef, hyaBin, prepareBackend, requireHya, startBackend, tuiMain,
  type Backend, type BackendSetup, type BundleFiles, type McpServerOption, type PermissionModel,
} from "./backend"
export { api, hyaTui, fakeModelRef, hyaBin, mcpToolsServer, tuiMain, type Backend, type BundleFiles, type McpServerOption, type PermissionModel } from "./backend"

/**
 * A `kind: Plugin` bundle that declares `permission_modes:` and answers
 * `permission.approve` from a Bun process (the repository's Bun adapter
 * loading `approver.ts` as a bundle extension). `approve` is the JS source
 * of the hook handler, called with `{ session, root_session, agent, mode,
 * action, resource }` and returning `allow_once | allow_always | reject |
 * defer`. Select a mode as `<id>/<mode id>`; `GET /v1/permission-modes`
 * lists it with `source: <id>`.
 */
export function approverBundle(options: { id: string; modes: { id: string; title: string; description?: string }[]; approve: string }): BundleFiles {
  const namespace = options.id.split("/").at(-1)!
  const modes = options.modes
    .map((mode) => `  - id: ${mode.id}\n    title: ${JSON.stringify(mode.title)}\n${mode.description ? `    description: ${JSON.stringify(mode.description)}\n` : ""}`)
    .join("")
  return {
    "bundle.yaml":
      "kind: Plugin\n" +
      `identity: { id: ${options.id}, version: 1.0.0, publisher: e2e }\n` +
      "extensions:\n" +
      `  process: { kind: bun, command: [bun, run, ${JSON.stringify(bunAdapterMain)}, --plugin-id, ${namespace}, --bundle-extension, '\${BUNDLE_ROOT}/approver.ts'] }\n` +
      "  files:\n" +
      "    - { id: approver, path: approver.ts }\n" +
      "resources:\n" +
      "  hooks:\n" +
      "    - { id: permission.approve, path: hooks/permission-approve.json }\n" +
      "permission_modes:\n" +
      modes,
    "hooks/permission-approve.json": "{}\n",
    "approver.ts": `export default {\n  id: ${JSON.stringify(namespace)},\n  server: async () => ({\n    "permission.approve": ${options.approve},\n  }),\n}\n`,
  }
}

/**
 * Wraps the step list so the "option" fixture's value is a plain object, not
 * a bare array. Playwright's fixture-option machinery treats an array *value*
 * on an "option" fixture as a set of values to parametrize the test across
 * (one run per array element), which silently drops steps beyond the first;
 * wrapping sidesteps that.
 */
export type FakeModelOption = {
  steps: Step[]
  /**
   * Wire protocol hya uses to reach the fake: `chat` (default) registers it
   * as an `openai-compatible` provider (`/chat/completions`); `responses`
   * registers it as `openai-response` (`/responses`), the route whose decoder
   * streams reasoning, so `reasoningStep` renders thinking only there.
   */
  protocol?: Protocol
  /**
   * `permission.model` of the backend config (default `default`, where
   * `bash`, `edit`, and `write` ask first and create pending permission
   * requests). `allow` approves resource checks unless a rule denies them;
   * `danger` bypasses every check. Specs that exercise tools without
   * answering prompts use `allow`.
   */
  permission?: PermissionModel
  /**
   * Provider model ids registered for the fake model, each reachable as
   * `fake/<id>` (S9: two lets a spec exercise the `/model` picker's rows and
   * a session model switch). Default `["model"]` (`fakeModelRef`), backward
   * compatible with specs that do not set it.
   */
  models?: string[]
  /**
   * `limit.context` of every fake model entry (object-form model entries in
   * the backend config), so `ModelSummary.contextLimit` is known and the
   * status bar can show `ctx N%` (E22). Unset (the default) writes plain
   * entries, backward compatible.
   */
  contextLimit?: number
  /** `modalities.input` per model id (docs/configuration.md), e.g. `{ model: ["text"] }` to make a model refuse image attachments. */
  modelModalities?: Record<string, string[]>
  /** Pin agents' models in the backend config (`agents.<id>.model`), e.g. `{ "hya-main": "fake/beta" }`. */
  agentModels?: Record<string, string>
}

/**
 * An isolated workspace for a TUI that starts its own backend (one-command
 * launch, G31): the same HOME/XDG directories, config, and project bundles
 * as `backend`, but no running server. Launch with
 * `tui(...selfLaunch(workspace))`.
 */
export type Workspace = {
  root: string
  /** Workspace directory, passed to the TUI as `--dir`. */
  dir: string
  /** Environment for the TUI (and so its `hya serve`): isolated HOME/XDG, `HYA_BIN`. */
  env: Record<string, string>
}

type Fixtures = { backend: Backend; fakeModel: FakeModel | undefined }
type Options = {
  /**
   * Scripted steps for a fake OpenAI-compatible model backing the isolated
   * backend (`test.use({ model: { steps: [textStep("hi")] } })`), consumed by
   * `startFakeModel`. Unset (the default) keeps the existing offline echo
   * model, so pre-existing specs are unaffected.
   */
  model: FakeModelOption | undefined
  /**
   * Project bundles for the isolated backend, by directory name
   * (`test.use({ projectBundles: { approver: approverBundle({...}) } })`),
   * written to `<backend.dir>/.hya/bundles/<name>/` before `hya serve`
   * starts. An object, not an array (see `FakeModelOption`). Unset (the
   * default) writes none.
   */
  projectBundles: Record<string, BundleFiles> | undefined
  /**
   * Stdio MCP servers for the isolated backend's config `mcp:` map, by
   * server name (`test.use({ mcpServers: { many: { command: mcpToolsServer(40) } } })`);
   * `hya serve` connects them at startup. Unset (the default) writes
   * `mcp: {}`.
   */
  mcpServers: Record<string, McpServerOption> | undefined
}

const setupOf = (fakeModel: FakeModel | undefined, model: FakeModelOption | undefined, projectBundles: Record<string, BundleFiles> | undefined, mcpServers?: Record<string, McpServerOption>): BackendSetup => ({
  fakeModel,
  protocol: model?.protocol ?? "chat",
  permission: model?.permission ?? "default",
  bundles: projectBundles,
  ...(model?.models ? { modelIds: model.models } : {}),
  ...(model?.contextLimit ? { contextLimit: model.contextLimit } : {}),
  ...(model?.modelModalities ? { modelModalities: model.modelModalities } : {}),
  ...(model?.agentModels ? { agentModels: model.agentModels } : {}),
  ...(mcpServers ? { mcpServers } : {}),
})

const withOptions = base.extend<{ fakeModel: FakeModel | undefined } & Options>({
  model: [undefined, { option: true }],
  projectBundles: [undefined, { option: true }],
  mcpServers: [undefined, { option: true }],
  fakeModel: async ({ model }, use) => {
    if (!model) {
      await use(undefined)
      return
    }
    const fake = await startFakeModel(model.steps)
    await use(fake)
    await fake.stop()
  },
})

/**
 * Specs of a TUI that starts its own backend (no `--server`): the
 * `workspace` fixture replaces `backend`; the same `model` and
 * `projectBundles` options apply.
 */
export const launchTest = withOptions.extend<{ workspace: Workspace }>({
  workspace: async ({ fakeModel, model, projectBundles }, use) => {
    requireHya()
    const root = await mkdtemp(join(tmpdir(), "hya-tui-launch-"))
    const { dir, env } = await prepareBackend(root, setupOf(fakeModel, model, projectBundles))
    const workspace = { root, dir, env: { ...env, HYA_BIN: hyaBin, ...(fakeModel ? { HYA_MODEL: fakeModelRef } : {}) } }
    await use(workspace)
    // The TUIs' backend daemon outlives them (ADR-0023): stop it with the workspace.
    await daemon(workspace, ["stop", "--force", "--timeout", "10"]).catch(() => undefined)
    await rm(root, { recursive: true, force: true })
  },
  // Capture the final screen while the fake model (a workspace dependency) still runs.
  tui: async ({ tui, workspace: _workspace }, use, testInfo) => {
    let last: Tui | undefined
    await use(async (command, options) => (last = await tui(command, options)))
    if (last) await last.attach(testInfo, "final-screen").catch(() => {})
  },
})

export const test = withOptions.extend<Fixtures>({
  backend: async ({ fakeModel, model, projectBundles, mcpServers }, use) => {
    requireHya()
    const root = await mkdtemp(join(tmpdir(), "hya-tui-web-"))
    const { child, backend } = await startBackend(root, setupOf(fakeModel, model, projectBundles, mcpServers))
    await use(backend)
    if (child.exitCode === null) {
      const exited = new Promise((resolve) => child.once("exit", resolve))
      child.kill("SIGTERM")
      const timer = setTimeout(() => child.kill("SIGKILL"), 3_000)
      await exited
      clearTimeout(timer)
    }
    await rm(root, { recursive: true, force: true })
  },
  // Capture the final screen here, while `backend` is still running. The base
  // fixture tears down after the backend, so its capture would show the TUI's
  // reconnect error instead of the state under test.
  //
  // The TUI inherits the runner's environment, so its preferences file
  // (theme) would be the developer's own `~/.config/hya/tui.json`; point it
  // at the backend's isolated config directory unless the spec sets one.
  tui: async ({ tui, backend }, use, testInfo) => {
    let last: Tui | undefined
    const isolated = { HYA_TUI_CONFIG: join(dirname(backend.dir), "config", "hya", "tui.json") }
    await use(async (command, options = {}) => {
      last = await tui(command, { ...options, env: { ...isolated, ...options.env } })
      // Plain v1 TUI starts create their initial session asynchronously.
      // Wait for admission before tests immediately issue commands or API reads.
      if (command.includes("--server") && !command.includes("--continue") && !command.includes("--remote") && !command.includes("--session")) await waitForSession(backend)
      return last
    })
    if (last) await last.attach(testInfo, "final-screen").catch(() => {})
  },
})

export { expect } from "./harness"
export { hangStep, httpErrorStep, reasoningStep, startFakeModel, textStep, toolStep, toolsStep, type FakeModel, type Protocol, type Step } from "./fake-model"

/** The isolated backend's hya config directory (`$XDG_CONFIG_HOME/hya`: config.yaml, auth/). */
export function backendConfigDir(backend: Backend): string {
  return join(dirname(backend.dir), "config", "hya")
}

/** Wait until a plain TUI start has created its initial session. */
export async function waitForSession(backend: Backend, timeout = 15_000): Promise<void> {
  await expect.poll(async () => {
    const result = await api<{ sessions?: unknown[] }>(backend, "GET", "/v1/sessions")
    return result.sessions?.length ?? 0
  }, { timeout }).toBeGreaterThan(0)
}

/**
 * A headless run next to the TUI: create a session on the fake model over
 * the HTTP API and admit one prompt turn in it (not awaited to its end).
 * Returns the new session's id.
 */
export async function headlessTurn(backend: Backend, text: string): Promise<string> {
  const { session } = await api<{ session: { id: string } }>(backend, "POST", "/v1/sessions", { agent: "hya-main", model: fakeModelRef, workdir: backend.dir })
  await api(backend, "POST", `/v1/sessions/${session.id}/turns`, { prompt: { text } })
  return session.id
}

/** A browser viewport wide enough for the right sidebar (150 columns minimum). */
export const wideViewport = { width: 1500, height: 640 }

/** Explicit /status facts are drawn in the passive conversation viewer, without a frame. */
async function statusRows(term: Tui): Promise<string[] | undefined> {
  const lines = await term.lines()
  const at = lines.findIndex((line) => /Version {5}\d+\./.test(line))
  if (at < 0) return undefined
  const left = lines[at]!.indexOf("Version")
  return lines.slice(Math.max(0, at - 1)).map((line) => line.slice(left).replace(/│.*$/, "").trimEnd())
}

/** Open the explicit metadata view (`/status`); startup can finish after the first frame. */
export async function showStatusView(term: Tui): Promise<void> {
  await expect.poll(async () => {
    // The persistent Context pane also shows Version, but uses two spaces.
    // Wait for the explicit /status view's aligned field before reading it.
    if (!/Version {5}\d+\./.test(await term.text())) {
      await term.type("/status")
      await term.press("Enter")
    }
    return /Version {5}\d+\./.test(await term.text())
  }, { timeout: 30_000 }).toBe(true)
}

/**
 * Read one field from /status; a value wrapped onto following rows (a long
 * Directory) is joined back. `""` when the field is not on screen (a frame
 * read mid-redraw): both callers poll.
 */
async function statusField(term: Tui, field: string): Promise<string> {
  await showStatusView(term)
  const rows = (await statusRows(term)) ?? []
  const at = rows.findIndex((row) => new RegExp(`^${field}\\s{2,}`).test(row))
  if (at < 0) return ""
  let value = rows[at]!.replace(new RegExp(`^${field}\\s{2,}`), "").trim()
  for (const next of rows.slice(at + 1)) {
    if (!next.trim() || /^[A-Z][A-Za-z]*\s{2,}/.test(next)) break
    value += next.trim()
  }
  return value
}

/** Return to the transcript without changing the split tree or message draft. */
async function showConversation(term: Tui): Promise<void> {
  await term.type("/layout show")
  await term.press("Enter")
  await expect.poll(() => term.find("Version     ")).toBeNull()
}

/** Read the untitled selected session, then return to Conversation. */
export async function statusSessionId(term: Tui, timeout = 30_000): Promise<string> {
  let id = ""
  await expect.poll(async () => {
    const value = await statusField(term, "Session")
    id = /^hysec_\w+$/.test(value) ? value : ""
    if (!id) await showConversation(term)
    return id
  }, { timeout }).not.toBe("")
  await showConversation(term)
  return id
}

/** Wait for a named field, then leave the transcript ready for subsequent interaction. */
export async function expectStatus(term: Tui, field: string, expected: string | RegExp): Promise<void> {
  await expect.poll(async () => {
    const value = await statusField(term, field)
    const matches = typeof expected === "string" ? value === expected : expected.test(value)
    if (!matches) await showConversation(term)
    return matches
  }, { timeout: 30_000 }).toBe(true)
  await showConversation(term)
}

/** /new completes asynchronously; verify the selected session actually changed. */
export async function createSession(term: Tui): Promise<string> {
  const previous = await statusSessionId(term)
  await term.type("/new")
  await term.press("Enter")
  let next = previous
  await expect.poll(async () => {
    next = await statusSessionId(term)
    return next
  }, { timeout: 30_000 }).not.toBe(previous)
  return next
}

const escapeRegExp = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")

/**
 * A tool card (docs/tui.md "Tool calls"): the borderless block headed `tool`, its state row
 * (`icon`, then `summary` such as `awaiting approval`), and the next row showing either the
 * compact JSON arguments containing `args` or the card's display of the first argument value
 * (`"command":"echo hi"` → `echo hi`).
 */
export function toolCardBlock(icon: "✓" | "✗" | "◌" | "○", tool: string, args: string, summary?: string): RegExp {
  const state = `${escapeRegExp(icon)}${summary ? `\\s+${escapeRegExp(summary)}` : ""}`
  let display = args
  try {
    const parsed = JSON.parse(`{${args}}`) as Record<string, unknown>
    const first = Object.values(parsed)[0]
    if (typeof first === "string") display = first
    else if (first !== undefined) display = String(first)
  } catch { /* compact argument fragment is not JSON on its own */ }
  return new RegExp(`[^\\n]*${escapeRegExp(tool)}[^\\n]*\\n[^\\n]*${state}[^\\n]*\\n[^\\n]*(?:\\{[^\\n]*${escapeRegExp(args)}|${escapeRegExp(display)})`)
}

/**
 * One pid per running TUI among `pids` (matching TUI processes): a TUI is a
 * supervisor plus the app it runs with the same command line (docs/tui.md
 * "Hot update after `hya serve restart`"), so only the processes whose parent
 * is not one of `pids` count.
 */
export function tuiInstances(pids: readonly number[]): number[] {
  if (!pids.length) return []
  const parents = new Map(execFileSync("ps", ["-axo", "pid=,ppid="], { maxBuffer: 64 * 1024 * 1024 }).toString().trim().split("\n")
    .map((line) => line.trim().split(/\s+/).map(Number) as [number, number]))
  return pids.filter((pid) => !pids.includes(parents.get(pid) ?? -1))
}

/**
 * `tui()` arguments that run packages/hya-tui with no `--server`, so it
 * starts its own `hya serve` (`HYA_BIN` = the binary under test) in
 * `workspace.dir` with the workspace's isolated environment: its default
 * database lands under the workspace's `XDG_STATE_HOME`, so `--continue`
 * sees the sessions of earlier launches in the same test.
 */
export function selfLaunch(workspace: Workspace, extra: string[] = [], options: LaunchOptions = {}): [string[], LaunchOptions] {
  return [
    ["bun", tuiMain, "--dir", workspace.dir, ...extra],
    { ...options, env: { ...workspace.env, ...options.env } },
  ]
}

/** The workspace's default database (`$XDG_STATE_HOME/hya/sessions.db`): the one its TUIs' daemon serves. */
export function workspaceDb(workspace: Workspace): string {
  return join(workspace.env.XDG_STATE_HOME!, "hya", "sessions.db")
}

/** `hya serve <args> --db <workspace db>` with the workspace environment; stdout, stderr, and exit code. */
export function daemon(workspace: Workspace, args: string[]): Promise<{ stdout: string; stderr: string; code: number | null }> {
  const { HYA_MODEL: _model, ...inherited } = process.env
  return new Promise((resolve, reject) => {
    const child = spawn(hyaBin, ["serve", ...args, "--db", workspaceDb(workspace)], { cwd: workspace.dir, env: { ...inherited, ...workspace.env }, stdio: ["ignore", "pipe", "pipe"] })
    let stdout = ""
    let stderr = ""
    child.stdout!.on("data", (chunk: Buffer) => (stdout += chunk.toString()))
    child.stderr!.on("data", (chunk: Buffer) => (stderr += chunk.toString()))
    child.once("error", reject)
    child.once("exit", (code) => resolve({ stdout, stderr, code }))
  })
}

/** The workspace daemon's `hya serve status --json`, or `undefined` when none runs. */
export async function daemonStatus(workspace: Workspace): Promise<{ url: string; pid: number; version: string; startedAt: number } | undefined> {
  const result = await daemon(workspace, ["status", "--json"])
  return result.code === 0 ? JSON.parse(result.stdout.trim()) : undefined
}

/** Init a git repo with one commit in `dir` (E22 status bar git branch). */
export async function initGitRepo(dir: string, branch = "main"): Promise<void> {
  const run = (args: string[]): Promise<void> =>
    new Promise((resolve, reject) => {
      const child = spawn("git", args, { cwd: dir, stdio: "ignore" })
      child.once("exit", (code) => (code === 0 ? resolve() : reject(new Error(`git ${args.join(" ")} exited ${code}`))))
      child.once("error", reject)
    })
  await run(["init", "-q", "-b", branch])
  await run(["-c", "user.email=e2e@hya.test", "-c", "user.name=e2e", "commit", "-q", "--allow-empty", "-m", "init"])
}
