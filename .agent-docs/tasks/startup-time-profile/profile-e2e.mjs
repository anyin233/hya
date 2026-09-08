#!/usr/bin/env bun
/**
 * Capture HYA_STARTUP_TRACE marks from owned-mode hya-ts cold start
 * until sync_complete (or timeout). Runs under /usr/bin/script for a PTY.
 *
 * Usage:
 *   bun .agent-docs/tasks/startup-time-profile/profile-e2e.mjs [--runs N] [--mode f0|realistic]
 */
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { spawn } from "node:child_process"

const root = path.resolve(import.meta.dir, "../../..")
const hyaTs = path.join(root, "target/release/hya-ts")
const backend = path.join(root, "target/release/hya-backend")
const tuiDir = path.join(root, "packages/hya-tui-ts")

const args = process.argv.slice(2)
let runs = 5
let mode = "f0"
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--runs") runs = Number(args[++i])
  else if (args[i] === "--mode") mode = args[++i]
}

function parseMarks(text) {
  const marks = []
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (!trimmed.includes('"hya_startup"')) continue
    // Strip script/ANSI noise around a JSON object
    const start = trimmed.indexOf("{")
    const end = trimmed.lastIndexOf("}")
    if (start < 0 || end <= start) continue
    try {
      const obj = JSON.parse(trimmed.slice(start, end + 1))
      if (obj?.hya_startup === true && typeof obj.mark === "string" && typeof obj.wall_ms === "number") {
        marks.push(obj)
      }
    } catch {
      // ignore non-json
    }
  }
  return marks
}

function summarize(marks, t0Wall) {
  const byMark = {}
  for (const m of marks) {
    if (!(m.mark in byMark)) {
      byMark[m.mark] = {
        mark: m.mark,
        delta_ms: m.wall_ms - t0Wall,
        detail: m.detail,
        mono_ms: m.mono_ms,
      }
    }
  }
  return byMark
}

async function oneRun(runIndex) {
  const work = await mkdtemp(path.join(tmpdir(), "hya-startup-e2e-"))
  const project = path.join(work, "project")
  const home = path.join(work, "home")
  const marksFile = path.join(work, "marks.log")
  const transcript = path.join(work, "transcript")
  await mkdir(project)
  await writeFile(path.join(project, "README.md"), "# fixture\n")

  const env = {
    ...process.env,
    HYA_STARTUP_TRACE: "1",
    HYA_BACKEND_BIN: backend,
    HYA_TUI_TS_DIR: tuiDir,
    TERM: "xterm-256color",
    // Force marks onto a dedicated fd via shell redirect inside script -c
  }

  if (mode === "f0") {
    await mkdir(path.join(home, ".config", "hya"), { recursive: true })
    await mkdir(path.join(home, ".local", "state", "hya"), { recursive: true })
    await writeFile(
      path.join(home, ".config", "hya", "config.yaml"),
      "default_model: fake/fake\nproviders: {}\n",
    )
    env.HOME = home
    env.XDG_CONFIG_HOME = path.join(home, ".config")
    env.XDG_STATE_HOME = path.join(home, ".local", "state")
    env.HYA_DB = "" // in-memory; avoid sessions.db recovery
  }
  // realistic: inherit real HOME/config/db

  const t0Wall = Date.now()
  const shellCmd = [
    "stty rows 40 cols 120;",
    `exec 2>${JSON.stringify(marksFile)};`,
    `${JSON.stringify(hyaTs)} ${JSON.stringify(project)}`,
  ].join(" ")

  const child = spawn(
    "/usr/bin/script",
    ["-q", "-e", "-f", "-c", shellCmd, transcript],
    {
      cwd: tuiDir,
      env,
      stdio: ["pipe", "ignore", "pipe"],
    },
  )

  let stderr = ""
  child.stderr.on("data", (buf) => {
    stderr += buf.toString("utf8")
  })

  const deadline = Date.now() + 25_000
  let marks = []
  let done = false
  while (Date.now() < deadline && !done) {
    await Bun.sleep(30)
    let text = ""
    try {
      text = await readFile(marksFile, "utf8")
    } catch {
      // not created yet
    }
    marks = parseMarks(text)
    if (marks.some((m) => m.mark === "sync_complete") && marks.some((m) => m.mark === "shell_paint")) {
      done = true
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
    Bun.sleep(2000),
  ])
  try {
    child.kill("SIGKILL")
  } catch {
    // ignore
  }

    let byMark = summarize(marks, t0Wall)
  const row = {
    run: runIndex,
    t0_wall_ms: t0Wall,
    marks: byMark,
    wall_to_shell_ms: byMark.shell_paint?.delta_ms ?? null,
    wall_to_sync_ms: byMark.sync_complete?.delta_ms ?? null,
    wall_to_backend_ms: byMark.backend_listen?.delta_ms ?? null,
    wall_to_bun_ms: byMark.bun_entry?.delta_ms ?? null,
    wall_to_bun_spawn_ms: byMark.bun_spawn?.delta_ms ?? null,
    mark_count: marks.length,
    stderr_tail: stderr.slice(-500),
  }

  await rm(work, { recursive: true, force: true })
  return row
}

function pct(sorted, p) {
  if (!sorted.length) return null
  const idx = Math.round((sorted.length - 1) * p)
  return sorted[idx]
}

const samples = []
for (let i = 1; i <= runs; i++) {
  const row = await oneRun(i)
  samples.push(row)
  console.log(
    `run ${i} backend=${row.wall_to_backend_ms} bun=${row.wall_to_bun_ms} shell=${row.wall_to_shell_ms} sync=${row.wall_to_sync_ms} marks=${row.mark_count}`,
  )
  if (row.mark_count === 0) console.log("  stderr:", row.stderr_tail)
  else console.log("  marks:", JSON.stringify(row.marks))
}

const shells = samples.map((s) => s.wall_to_shell_ms).filter((x) => x != null).sort((a, b) => a - b)
const syncs = samples.map((s) => s.wall_to_sync_ms).filter((x) => x != null).sort((a, b) => a - b)
const backends = samples.map((s) => s.wall_to_backend_ms).filter((x) => x != null).sort((a, b) => a - b)

console.log(
  JSON.stringify(
    {
      mode,
      n: runs,
      backend_p50: pct(backends, 0.5),
      backend_p95: pct(backends, 0.95),
      shell_p50: pct(shells, 0.5),
      shell_p95: pct(shells, 0.95),
      sync_p50: pct(syncs, 0.5),
      sync_p95: pct(syncs, 0.95),
      samples,
    },
    null,
    2,
  ),
)
