import { afterAll, describe, expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { HttpError } from "../src/client"
import { locateSdk, materialize, parseCatalogEntry, sdkCompatible, type CatalogExtension } from "../src/extensions/install"
import { ExtensionManager, type ManagerOptions } from "../src/extensions/manager"
import type { ChildProcess, Spawn } from "../src/extensions/hostChannel"
import { confineScript, planSandbox, SandboxUnavailableError } from "../src/extensions/sandbox"
import { scopeContext, validateContributions, validateRenderNode } from "../src/extensions/wire"
import { parsePaneLayout, setPaneKind, splitPane, defaultPaneLayout } from "../src/state/panes"

const temp = mkdtempSync(join(tmpdir(), "hya-tui-ext-"))
afterAll(() => rmSync(temp, { recursive: true, force: true }))

const sha = (text: string) => createHash("sha256").update(text).digest("hex")
let digestSeed = 0
/** A catalog row as `GET /v1/tui-extensions` returns it. */
function catalogRow(id: string, files: Record<string, string>, permissions: string[], entry = "tui/main.ts") {
  return {
    bundleId: id, bundleVersion: "1.0.0", preparedDigest: sha(`${id}:${++digestSeed}`), apiVersion: 1, entry, sdk: "1.0.0", permissions,
    files: Object.entries(files).map(([path, content]) => ({ path, sha256: sha(content), content })),
  }
}

const allPermissions = ["tui.action", "tui.panel", "tui.render", "tui.session.read", "tui.status_item", "tui.transcript.read", "tui.workspace.read", "workspace.git.read"]
const fullExtension = `
import { defineTuiExtension } from "@hya/tui-sdk"
import { label } from "./helper"
export default defineTuiExtension({
  activate(api) {
    let clicks = 0
    api.registerPanel({
      id: "git", title: "Git",
      render: ({ context, width }) => ({ kind: "column", children: [
        { kind: "text", text: label + " " + (context.git?.branch ?? "?") + " w" + width },
        { kind: "text", text: "clicks " + clicks, action: { name: "click" } },
      ] }),
      onAction: () => { clicks += 1; return "clicked" },
    })
    api.registerPanel({ id: "todos", title: "My todos", replaces: "todos", render: () => "custom todos" })
    api.registerStatusItem({ id: "branch", label: "Branch", priority: 3, render: ({ context }) => context.git?.branch })
    api.registerRenderer({ id: "frame", target: "tool_call", mode: "decorate", render: (input) => ({ kind: "box", title: input.tool, children: [{ kind: "slot" }] }) })
    api.registerRenderer({ id: "compact", target: "tool_call", mode: "replace", priority: 1, render: (input) => input.tool === "bash" ? "$ " + input.summary + " " + (input.output ?? "") : null })
    api.registerRenderer({ id: "banner", target: "composer", render: () => ({ kind: "column", children: [{ kind: "text", text: "BANNER" }, { kind: "slot" }] }) })
    api.registerFormatter({ id: "upper", format: ({ text }) => text.toUpperCase() })
    api.registerInterceptor({ id: "guard", target: "submit", intercept: ({ text }) =>
      text.includes("secret") ? { decision: "block", message: "no secrets" } : text.startsWith("!!") ? { decision: "replace", text: text.slice(2) } : undefined })
  },
})
`

// Real child processes answer asynchronously and the manager exposes state, not
// completion promises, for its fire-and-forget renders: poll that state.
async function until<T>(read: () => T | undefined | Promise<T | undefined>, what: string, timeoutMs = 5_000): Promise<T> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = await read()
    if (value !== undefined && value !== false) return value
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await Bun.sleep(20)
  }
}

function manager(options: Partial<ManagerOptions> = {}): ExtensionManager {
  return new ExtensionManager({ cacheRoot: join(temp, "cache"), sdk: locateSdk()!, sandbox: "disabled", ...options })
}
const local = { enabled: () => undefined, remote: false, trusted: () => undefined }
const context = { terminal: { columns: 120, rows: 40 }, git: { branch: "main", dirty: 1, ahead: 0, behind: 0 }, session: { id: "hysec_1", busy: false } }

describe("wire validation", () => {
  test("render trees: slots only where decorating, inert actions without tui.action, escapes stripped", () => {
    const tree = { kind: "column", children: [{ kind: "text", text: "a\u001b[31mb\u0000c\u001b]0;title\u0007", action: { name: "go" } }, { kind: "slot" }] }
    expect(validateRenderNode(tree, { slot: "forbid", actions: true })).toBe("slot is only allowed in decorating renderers")
    expect(validateRenderNode(tree, { slot: "require", actions: false })).toEqual({ kind: "column", children: [{ kind: "text", text: "abc" }, { kind: "slot" }] })
    expect(validateRenderNode({ kind: "text", text: "x", action: { name: "go" } }, { slot: "forbid", actions: true })).toEqual({ kind: "text", text: "x", action: { name: "go", data: null } })
    expect(validateRenderNode({ kind: "column", children: [] }, { slot: "require", actions: false })).toBe("a decorating render tree needs exactly one slot")
    expect(validateRenderNode({ kind: "script", code: "x" }, { slot: "forbid", actions: false })).toContain("unknown render node kind")
    expect(validateRenderNode({ kind: "text", text: "x", onClick: "y" }, { slot: "forbid", actions: false })).toBe("text: unknown field")
    expect(validateRenderNode({ kind: "text", text: "x", style: { color: "red; rm -rf" } }, { slot: "forbid", actions: false })).toContain("#rrggbb")
    let deep: unknown = { kind: "text", text: "x" }
    for (let depth = 0; depth < 40; depth++) deep = { kind: "column", children: [deep] }
    expect(validateRenderNode(deep, { slot: "forbid", actions: false })).toContain("deeper than")
  })

  test("contributions without their permission are dropped with a warning; malformed ones fail activation", () => {
    const contributions = { panels: [{ id: "p", title: "P", placement: "sidebar" }], renderers: [{ id: "r", target: "tool_call", mode: "replace", priority: 0 }], interceptors: [{ id: "i", target: "submit", priority: 0 }] }
    const checked = validateContributions(contributions, ["tui.panel"])
    if (typeof checked === "string") throw new Error(checked)
    expect(checked.contributions.panels.map((panel) => panel.id)).toEqual(["p"])
    expect(checked.contributions.renderers).toEqual([])
    expect(checked.contributions.interceptors).toEqual([])
    expect(checked.warnings).toEqual(["renderers ignored: permission tui.render not declared", "interceptors ignored: permission tui.action not declared"])
    expect(validateContributions({ renderers: [{ id: "c", target: "composer", mode: "replace" }] }, ["tui.render"])).toContain("only be decorated")
    expect(validateContributions({ panels: [{ id: "p", title: "P", placement: "sidebar", replaces: "conversation" }] }, ["tui.panel"])).toContain("cannot replace conversation")
  })

  test("context sections follow permissions", () => {
    const full = { ...context, transcript: [{ role: "user", text: "hi" }], workspace: { directory: "/w" }, sessions: { ready: true, items: [] }, projects: { ready: true, items: [] }, todos: [], status: { ready: true, mode: { text: "manual", tone: "strong" }, server: "local", connection: "connected", versions: { tui: "1" }, items: [] } } as const
    expect(scopeContext(full, [])).toEqual({ terminal: context.terminal })
    expect(Object.keys(scopeContext(full, ["workspace.git.read", "tui.transcript.read"])).sort()).toEqual(["git", "terminal", "transcript"])
    expect(Object.keys(scopeContext(full, ["tui.sessions.read", "tui.projects.read", "tui.todos.read", "tui.status.read"])).sort()).toEqual(["projects", "sessions", "status", "terminal", "todos"])
  })
})

describe("catalog files", () => {
  test("rejects tampered content, escaping paths, and a missing entry", () => {
    const row = catalogRow("acme/x", { "tui/main.ts": "export default 1" }, [])
    expect(typeof parseCatalogEntry(row)).toBe("object")
    expect(parseCatalogEntry({ ...row, files: [{ ...row.files[0], content: "evil" }] })).toContain("does not match its sha256")
    expect(parseCatalogEntry({ ...row, files: [{ path: "../x.ts", sha256: sha("x"), content: "x" }], entry: "../x.ts" })).toContain("invalid entry")
    expect(parseCatalogEntry({ ...row, entry: "tui/other.ts" })).toContain("is not among its files")
    expect(parseCatalogEntry({ ...row, permissions: ["fs.write"] })).toContain("unknown permission")
    expect(parseCatalogEntry({ ...row, apiVersion: 2 })).toContain("api_version 2 is not supported")
  })

  test("materializes read-only files under the digest, once", async () => {
    const parsed = parseCatalogEntry(catalogRow("acme/files", { "tui/main.ts": "export default 1", "tui/lib/util.ts": "export const u = 1" }, [])) as CatalogExtension
    const dir = await materialize(parsed, join(temp, "files"))
    expect(dir).toEndWith(parsed.preparedDigest)
    expect(readFileSync(join(dir, "tui/lib/util.ts"), "utf8")).toBe("export const u = 1")
    expect(statSync(join(dir, "tui/main.ts")).mode & 0o222).toBe(0)
    expect(await materialize(parsed, join(temp, "files"))).toBe(dir)
  })

  test("SDK compatibility: same major, installed minor not older", () => {
    expect(sdkCompatible("1.0.0", "1.0.0")).toBe(true)
    expect(sdkCompatible("1.0", "1.2.0")).toBe(true)
    expect(sdkCompatible("1.3.0", "1.2.0")).toBe(false)
    expect(sdkCompatible("2.0.0", "2.0.0")).toBe(false)
  })

  test("the SDK is found next to the TUI in both the repository and the release layout", () => {
    expect(locateSdk()?.dir).toEndWith("hya-tui-sdk")
    expect(locateSdk(join(temp, "nowhere/tui/src/extensions"))).toBeUndefined()
  })
})

// Whether this host can confine a process (macOS; Linux with Landlock), found the way the manager finds it.
const osUsable = (await planSandbox({ argv: [process.execPath], policy: "best-effort", readable: [locateSdk()!.dir] })).isolated

describe("OS sandbox plans", () => {
  test("disabled passes argv through; an unsupported platform warns (best-effort) or refuses (required)", async () => {
    const argv = ["/bun", "main.ts"]
    expect(await planSandbox({ argv, policy: "disabled", readable: [], platform: "linux" })).toMatchObject({ argv, isolated: false })
    expect(await planSandbox({ argv, policy: "best-effort", readable: [], platform: "win32" })).toMatchObject({ argv, isolated: false })
    await expect(planSandbox({ argv, policy: "required", readable: [], platform: "win32" })).rejects.toThrow(SandboxUnavailableError)
  })

  // Probe results are cached per launcher command: each case uses its own runtime path.
  test("linux and macOS start the extension through the self-confining launcher, probed once with bun --version", async () => {
    for (const platform of ["linux", "darwin"] as const) {
      const probes: string[][] = []
      const bun = `/bun-${platform}`
      const probe = async (argv: string[]) => { probes.push(argv); return undefined }
      const plan = await planSandbox({ argv: [bun, "main.ts", "entry.ts"], policy: "required", readable: ["/opt/sdk", "/opt/ext"], platform, probe })
      expect(plan).toEqual({ argv: [bun, confineScript, "/opt/sdk", "/opt/ext", "--", bun, "main.ts", "entry.ts"], isolated: true })
      await planSandbox({ argv: [bun, "main.ts", "other.ts"], policy: "required", readable: ["/opt/sdk", "/opt/ext"], platform, probe })
      expect(probes).toEqual([[bun, confineScript, "/opt/sdk", "/opt/ext", "--", bun, "--version"]])
    }
  })

  test("a launcher that cannot confine here: best-effort warns with its reason, required refuses", async () => {
    const request = { argv: ["/bun-nolandlock", "x"], readable: [], platform: "linux" as const, probe: async () => "Landlock is not available in this kernel (errno 38)" }
    const plan = await planSandbox({ ...request, policy: "best-effort" })
    expect(plan).toMatchObject({ argv: ["/bun-nolandlock", "x"], isolated: false })
    expect(plan.warning).toContain("Landlock is not available")
    expect(plan.warning).toContain("the extension VM still applies")
    await expect(planSandbox({ ...request, policy: "required" })).rejects.toThrow("Landlock is not available")
  })
})

describe("extension panes", () => {
  test("an extension pane names its panel; saved layouts validate it", () => {
    const layout = splitPane(defaultPaneLayout(), "vertical", "extension", "acme/git#git")
    expect(parsePaneLayout(JSON.parse(JSON.stringify(layout)))).toEqual(layout)
    expect(() => splitPane(defaultPaneLayout(), "vertical", "extension")).toThrow("needs a panel")
    const assigned = setPaneKind(layout, "jobs")
    expect(JSON.stringify(assigned)).not.toContain("acme/git#git")
    expect(parsePaneLayout({ ...layout, root: { type: "pane", id: "pane-1", kind: "conversation", panel: "x#y" }, active: "pane-1" })).toBeUndefined()
  })
})

describe("extension processes (real SDK, real bun)", () => {
  test("contributions reach every surface: panels, status, tool cards, composer, formatter, interceptor, actions", async () => {
    const notices: string[] = []
    const extensions = manager({ notice: (text) => notices.push(text) })
    extensions.setContext(context)
    const rejected = await extensions.load([catalogRow("acme/full", { "tui/main.ts": fullExtension, "tui/helper.ts": 'export const label = "branch:"' }, allPermissions)], local)
    expect(rejected).toEqual([])
    const [info] = extensions.list()
    expect(info).toMatchObject({ id: "acme/full", state: "running" })
    expect(extensions.panels().map((panel) => panel.key)).toEqual(["acme/full#git", "acme/full#todos"])
    expect(extensions.replacement("todos")?.key).toBe("acme/full#todos")

    const panel = await until(() => extensions.panelView("acme/full#git", 40, 10).node, "panel tree")
    expect(panel).toEqual({ kind: "column", children: [{ kind: "text", text: "branch: main w40" }, { kind: "text", text: "clicks 0", action: { name: "click", data: null } }] })
    expect(await until(() => extensions.statusFields()[0], "status field")).toEqual({ label: "Branch", value: "main", priority: 3 })

    const bash = { id: "t1", tool: "bash", status: "completed" as const, summary: "ls", output: "a.txt" }
    const trees = await until(() => { const value = extensions.toolCall(bash); return value.replace && value.decorators.length ? value : undefined }, "tool trees")
    expect(trees.replace?.node).toEqual({ kind: "text", text: "$ ls a.txt" })
    expect(trees.decorators.map((tree) => tree.node)).toEqual([{ kind: "box", title: "bash", children: [{ kind: "slot" }] }])
    const read = { id: "t2", tool: "read", status: "completed" as const, summary: "x" }
    const readTrees = await until(() => { const value = extensions.toolCall(read); return value.decorators.length ? value : undefined }, "read decorators")
    expect(readTrees.replace).toBeUndefined()
    expect((await until(() => extensions.composer(80).decorators[0], "composer decorator")).node).toEqual({ kind: "column", children: [{ kind: "text", text: "BANNER" }, { kind: "slot" }] })

    expect(extensions.format({ out: 1 }, "plain")).toBe("plain")
    await until(() => extensions.format({ out: 1 }, "plain") === "PLAIN", "formatted text")

    expect(await extensions.interceptSubmit("hello")).toEqual({ text: "hello" })
    expect(await extensions.interceptSubmit("!!rewritten")).toEqual({ text: "rewritten" })
    expect(await extensions.interceptSubmit("my secret")).toEqual({ blocked: "acme/full: no secrets" })

    await extensions.action("acme/full", { kind: "panel", id: "git" }, { name: "click", data: null })
    expect(notices).toContain("clicked")
    await until(() => JSON.stringify(extensions.panelView("acme/full#git", 40, 10).node).includes("clicks 1"), "re-render after action")

    extensions.setContext({ ...context, git: { ...context.git, branch: "feature" } })
    await until(() => extensions.statusFields()[0]?.value === "feature", "status after context change")
    await extensions.stopAll()
    expect(extensions.list()).toEqual([])
  }, 20_000)

  test("permissions bound the context and contributions an extension gets", async () => {
    const source = `
import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) {
  api.registerPanel({ id: "seen", title: "Seen", render: ({ context }) => Object.keys(context).sort().join(",") })
  api.registerFormatter({ id: "f", format: () => "never" })
} })`
    const extensions = manager()
    extensions.setContext(context)
    await extensions.load([catalogRow("acme/narrow", { "tui/main.ts": source }, ["tui.panel"])], local)
    expect(await until(() => extensions.panelView("acme/narrow#seen", 20, 5).node, "panel")).toEqual({ kind: "text", text: "terminal" })
    expect(extensions.list()[0]?.warnings).toContain("formatters ignored: permission tui.render not declared")
    await extensions.stopAll()
  }, 20_000)

  test("a crashed host restarts every extension within its budget, then they stay failed; the TUI keeps working", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => "alive" }) } })`
    // An extension cannot end the host (the VM has no process API): kill it as a crash would.
    const children: { kill(signal?: number | NodeJS.Signals): void }[] = []
    const spawn: Spawn = (argv) => {
      const child = Bun.spawn(argv, { env: {}, stdin: "pipe", stdout: "pipe", stderr: "pipe" })
      children.push(child)
      return child as unknown as ChildProcess
    }
    const extensions = manager({ restartBudget: 2, spawn })
    await extensions.load([catalogRow("acme/crash", { "tui/main.ts": source }, ["tui.panel"]), catalogRow("acme/crash2", { "tui/main.ts": source }, ["tui.panel"])], local)
    expect(extensions.list().map((info) => info.state)).toEqual(["running", "running"])
    expect(children).toHaveLength(1)
    children.at(-1)!.kill("SIGKILL")
    await until(() => extensions.list().every((info) => info.state === "failed"), "both failed")
    expect(extensions.list()[0]?.reason).toContain("exited")
    await until(() => extensions.list().every((info) => info.state === "running"), "both restarted", 5_000)
    expect(children).toHaveLength(2)
    expect(await until(() => extensions.panelView("acme/crash2#p", 10, 2).node, "panel after restart")).toEqual({ kind: "text", text: "alive" })
    children.at(-1)!.kill("SIGKILL")
    await until(() => extensions.list()[0]?.reason?.includes("restart budget exhausted"), "budget exhausted", 8_000)
    expect(extensions.panelView("acme/crash#p", 10, 2).error).toContain("not available")
    await extensions.stopAll()
  }, 20_000)

  test("a render past the VM deadline fails that extension (restarts within budget); the others in the host keep answering", async () => {
    const hang = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "slow", title: "Slow", render: () => { for (;;) {} } }) } })`
    const fine = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => "fine" }) } })`
    const extensions = manager({ restartBudget: 1 })
    await extensions.load([catalogRow("acme/hang", { "tui/main.ts": hang }, ["tui.panel"]), catalogRow("acme/fine", { "tui/main.ts": fine }, ["tui.panel"])], local)
    extensions.panelView("acme/hang#slow", 10, 2)
    extensions.panelView("acme/fine#p", 10, 2)
    // The SDK's VM deadline is a fixed 2 s of real execution.
    await until(() => extensions.list()[1]?.reason?.includes("exceeded its 2000 ms limit"), "deadline", 10_000)
    expect(extensions.list()[1]?.state).toBe("failed")
    expect(await until(() => extensions.panelView("acme/fine#p", 10, 2).node, "the other extension")).toEqual({ kind: "text", text: "fine" })
    await extensions.stopAll()
  }, 20_000)

  test("a promise that can never settle fails only that render", async () => {
    const pending = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => new Promise(() => {}) }) } })`
    const extensions = manager()
    await extensions.load([catalogRow("acme/pending", { "tui/main.ts": pending }, ["tui.panel"])], local)
    extensions.panelView("acme/pending#p", 10, 2)
    await until(() => extensions.list()[0]?.warnings.some((warning) => warning.includes("never settles")), "render error")
    expect(extensions.list()[0]?.state).toBe("running")
    await extensions.stopAll()
  }, 20_000)

  test("extensions run in the SDK's VM: runtime modules cannot be imported, and the reason is shown", async () => {
    const escape = `import { readFileSync } from "node:fs"\nimport { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => String(readFileSync("/etc/passwd")) }) } })`
    const extensions = manager()
    await extensions.load([catalogRow("acme/escape", { "tui/main.ts": escape }, ["tui.panel"])], local)
    expect(extensions.list()[0]).toMatchObject({ state: "failed" })
    expect(extensions.list()[0]?.reason).toContain('cannot import "node:fs"')
    await extensions.stopAll()
  }, 20_000)

  test("remote extensions wait for an explicit enable; disabled ones never start", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate() {} })`
    const extensions = manager()
    await extensions.load([catalogRow("acme/remote", { "tui/main.ts": source }, [])], { enabled: () => undefined, remote: true, trusted: () => undefined })
    expect(extensions.list()[0]).toMatchObject({ state: "blocked" })
    await extensions.setEnabled("acme/remote", true)
    expect(extensions.list()[0]?.state).toBe("running")
    await extensions.load([catalogRow("acme/off", { "tui/main.ts": source }, [])], { enabled: () => false, remote: false, trusted: () => undefined })
    expect(extensions.list().map((info) => [info.id, info.state])).toEqual([["acme/off", "disabled"]])
    await extensions.stopAll()
  }, 20_000)

  test.skipIf(!osUsable)("the OS launcher confines a process: no other files, no writes, no network, no child processes", async () => {
    await using server = Bun.serve({ port: 0, fetch: () => new Response("reachable") })
    const dir = mkdtempSync(join(tmpdir(), "hya-confine-"))
    const secret = join(temp, "secret.txt")
    await Bun.write(secret, "secret")
    await Bun.write(join(dir, "probe.ts"), `
import { readFileSync, writeFileSync } from "node:fs"
const result = {}
const attempt = async (name, fn) => { try { await fn(); result[name] = "allowed" } catch { result[name] = "blocked" } }
await attempt("own", () => readFileSync(${JSON.stringify(join(dir, "probe.ts"))}))
await attempt("secret", () => readFileSync(${JSON.stringify(secret)}))
await attempt("write", () => writeFileSync(${JSON.stringify(join(temp, "written.txt"))}, "x"))
await attempt("tcp", async () => { await fetch(${JSON.stringify(server.url.href)}) })
await attempt("spawn", () => { const child = Bun.spawnSync(["/bin/sh", "-c", "true"]); if (!child.success) throw new Error("failed") })
console.log(JSON.stringify(result))`)
    const run = async (argv: string[]) => JSON.parse(await new Response(Bun.spawn(argv, { stdout: "pipe", stderr: "inherit" }).stdout).text())
    const bun = realpathSync(process.execPath)
    expect(await run([bun, join(dir, "probe.ts")])).toEqual({ own: "allowed", secret: "allowed", write: "allowed", tcp: "allowed", spawn: "allowed" })
    expect(await run([bun, confineScript, dir, "--", bun, join(dir, "probe.ts")])).toEqual({ own: "allowed", secret: "blocked", write: "blocked", tcp: "blocked", spawn: "blocked" })
    rmSync(dir, { recursive: true, force: true })
  }, 20_000)

  test.skipIf(!osUsable)("with the OS sandbox required, extensions run confined, also on a first run whose cache does not exist yet", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"\nexport default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => "confined" }) } })`
    // The host starts (prewarm) before any extension file creates the cache.
    const extensions = manager({ sandbox: "required", cacheRoot: join(temp, "first-run", "cache") })
    await extensions.prewarm()
    await extensions.load([catalogRow("acme/confined", { "tui/main.ts": source }, ["tui.panel"])], local)
    expect(extensions.list()[0]).toMatchObject({ state: "running", isolated: true })
    expect(await until(() => extensions.panelView("acme/confined#p", 10, 1).node, "confined panel")).toEqual({ kind: "text", text: "confined" })
    await extensions.stopAll()
  }, 20_000)

  test("action and key commands validate permissions; ui.release runs without one", async () => {
    const executed: string[] = []
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => "p", onAction: () => ({ commands: [{ command: "nope" }, { command: "project.switch", id: "x" }, { command: "ui.release" }] }), onKey: () => ({ commands: [{ command: "project.switch", id: "x" }, { command: "ui.release" }] }) }) } })`
    const extensions = manager({ executeCommand: async (command) => { executed.push(command.command); return undefined }, release: () => { executed.push("released") } })
    await extensions.load([catalogRow("acme/commands", { "tui/main.ts": source }, ["tui.panel", "tui.action", "tui.keys"])], local)
    await extensions.action("acme/commands", { kind: "panel", id: "p" }, { name: "go", data: null })
    await extensions.key("acme/commands#p", { name: "x", sequence: "x", ctrl: false, shift: false, meta: false })
    expect(executed).toEqual(["released", "released"])
    expect(extensions.list()[0]?.warnings.join("\n")).toContain("unknown command nope")
    expect(extensions.list()[0]?.warnings.join("\n")).toContain("missing permission")
    await extensions.stopAll()
  }, 20_000)

  test("command results round-trip values and HttpError detail through onResult", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) { let result = "none"; api.registerPanel({ id: "p", title: "P", render: () => result, onAction: () => ({ commands: [{ command: "project.switch", id: "x", token: "value" }] }), onResult: (answer) => { result = answer.ok ? "ok:" + JSON.stringify(answer.value) : "error:" + answer.error } }) } })`
    const success = manager({ executeCommand: async () => ({ switched: true }) })
    await success.load([catalogRow("acme/result-ok", { "tui/main.ts": source }, ["tui.panel", "tui.action", "tui.project.control"])], local)
    await success.action("acme/result-ok", { kind: "panel", id: "p" }, { name: "go", data: null })
    expect(await until(() => { const node = success.panelView("acme/result-ok#p", 40, 5).node; return node?.kind === "text" ? node : undefined }, "successful command result")).toEqual({ kind: "text", text: "ok:{\"switched\":true}" })
    await success.stopAll()
    const failure = manager({ executeCommand: async () => { throw new HttpError(409, "POST", "/v1/x", "conflict: busy") } })
    await failure.load([catalogRow("acme/result-error", { "tui/main.ts": source }, ["tui.panel", "tui.action", "tui.project.control"])], local)
    await failure.action("acme/result-error", { kind: "panel", id: "p" }, { name: "go", data: null })
    expect(await until(() => { const node = failure.panelView("acme/result-error#p", 40, 5).node; return node?.kind === "text" ? node : undefined }, "failed command result")).toEqual({ kind: "text", text: "error:conflict: busy" })
    await failure.stopAll()
  }, 20_000)

  test("captured keys are serialized in input order", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) { let seen = ""; api.registerPanel({ id: "p", title: "P", render: () => seen, onKey: (key) => { seen += key.sequence } }) } })`
    const extensions = manager()
    await extensions.load([catalogRow("acme/keys", { "tui/main.ts": source }, ["tui.panel", "tui.keys"])], local)
    const input = Array.from({ length: 20 }, (_, index) => String.fromCharCode(65 + index))
    await Promise.all(input.map((sequence) => extensions.key("acme/keys#p", { name: sequence, sequence, ctrl: false, shift: false, meta: false })))
    expect(await until(() => { const node = extensions.panelView("acme/keys#p", 40, 5).node; return node?.kind === "text" && node.text === input.join("") ? node : undefined }, "ordered keys")).toEqual({ kind: "text", text: input.join("") })
    await extensions.stopAll()
  }, 20_000)

  test("non-first-party replacements take precedence over first-party replacements", async () => {
    const panel = (text: string) => `import { defineTuiExtension } from "@hya/tui-sdk"; export default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", replaces: "sessions", render: () => ${JSON.stringify(text)} }) } })`
    const extensions = manager()
    await extensions.load([catalogRow("zzz/x", { "tui/main.ts": panel("project") }, ["tui.panel"]), { ...catalogRow("hya/basic-tui-components", { "tui/main.ts": panel("first") }, ["tui.panel"]), firstParty: true }], local)
    expect(extensions.replacement("sessions")?.extension).toBe("zzz/x")
    await extensions.stopAll()
  }, 20_000)

  test("remote first-party entries start but third-party entries remain blocked", async () => {
    const source = `import { defineTuiExtension } from "@hya/tui-sdk"; export default defineTuiExtension({ activate() {} })`
    const extensions = manager()
    await extensions.load([{ ...catalogRow("hya/basic-tui-components", { "tui/main.ts": source }, []), firstParty: true }, catalogRow("zzz/x", { "tui/main.ts": source }, [])], { enabled: () => undefined, remote: true, trusted: () => undefined })
    expect(extensions.list().map((entry) => [entry.id, entry.state, entry.jit])).toEqual([["hya/basic-tui-components", "running", false], ["zzz/x", "blocked", false]])
    await extensions.stopAll()
  }, 20_000)

  test("trust tiers: local first-party runs on the JIT, the rest in the VM; a preference overrides; trust moves a running extension", async () => {
    const source = (text: string) => `import { defineTuiExtension } from "@hya/tui-sdk"; export default defineTuiExtension({ activate(api) { api.registerPanel({ id: "p", title: "P", render: () => ${JSON.stringify(text)} }) } })`
    const rows = [
      { ...catalogRow("hya/first", { "tui/main.ts": source("first") }, ["tui.panel"]), firstParty: true },
      { ...catalogRow("hya/declined", { "tui/main.ts": source("declined") }, ["tui.panel"]), firstParty: true },
      catalogRow("acme/third", { "tui/main.ts": source("third") }, ["tui.panel"]),
    ]
    const extensions = manager()
    await extensions.load(rows, { enabled: () => undefined, remote: false, trusted: (id) => id === "hya/declined" ? false : undefined })
    expect(extensions.list().map((entry) => [entry.id, entry.jit])).toEqual([["acme/third", false], ["hya/declined", false], ["hya/first", true]])
    for (const [key, text] of [["hya/first#p", "first"], ["hya/declined#p", "declined"], ["acme/third#p", "third"]] as const) {
      expect(await until(() => extensions.panelView(key, 20, 1).node, key)).toEqual({ kind: "text", text })
    }
    await extensions.setTrusted("acme/third", true)
    expect(extensions.list()[0]).toMatchObject({ id: "acme/third", jit: true, state: "running" })
    expect(await until(() => extensions.panelView("acme/third#p", 20, 1).node, "trusted third")).toEqual({ kind: "text", text: "third" })
    await extensions.stopAll()
  }, 20_000)

  test("the first-party bundle renders sessions and switches projects from the keyboard", async () => {
    const bundleDir = join(import.meta.dir, "../../../bundles/first-party/basic-tui-components")
    const yaml = readFileSync(join(bundleDir, "bundle.yaml"), "utf8")
    const files = [...yaml.matchAll(/path: (tui\/[^ }]+)/g)].map((match) => match[1]!)
    const permissions = [...yaml.slice(yaml.indexOf("\ntui:")).matchAll(/^    - (.+)$/gm)].map((match) => match[1]!)
    const entry = { ...catalogRow("hya/basic-tui-components", Object.fromEntries(files.map((file) => [file, readFileSync(join(bundleDir, file), "utf8")])), permissions), firstParty: true }
    const commands: string[] = []
    const extensions = manager({ executeCommand: async (command) => { commands.push(`${command.command}${"id" in command ? `:${command.id}` : ""}`); return undefined }, release: () => { commands.push("released") } })
    const sessions = { ready: true, items: [{ id: "s1", title: "First", agent: "a", temporary: false, archived: false, busy: false, waiting: false }, { id: "s2", title: "Second", agent: "a", temporary: false, archived: false, busy: false, waiting: false }] }
    extensions.setContext({ ...context, sessions, projects: { ready: true, items: [{ id: "p1", name: "One", roots: [], busy: false }, { id: "p2", name: "Two", roots: [], busy: false }], active: "p1" } })
    await extensions.load([entry], local)
    expect(extensions.list()[0]).toMatchObject({ state: "running", jit: true })
    const rendered = await until(() => extensions.panelView("hya/basic-tui-components#sessions", 60, 20).node, "first-party sessions")
    expect(JSON.stringify(rendered)).toContain("1. First")
    expect(JSON.stringify(rendered)).toContain("2. Second")
    expect(extensions.replacement("projects")?.keys).toBe(true)
    await extensions.key("hya/basic-tui-components#projects", { name: "down", sequence: "", ctrl: false, shift: false, meta: false })
    await extensions.key("hya/basic-tui-components#projects", { name: "return", sequence: "\r", ctrl: false, shift: false, meta: false })
    expect(commands).toEqual(["released", "project.switch:p2"])
    await extensions.stopAll()
  }, 20_000)
})

describe("file access (api.fs through the shared host)", () => {
  /** A Project root with files, a secret outside it, and a symlink inside pointing at the secret. */
  function project() {
    const base = mkdtempSync(join(tmpdir(), "hya-fs-"))
    const root = join(base, "root")
    mkdirSync(root)
    writeFileSync(join(root, "notes.md"), "hello notes")
    mkdirSync(join(root, "sub"))
    writeFileSync(join(base, "secret.txt"), "secret")
    symlinkSync(join(base, "secret.txt"), join(root, "link.txt"))
    return { base, root, secret: join(base, "secret.txt") }
  }
  const reader = (secret: string) => `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) {
  const attempt = async (work) => { try { return await work() } catch (error) { return "error:" + error.message } }
  api.registerPanel({ id: "p", title: "P", render: async () => JSON.stringify({
    read: await attempt(() => api.fs.read("notes.md")),
    up: await attempt(() => api.fs.read("../secret.txt")),
    absolute: await attempt(() => api.fs.read(${JSON.stringify(secret)})),
    link: await attempt(() => api.fs.read("link.txt")),
    list: await attempt(async () => (await api.fs.list(".")).map((entry) => entry.name + ":" + entry.kind).join(",")),
    missing: await attempt(() => api.fs.stat("nope.txt")),
  }) })
} })`
  const panelJson = async (extensions: ExtensionManager, key: string) => JSON.parse(String(await until(() => {
    const node = extensions.panelView(key, 200, 5).node
    return node?.kind === "text" ? node.text : undefined
  }, key))) as Record<string, unknown>

  test("reads stay inside the active Project's roots: .., absolute paths, and symlinks out are refused", async () => {
    const { root, secret } = project()
    const extensions = manager()
    extensions.setRoots([root])
    await extensions.load([catalogRow("acme/reader", { "tui/main.ts": reader(secret) }, ["tui.panel", "fs.read"])], local)
    expect(await panelJson(extensions, "acme/reader#p")).toEqual({
      read: "hello notes",
      up: "error:outside the Project roots",
      absolute: "error:outside the Project roots",
      link: "error:outside the Project roots",
      list: "link.txt:other,notes.md:file,sub:dir",
      missing: null,
    })
    await extensions.stopAll()
  }, 20_000)

  test("without fs.read, or with no Project open, every call is refused", async () => {
    const { root, secret } = project()
    const denied = manager()
    denied.setRoots([root])
    await denied.load([catalogRow("acme/denied", { "tui/main.ts": reader(secret) }, ["tui.panel"])], local)
    expect((await panelJson(denied, "acme/denied#p")).read).toBe("error:missing permission fs.read")
    await denied.stopAll()
    const closed = manager()
    await closed.load([catalogRow("acme/closed", { "tui/main.ts": reader(secret) }, ["tui.panel", "fs.read"])], local)
    expect((await panelJson(closed, "acme/closed#p")).read).toBe("error:no Project is open")
    await closed.stopAll()
  }, 20_000)

  test("watches deliver batched changes, stop at the per-extension limit, and close when the Project changes", async () => {
    const { root } = project()
    const other = mkdtempSync(join(tmpdir(), "hya-fs-other-"))
    const watcher = `import { defineTuiExtension } from "@hya/tui-sdk"
export default defineTuiExtension({ activate(api) {
  let changes = 0, closed = 0
  for (let index = 0; index < 17; index++) api.fs.watch("sub", (events) => { for (const event of events) { if (event.kind === "closed") closed++; else changes++ } })
  api.registerPanel({ id: "p", title: "P", render: () => JSON.stringify({ changes, closed }) })
} })`
    const extensions = manager()
    extensions.setRoots([root])
    await extensions.load([catalogRow("acme/watcher", { "tui/main.ts": watcher }, ["tui.panel", "fs.read"])], local)
    // The 17th watch is over the limit of 16: it ends at once.
    await until(async () => (await panelJson(extensions, "acme/watcher#p")).closed === 1 || undefined, "the refused watch")
    for (let index = 0; index < 5; index++) writeFileSync(join(root, "sub", `file-${index}.txt`), "x")
    await until(async () => Number((await panelJson(extensions, "acme/watcher#p")).changes) >= 16 || undefined, "changes")
    extensions.setRoots([other])
    // The 16 live watches lie outside the new root: each ends with a `closed` event.
    await until(async () => (await panelJson(extensions, "acme/watcher#p")).closed === 17 || undefined, "closed on Project change")
    await extensions.stopAll()
  }, 20_000)
})
