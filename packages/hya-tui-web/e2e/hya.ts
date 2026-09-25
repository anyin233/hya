// hya-specific fixtures: an isolated `hya serve` on the offline echo model
// (or, when a spec opts in, a scripted fake OpenAI model on the Chat
// Completions or Responses protocol) and the
// argv that runs packages/hya-tui against it. The host itself stays generic;
// only these specs know about hya.

import { spawn, type ChildProcess } from "node:child_process"
import { existsSync } from "node:fs"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { startFakeModel, type FakeModel, type Protocol, type Step } from "./fake-model"
import { test as base, type LaunchOptions, type Tui } from "./harness"

const repoRoot = fileURLToPath(new URL("../../..", import.meta.url))

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

/** Write project bundles into `<dir>/.hya/bundles/<name>/` (the backend runs in `dir`, so it loads them at startup). */
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
type BackendSetup = {
  fakeModel: FakeModel | undefined
  protocol: Protocol
  permission: PermissionModel
  bundles: Record<string, BundleFiles> | undefined
  modelIds?: string[]
  contextLimit?: number
}

/**
 * Create the isolated HOME/XDG directories, the workspace `dir`, project
 * bundles, and (with a fake model) the backend config under `root`; returns
 * the environment a `hya` process needs to use them.
 */
async function prepareBackend(root: string, setup: BackendSetup): Promise<{ dir: string; env: Record<string, string> }> {
  const { fakeModel, protocol, permission, bundles, modelIds = ["model"], contextLimit } = setup
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
    const modelsYaml = modelIds.map((id) => `      - id: ${id}\n${limit}`).join("")
    await writeFile(
      join(hyaCfgDir, "config.yaml"),
      `default_model: ${fakeModelRef}\n` +
        "providers:\n" +
        "  fake:\n" +
        `    kind: ${providerKinds[protocol]}\n` +
        `    base_url: ${fakeModel.baseUrl}\n` +
        "    api_key: e2e-test-key\n" +
        "    models:\n" +
        modelsYaml +
        "mcp: {}\n" +
        "plugins: {}\n" +
        "permission:\n" +
        `  model: ${permission}\n` +
        "  rules: []\n",
    )
    await writeFile(join(hyaCfgDir, "auth", "fake.yaml"), "token: e2e-test-key\n")
  }
  return {
    dir,
    env: { HOME: env.home!, XDG_CONFIG_HOME: env.config!, XDG_DATA_HOME: env.data!, XDG_STATE_HOME: env.state!, XDG_CACHE_HOME: env.cache! },
  }
}

async function startBackend(root: string, setup: BackendSetup): Promise<{ child: ChildProcess; backend: Backend }> {
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
}

const setupOf = (fakeModel: FakeModel | undefined, model: FakeModelOption | undefined, projectBundles: Record<string, BundleFiles> | undefined): BackendSetup => ({
  fakeModel,
  protocol: model?.protocol ?? "chat",
  permission: model?.permission ?? "default",
  bundles: projectBundles,
  ...(model?.models ? { modelIds: model.models } : {}),
  ...(model?.contextLimit ? { contextLimit: model.contextLimit } : {}),
})

function requireHya(): void {
  if (!existsSync(hyaBin)) {
    throw new Error(`hya binary not found at ${hyaBin}; run \`cargo build -p hya-backend --bin hya\` or set HYA_BIN`)
  }
}

const withOptions = base.extend<{ fakeModel: FakeModel | undefined } & Options>({
  model: [undefined, { option: true }],
  projectBundles: [undefined, { option: true }],
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
    await use({ root, dir, env: { ...env, HYA_BIN: hyaBin, ...(fakeModel ? { HYA_MODEL: fakeModelRef } : {}) } })
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
  backend: async ({ fakeModel, model, projectBundles }, use) => {
    requireHya()
    const root = await mkdtemp(join(tmpdir(), "hya-tui-web-"))
    const { child, backend } = await startBackend(root, setupOf(fakeModel, model, projectBundles))
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
  tui: async ({ tui, backend: _backend }, use, testInfo) => {
    let last: Tui | undefined
    await use(async (command, options) => (last = await tui(command, options)))
    if (last) await last.attach(testInfo, "final-screen").catch(() => {})
  },
})

export { expect } from "./harness"
export { hangStep, httpErrorStep, reasoningStep, textStep, toolStep, toolsStep, type FakeModel, type Protocol, type Step } from "./fake-model"

/** argv that runs packages/hya-tui against `backend`. */
export function hyaTui(backend: Backend): string[] {
  return ["bun", tuiMain, "--server", backend.url, "--dir", backend.dir]
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
