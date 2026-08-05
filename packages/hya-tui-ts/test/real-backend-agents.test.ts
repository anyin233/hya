/**
 * Track T: real-backend multi-agent roster visibility via pinned SDK.
 * Complements task-presentation unit tests with a live hya-backend process.
 */
import { afterEach, expect, test } from "bun:test"
import { createOpencodeClient } from "@opencode-ai/sdk/v2/client"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

const root = path.resolve(import.meta.dir, "../../..")
const backend = path.join(root, "target/debug/hya-backend")
const cleanups: Array<() => Promise<void>> = []

afterEach(async () => {
  await Promise.all(cleanups.splice(0).map((cleanup) => cleanup()))
})

async function startBackend() {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-real-backend-agents-")))
  const project = path.join(temp, "project")
  await mkdir(project)
  await writeFile(path.join(project, "README.md"), "real backend agents fixture\n")
  const args = [backend, "--yolo", "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"]
  const process = Bun.spawn(args, {
    cwd: project,
    env: {
      ...Bun.env,
      HOME: path.join(temp, "home"),
      XDG_CONFIG_HOME: path.join(temp, "config"),
      XDG_STATE_HOME: path.join(temp, "state"),
      XDG_CACHE_HOME: path.join(temp, "cache"),
    },
    stdout: "pipe",
    stderr: "pipe",
  })
  const reader = process.stdout.getReader()
  const decoder = new TextDecoder()
  let output = ""
  const url = await Promise.race([
    (async () => {
      while (true) {
        const chunk = await reader.read()
        if (chunk.done) throw new Error(`hya-backend exited before readiness: ${output}`)
        output += decoder.decode(chunk.value, { stream: true })
        const match = output.match(/hya server listening on (http:\/\/127\.0\.0\.1:\d+)/)
        if (match) return match[1]
      }
    })(),
    Bun.sleep(10_000).then(() => {
      throw new Error(`timed out waiting for hya-backend: ${output}`)
    }),
  ])
  cleanups.push(async () => {
    process.kill()
    await Promise.race([process.exited, Bun.sleep(2_000).then(() => process.kill(9))])
    await rm(temp, { recursive: true, force: true })
  })
  return { project, url }
}

test("pinned SDK lists multi-agent roster from real backend", async () => {
  const { project, url } = await startBackend()
  const client = createOpencodeClient({ baseUrl: url, directory: project })

  const agents = await client.app.agents({}, { throwOnError: true })
  const list = agents.data ?? []
  expect(Array.isArray(list)).toBe(true)
  expect(list.length).toBeGreaterThan(0)

  const names = list.map((agent: { name?: string; id?: string }) => agent.name ?? agent.id ?? "").filter(Boolean)
  const blob = JSON.stringify(list)
  expect(blob.includes("build") || names.some((n) => n.includes("build"))).toBe(true)

  // Spawnable ordinary agents used by task presentation / multi-agent UI.
  const hasSpawnable =
    blob.includes("general") || blob.includes("explore") || blob.includes("plan") || names.length > 1
  expect(hasSpawnable).toBe(true)

  const created = await client.session.create({ title: "Roster session" }, { throwOnError: true })
  const sessionID = created.data!.id
  expect(sessionID).toBeTruthy()
  const listed = await client.session.list({}, { throwOnError: true })
  expect(listed.data!.some((session) => session.id === sessionID)).toBe(true)
}, 30_000)
