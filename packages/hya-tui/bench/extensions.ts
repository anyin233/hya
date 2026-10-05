/**
 * Startup cost of a realistic extension set through the real manager, SDK and
 * shared host: 20 bundles, each with a full panel, half of them reading and
 * watching Project files.
 *
 * Reports, for a cold cache and a warm one, with every extension in the VM
 * and with every extension trusted (JIT tier): time from catalog arrival to
 * every panel's first render, the host's RSS, and the longest stall of the TUI
 * thread (an event-loop lag sampler). Targets: ≤ 300 ms, ≤ 300 MB, ≈ 5 ms.
 *
 *   bun run bench            # from packages/hya-tui
 *   HYA_BENCH_SANDBOX=disabled bun run bench
 */
import { createHash } from "node:crypto"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { locateSdk } from "../src/extensions/install"
import { ExtensionManager } from "../src/extensions/manager"
import type { ChildProcess } from "../src/extensions/hostChannel"

const count = 20
const sha = (text: string) => createHash("sha256").update(text).digest("hex")
const helpers = Array.from({ length: 60 }, (_, index) =>
  `function helper${index}(rows) { return rows.map((row) => ({ ...row, v${index}: String(row.n * ${index}).padStart(6) })) }`).join("\n")

function source(index: number): string {
  const files = index % 2 === 0
  return `import { defineTuiExtension } from "@hya/tui-sdk"
${helpers}
export default defineTuiExtension({ activate(api) {
  let text = ""
  ${files ? `api.fs.watch(".", () => true)` : ""}
  api.registerPanel({ id: "p", title: "Panel ${index}", render: async ({ width }) => {
    ${files ? `text ||= await api.fs.read("notes-${index}.md")` : ""}
    const rows = helper7(helper3(Array.from({ length: 40 }, (_, n) => ({ n }))))
    return { kind: "column", children: [{ kind: "text", text: text.slice(0, width) }, ...rows.map((row) => ({ kind: "text", text: ("row " + row.n + row.v3).slice(0, width) }))] }
  } })
} })`
}

/** Catalog rows as the server sends them; `cached` rows carry no files (the TUI reported their digest). */
function catalog(cached: boolean): Record<string, unknown>[] {
  return Array.from({ length: count }, (_, index) => {
    const content = source(index)
    return {
      bundleId: `bench/ext-${index}`, bundleVersion: "1.0.0", preparedDigest: sha(content), apiVersion: 1, entry: "tui/main.ts", sdk: "1.0.0",
      permissions: index % 2 === 0 ? ["tui.panel", "fs.read"] : ["tui.panel"],
      files: cached ? [] : [{ path: "tui/main.ts", sha256: sha(content), content }], cached,
    }
  })
}

/** The host's resident set in MB: `/proc` on Linux (slim images have no `ps`), else `ps`. */
async function rssMb(pid: number): Promise<number> {
  const status = await Bun.file(`/proc/${pid}/status`).text().catch(() => undefined)
  const kb = status ? /VmRSS:\s+(\d+)/.exec(status)?.[1] : (await new Response(Bun.spawn(["ps", "-o", "rss=", "-p", String(pid)]).stdout).text()).trim()
  return Math.round(Number(kb) / 1024)
}

/** `fetchMs`: how long the catalog request takes; the TUI prewarms the host meanwhile (src/app/run.tsx). */
async function run(label: string, cacheRoot: string, project: string, rows: Record<string, unknown>[], fetchMs: number, jit: boolean) {
  const sdk = locateSdk()
  if (!sdk) throw new Error("the TUI extension SDK is not installed next to this TUI")
  let pid = 0
  const extensions = new ExtensionManager({
    cacheRoot, sdk, sandbox: (process.env.HYA_BENCH_SANDBOX ?? "best-effort") as "best-effort",
    spawn: (argv) => {
      const child = Bun.spawn(argv, { stdin: "pipe", stdout: "pipe", stderr: "pipe" })
      pid = child.pid
      return child as unknown as ChildProcess
    },
  })
  extensions.setRoots([project])
  // The sampler fires every millisecond; a longer gap is time the TUI thread could not paint.
  let lag = 0
  let last = performance.now()
  const sampler = setInterval(() => {
    const now = performance.now()
    lag = Math.max(lag, now - last - 1)
    last = now
  }, 1)
  const fetchStart = performance.now()
  void extensions.prewarm()
  await Bun.sleep(fetchMs)
  const start = performance.now()
  const rejected = await extensions.load(rows, { enabled: () => undefined, remote: false, trusted: () => jit })
  if (rejected.length) throw new Error(`catalog rejected: ${rejected.join("; ")}`)
  const keys = rows.map((row) => `${String(row.bundleId)}#p`)
  for (;;) {
    if (keys.every((key) => extensions.panelView(key, 60, 40).node)) break
    if (performance.now() - start > 10_000) {
      const missing = keys.filter((key) => !extensions.panelView(key, 60, 40).node)
      throw new Error(`panels not rendered: ${missing.map((key) => `${key} ${JSON.stringify(extensions.panelView(key, 60, 40)).slice(0, 200)}`).join("; ")}`)
    }
    await Bun.sleep(1)
  }
  const rendered = performance.now()
  clearInterval(sampler)
  const rss = await rssMb(pid)
  await extensions.stopAll()
  console.log(`${jit ? "jit" : "vm "} ${label.padEnd(5)} catalog after ${String(fetchMs).padStart(3)} ms · every panel rendered ${(rendered - start).toFixed(0).padStart(4)} ms after it (${(rendered - fetchStart).toFixed(0).padStart(4)} ms after the fetch began) · host RSS ${rss} MB · longest TUI stall ${lag.toFixed(1)} ms`)
}

const base = mkdtempSync(join(tmpdir(), "hya-bench-"))
const project = join(base, "project")
mkdirSync(project)
for (let index = 0; index < count; index++) writeFileSync(join(project, `notes-${index}.md`), `notes ${index}`)
try {
  const cache = join(base, "cache")
  for (const jit of [false, true]) {
    for (const fetchMs of [0, 100]) {
      await run("cold", cache, project, catalog(false), fetchMs, jit)
      await run("warm", cache, project, catalog(true), fetchMs, jit)
      rmSync(cache, { recursive: true, force: true })
    }
  }
} finally {
  rmSync(base, { recursive: true, force: true })
}
