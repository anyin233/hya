// hya-specific fixtures: an isolated `hya serve` on the offline echo model
// (or, when a spec opts in, a scripted fake OpenAI model on the Chat
// Completions or Responses protocol) and the
// argv that runs packages/hya-tui against it. The host itself stays generic;
// only these specs know about hya.

import { spawn, type ChildProcess } from "node:child_process"
import { existsSync } from "node:fs"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { startFakeModel, type FakeModel, type Protocol, type Step } from "./fake-model"
import { test as base, type Tui } from "./harness"

const repoRoot = fileURLToPath(new URL("../../..", import.meta.url))

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

async function startBackend(root: string, fakeModel: FakeModel | undefined, protocol: Protocol, permission: PermissionModel): Promise<{ child: ChildProcess; backend: Backend }> {
  const dir = join(root, "work")
  const env: Record<string, string> = {}
  for (const name of ["home", "config", "data", "state", "cache"]) {
    env[name] = join(root, name)
    await mkdir(env[name]!, { recursive: true })
  }
  await mkdir(dir, { recursive: true })
  if (fakeModel) {
    const hyaCfgDir = join(env.config!, "hya")
    await mkdir(join(hyaCfgDir, "auth"), { recursive: true })
    await writeFile(
      join(hyaCfgDir, "config.yaml"),
      `default_model: ${fakeModelRef}\n` +
        "providers:\n" +
        "  fake:\n" +
        `    kind: ${providerKinds[protocol]}\n` +
        `    base_url: ${fakeModel.baseUrl}\n` +
        "    api_key: e2e-test-key\n" +
        "    models:\n" +
        "      - id: model\n" +
        "mcp: {}\n" +
        "plugins: {}\n" +
        "permission:\n" +
        `  model: ${permission}\n` +
        "  rules: []\n",
    )
    await writeFile(join(hyaCfgDir, "auth", "fake.yaml"), "token: e2e-test-key\n")
  }
  const { HYA_MODEL: _model, ...inherited } = process.env
  const child = spawn(hyaBin, ["serve", "--bind", "127.0.0.1:0", "--db", join(root, "hya.db")], {
    cwd: dir,
    env: {
      ...inherited,
      HOME: env.home!,
      XDG_CONFIG_HOME: env.config!,
      XDG_DATA_HOME: env.data!,
      XDG_STATE_HOME: env.state!,
      XDG_CACHE_HOME: env.cache!,
    },
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
}

export const test = base.extend<Fixtures & Options>({
  model: [undefined, { option: true }],
  fakeModel: async ({ model }, use) => {
    if (!model) {
      await use(undefined)
      return
    }
    const fake = await startFakeModel(model.steps)
    await use(fake)
    await fake.stop()
  },
  backend: async ({ fakeModel, model }, use) => {
    if (!existsSync(hyaBin)) {
      throw new Error(`hya binary not found at ${hyaBin}; run \`cargo build -p hya-backend --bin hya\` or set HYA_BIN`)
    }
    const root = await mkdtemp(join(tmpdir(), "hya-tui-web-"))
    const { child, backend } = await startBackend(root, fakeModel, model?.protocol ?? "chat", model?.permission ?? "default")
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
  return ["bun", join(repoRoot, "packages/hya-tui/src/main.ts"), "--server", backend.url, "--dir", backend.dir]
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
