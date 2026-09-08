#!/usr/bin/env bun
/**
 * TUI e2e: fake catalog endpoints for anthropic / openai-response /
 * openai-completion. Verifies models.yml.cache updates after the endpoint
 * model list changes (startup cache → background refresh rewrite).
 *
 * Usage:
 *   bun .agent-docs/tasks/provider-models-cache/tui-cache-refresh-e2e.mjs
 */
import { createServer } from "node:http"
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { spawn } from "node:child_process"
import { setTimeout as sleep } from "node:timers/promises"

const root = path.resolve(import.meta.dir, "../../..")
const hyaTs = path.join(root, "target/release/hya-ts")
const backend = path.join(root, "target/release/hya-backend")
const tuiDir = path.join(root, "packages/hya-tui-ts")

const PHASE1 = {
  anthropic: ["claude-phase1"],
  responses: ["gpt-response-phase1"],
  completion: ["gpt-completion-phase1"],
}
const PHASE2 = {
  anthropic: ["claude-phase2", "claude-phase2-extra"],
  responses: ["gpt-response-phase2"],
  completion: ["gpt-completion-phase2", "gpt-completion-phase2-b"],
}

/** @type {{ anthropic: string[], responses: string[], completion: string[] }} */
let live = structuredClone(PHASE1)

function catalogPayload(ids) {
  return JSON.stringify({
    object: "list",
    data: ids.map((id) => ({ id })),
    has_more: false,
  })
}

function startFakeCatalog() {
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://127.0.0.1")
    const pathname = url.pathname.replace(/\/+$/, "") || "/"
    res.setHeader("content-type", "application/json")
    if (pathname === "/anthropic/v1/models") {
      res.end(catalogPayload(live.anthropic))
      return
    }
    if (pathname === "/responses/v1/models") {
      res.end(catalogPayload(live.responses))
      return
    }
    if (pathname === "/completion/v1/models") {
      res.end(catalogPayload(live.completion))
      return
    }
    res.statusCode = 404
    res.end(JSON.stringify({ error: `no catalog for ${pathname}` }))
  })
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address()
      const port = typeof addr === "object" && addr ? addr.port : 0
      resolve({ server, port, base: `http://127.0.0.1:${port}` })
    })
  })
}

async function writeFixture(work, base) {
  const configHome = path.join(work, "config")
  const hya = path.join(configHome, "hya")
  await mkdir(hya, { recursive: true })
  await mkdir(path.join(work, "state", "hya"), { recursive: true })
  await mkdir(path.join(work, "project"), { recursive: true })
  await writeFile(path.join(work, "project", "README.md"), "# cache refresh fixture\n")
  const yaml = `default_model: anthropic/claude-phase1
providers:
  anthropic:
    kind: anthropic
    base_url: ${base}/anthropic/v1
    api_key: test-anthropic
    models: []
  responses:
    kind: openai-response
    base_url: ${base}/responses/v1
    api_key: test-responses
    models: []
  completion:
    kind: openai-completion
    base_url: ${base}/completion/v1
    api_key: test-completion
    models: []
`
  await writeFile(path.join(hya, "config.yaml"), yaml)
  return {
    configHome,
    cachePath: path.join(hya, "models.yml.cache"),
    project: path.join(work, "project"),
    home: path.join(work, "home"),
    state: path.join(work, "state"),
  }
}

function parseCache(text) {
  /** @type {Record<string, string[]>} */
  const out = {}
  let provider = null
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trimEnd()
    const providerMatch = line.match(/^ {2}([A-Za-z0-9_-]+):\s*$/)
    if (providerMatch && !line.includes("id:")) {
      provider = providerMatch[1]
      out[provider] = out[provider] ?? []
      continue
    }
    const idMatch = line.match(/^\s+- id:\s*(.+)\s*$/)
    if (idMatch && provider) {
      out[provider].push(idMatch[1].replace(/^['"]|['"]$/g, ""))
    }
  }
  return out
}

function assertIds(cache, provider, expected, label) {
  const got = [...(cache[provider] ?? [])].sort()
  const want = [...expected].sort()
  if (JSON.stringify(got) !== JSON.stringify(want)) {
    throw new Error(
      `${label}: provider ${provider} expected ${JSON.stringify(want)} got ${JSON.stringify(got)}`,
    )
  }
}

async function waitForCache(cachePath, predicate, label, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs
  let last = ""
  while (Date.now() < deadline) {
    try {
      last = await readFile(cachePath, "utf8")
      const parsed = parseCache(last)
      if (predicate(parsed, last)) return { text: last, parsed }
    } catch {
      // not written yet
    }
    await sleep(100)
  }
  throw new Error(`${label}: timed out waiting for cache.\nlast=\n${last}`)
}

async function runTuiOnce(fixture, label) {
  const marksFile = path.join(path.dirname(fixture.cachePath), `marks-${label}.log`)
  const transcript = path.join(path.dirname(fixture.cachePath), `transcript-${label}.log`)
  await writeFile(marksFile, "")
  const shellCmd = [
    "stty rows 40 cols 120;",
    `exec 2>${JSON.stringify(marksFile)};`,
    `${JSON.stringify(hyaTs)} ${JSON.stringify(fixture.project)}`,
  ].join(" ")
  const child = spawn("/usr/bin/script", ["-q", "-e", "-f", "-c", shellCmd, transcript], {
    cwd: tuiDir,
    env: {
      ...process.env,
      HOME: fixture.home,
      XDG_CONFIG_HOME: fixture.configHome,
      XDG_STATE_HOME: fixture.state,
      XDG_CACHE_HOME: path.join(path.dirname(fixture.state), "cache"),
      XDG_DATA_HOME: path.join(path.dirname(fixture.state), "data"),
      HYA_BACKEND_BIN: backend,
      HYA_TUI_TS_DIR: tuiDir,
      HYA_STARTUP_TRACE: "1",
      HYA_DB: "",
      TERM: "xterm-256color",
    },
    stdio: ["pipe", "ignore", "pipe"],
  })

  const deadline = Date.now() + 25_000
  let sawSync = false
  while (Date.now() < deadline) {
    await sleep(50)
    try {
      const text = await readFile(marksFile, "utf8")
      if (text.includes('"sync_complete"') && text.includes('"shell_paint"')) {
        sawSync = true
        break
      }
    } catch {
      // ignore
    }
    if (child.exitCode !== null) break
  }

  try {
    child.kill("SIGTERM")
  } catch {
    // ignore
  }
  await Promise.race([
    new Promise((resolve) => child.on("close", resolve)),
    sleep(3000),
  ])
  try {
    child.kill("SIGKILL")
  } catch {
    // ignore
  }
  if (!sawSync) {
    const marks = await readFile(marksFile, "utf8").catch(() => "")
    throw new Error(`${label}: TUI did not reach sync_complete/shell_paint\n${marks.slice(-1500)}`)
  }
  return marksFile
}

async function main() {
  for (const bin of [hyaTs, backend]) {
    try {
      await readFile(bin)
    } catch {
      throw new Error(`missing release binary: ${bin}`)
    }
  }

  const work = await mkdtemp(path.join(tmpdir(), "hya-tui-cache-e2e-"))
  const fake = await startFakeCatalog()
  const fixture = await writeFixture(work, fake.base)
  console.log(`fixture=${work}`)
  console.log(`fake=${fake.base}`)

  try {
    console.log("phase1: TUI cold start against PHASE1 catalogs")
    await runTuiOnce(fixture, "phase1")
    // models CLI awaits refresh; also forces a second discovery write if TUI
    // background refresh raced. Prefer waiting on the cache file itself.
    const phase1 = await waitForCache(
      fixture.cachePath,
      (parsed) =>
        (parsed.anthropic ?? []).includes("claude-phase1") &&
        (parsed.responses ?? []).includes("gpt-response-phase1") &&
        (parsed.completion ?? []).includes("gpt-completion-phase1"),
      "phase1 cache",
    )
    console.log("phase1 cache ok")
    assertIds(phase1.parsed, "anthropic", PHASE1.anthropic, "phase1")
    assertIds(phase1.parsed, "responses", PHASE1.responses, "phase1")
    assertIds(phase1.parsed, "completion", PHASE1.completion, "phase1")
    if (!phase1.text.includes("limit:") && !phase1.text.includes("context")) {
      throw new Error(`phase1 cache missing limit metadata:\n${phase1.text}`)
    }

    live = structuredClone(PHASE2)
    console.log("phase2: endpoint catalogs mutated; restart TUI for background refresh")
    await runTuiOnce(fixture, "phase2")
    const phase2 = await waitForCache(
      fixture.cachePath,
      (parsed) =>
        (parsed.anthropic ?? []).includes("claude-phase2") &&
        (parsed.anthropic ?? []).includes("claude-phase2-extra") &&
        (parsed.responses ?? []).includes("gpt-response-phase2") &&
        (parsed.completion ?? []).includes("gpt-completion-phase2") &&
        (parsed.completion ?? []).includes("gpt-completion-phase2-b") &&
        !(parsed.anthropic ?? []).includes("claude-phase1"),
      "phase2 cache rewrite",
      30_000,
    )
    console.log("phase2 cache ok")
    assertIds(phase2.parsed, "anthropic", PHASE2.anthropic, "phase2")
    assertIds(phase2.parsed, "responses", PHASE2.responses, "phase2")
    assertIds(phase2.parsed, "completion", PHASE2.completion, "phase2")

    console.log(
      JSON.stringify(
        {
          ok: true,
          styles: ["anthropic", "openai-response", "openai-completion"],
          phase1: phase1.parsed,
          phase2: phase2.parsed,
          cache_path: fixture.cachePath,
        },
        null,
        2,
      ),
    )
  } finally {
    fake.server.close()
    await rm(work, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exit(1)
})
