/**
 * OS hardening of an extension process. The extension itself always runs in
 * the SDK's QuickJS-WASM VM (packages/hya-tui-sdk/src/host.ts); this adds the
 * operating system's sandbox around the whole process, with no helper binary:
 * the process is started through extensions/confine.ts, which restricts
 * itself (Linux: Landlock + seccomp; macOS: sandbox_init) and then execs the
 * extension runtime.
 *
 * Whether that works here (kernel without Landlock, an unusual container) is
 * found by running the launcher once with `bun --version`. Without it,
 * `best-effort` runs with the VM boundary only (with a warning) and `required`
 * refuses.
 */
import { readFile, realpath, mkdir, writeFile } from "node:fs/promises"
import { createHash } from "node:crypto"
import { release as osRelease } from "node:os"
import { join } from "node:path"

export type SandboxPolicy = "required" | "best-effort" | "disabled"
export const sandboxPolicies: readonly SandboxPolicy[] = ["required", "best-effort", "disabled"]

export interface SandboxPlan {
  readonly argv: string[]
  /** The OS sandbox applies (the VM boundary always does). */
  readonly isolated: boolean
  readonly warning?: string
}

/** Runs `argv` once; resolves to why it failed, or `undefined` when it exited 0. */
export type SandboxProbe = (argv: string[]) => Promise<string | undefined>

export interface SandboxRequest {
  /** `[bun, <sdk>/src/main.ts, <entry>]`. */
  readonly argv: string[]
  readonly policy: SandboxPolicy
  /** Paths the process may read: the SDK and the extension files (system libraries and the bun install are added by the launcher). */
  readonly readable: readonly string[]
  readonly platform?: NodeJS.Platform
  readonly probe?: SandboxProbe
  /** Persistent cache directory for the sandbox capability probe. */
  readonly cacheRoot?: string
}

export class SandboxUnavailableError extends Error {}

/** The launcher script (run with the same bun as the extension). */
export const confineScript = join(import.meta.dir, "confine.ts")

/**
 * Probes per launcher command: whether this machine can sandbox does not change
 * while the TUI runs. Asynchronous: the probe starts bun twice, and the TUI
 * must keep drawing and reading keys meanwhile.
 */
const probed = new Map<string, Promise<string | undefined>>()

async function probeCacheKey(argv: readonly string[], platform: NodeJS.Platform): Promise<string> {
  const confine = await readFile(confineScript)
  return createHash("sha256").update(JSON.stringify({
    bun: argv[0], bunVersion: process.versions.bun ?? process.version,
    confine: createHash("sha256").update(confine).digest("hex"),
    platform, release: osRelease(),
  })).digest("hex")
}

async function cachedProbe(cacheRoot: string | undefined, key: string, probe: SandboxProbe, argv: string[]): Promise<string | undefined> {
  if (!cacheRoot) return probe(argv)
  const path = join(cacheRoot, "sandbox-probe.json")
  try {
    const value = JSON.parse(await readFile(path, "utf8")) as { key?: unknown; reason?: unknown }
    if (value.key === key && (value.reason === undefined || typeof value.reason === "string")) return value.reason
  } catch { /* cache miss */ }
  const reason = await probe(argv)
  try {
    await mkdir(cacheRoot, { recursive: true })
    await writeFile(path, JSON.stringify({ key, reason }))
  } catch { /* cache write is best effort */ }
  return reason
}

const runProbe: SandboxProbe = async (argv) => {
  try {
    const child = Bun.spawn(argv, { env: {}, stdin: "ignore", stdout: "ignore", stderr: "pipe" })
    const [code, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()])
    if (code === 0) return undefined
    return stderr.trim().split("\n")[0] || `exited with status ${code}`
  } catch (error) {
    return error instanceof Error ? error.message : String(error)
  }
}

export async function planSandbox(request: SandboxRequest): Promise<SandboxPlan> {
  const { argv, policy } = request
  if (argv.length === 0) throw new SandboxUnavailableError("empty extension command")
  if (policy === "disabled") return { argv, isolated: false, warning: "OS sandbox disabled by preference; the extension VM still applies" }
  const platform = request.platform ?? process.platform
  let reason: string | undefined
  if (platform === "linux" || platform === "darwin") {
    const readable = [...new Set(await Promise.all(request.readable.map(async (path) => { try { return await realpath(path) } catch { return path } })))]
    const launcher = [argv[0]!, confineScript, ...readable, "--"]
    const key = JSON.stringify(launcher)
    let probe = probed.get(key)
    if (!probe) {
      const cacheKey = await probeCacheKey(argv, platform)
      probed.set(key, probe = cachedProbe(request.cacheRoot, cacheKey, request.probe ?? runProbe, [...launcher, argv[0]!, "--version"]))
    }
    reason = await probe
    if (reason === undefined) return { argv: [...launcher, ...argv], isolated: true }
  } else {
    reason = "unsupported platform"
  }
  const message = `no OS sandbox on ${platform} (${reason})`
  if (policy === "required") throw new SandboxUnavailableError(message)
  return { argv, isolated: false, warning: `${message}; the extension VM still applies` }
}
