import { mkdir, mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"

export type AdapterResponse = {
  readonly jsonrpc: string
  readonly id?: number
  readonly result?: unknown
  readonly error?: { readonly code: number; readonly message: string }
}

const tempDirs: string[] = []

export async function makeTempDir(prefix = "hya-bun-"): Promise<string> {
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

export type RunAdapterOptions = {
  readonly env?: Readonly<Record<string, string>>
  readonly argv?: readonly string[]
}

/** Spawn the adapter process, feed it NDJSON requests, and collect replies. */
export async function runAdapterProcess(
  requests: readonly unknown[],
  options: RunAdapterOptions = {},
): Promise<readonly AdapterResponse[]> {
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
  if (exitCode !== 0) {
    throw new Error(`adapter exited with ${exitCode}: ${stderr}`)
  }
  return stdout
    .trim()
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line) as AdapterResponse)
}

export function initializeRequest(id: number, extra: Record<string, unknown> = {}): unknown {
  return {
    jsonrpc: "2.0",
    id,
    method: "initialize",
    params: {
      protocol_version: 1,
      host: { name: "hya", version: "0.0.0" },
      ...extra,
    },
  }
}

export function shutdownRequest(id: number): unknown {
  return { jsonrpc: "2.0", id, method: "shutdown", params: {} }
}
