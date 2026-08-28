import { expect, test } from "bun:test"

import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import stripAnsi from "strip-ansi"

const root = path.resolve(import.meta.dir, "../../..")
const backend = path.join(root, "target/debug/hya-backend")
const launcher = path.join(root, "target/debug/hya-ts")

/** Wait until an observable condition becomes true or fail with its label. */
async function waitFor(check: () => boolean | Promise<boolean>, label: string) {
  const deadline = Date.now() + 20_000
  while (!(await check())) {
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}`)
    await Bun.sleep(50)
  }
}

/** Wait for the backend's ephemeral listen address. */
async function serverUrl(stdout: ReadableStream<Uint8Array>) {
  const reader = stdout.getReader()
  const decoder = new TextDecoder()
  let output = ""
  while (true) {
    const chunk = await reader.read()
    if (chunk.done) throw new Error(`hya-backend exited before readiness: ${output}`)
    output += decoder.decode(chunk.value, { stream: true })
    const match = output.match(/hya server listening on (http:\/\/127\.0\.0\.1:\d+)/)
    if (match) {
      reader.releaseLock()
      return match[1]
    }
  }
}

/** Start the real TUI under a fixed-width Linux PTY transcript. */
function startTui(input: {
  columns: number
  env: Record<string, string | undefined>
  project: string
  session: string
  transcript: string
  url: string
}) {
  return Bun.spawn(
    [
      "/usr/bin/script",
      "-q",
      "-e",
      "-f",
      "-c",
      `stty rows 30 cols ${input.columns}; "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_ROOT_SESSION"`,
      input.transcript,
    ],
    {
      cwd: path.join(root, "packages/hya-tui-ts"),
      env: {
        ...input.env,
        HYA_PTY_PROJECT: input.project,
        HYA_PTY_URL: input.url,
        HYA_ROOT_SESSION: input.session,
        HYA_TS: launcher,
        HYA_TUI_TS_DIR: path.join(root, "packages/hya-tui-ts"),
        TERM: "xterm-256color",
      },
      stdin: "pipe",
      stdout: "ignore",
      stderr: "pipe",
    },
  )
}

/** Stop one PTY process without leaving its frontend child behind. */
async function stopTui(process: ReturnType<typeof Bun.spawn>) {
  process.kill()
  await Promise.race([process.exited, Bun.sleep(2_000).then(() => process.kill(9))])
}

test("real PTY shows Workflow fan-out, terminal replay, and narrow layout", async () => {
  const temp = await mkdtemp(path.join(os.tmpdir(), "hya-workflow-pty-"))
  const project = path.join(temp, "project")
  const configHome = path.join(temp, "config")
  await mkdir(path.join(project, ".hya/workflows"), { recursive: true })
  await mkdir(path.join(configHome, "hya"), { recursive: true })
  await mkdir(path.join(temp, "home"), { recursive: true })

  const provider = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      if (!new URL(request.url).pathname.endsWith("/chat/completions")) {
        return new Response("not found", { status: 404 })
      }
      await Bun.sleep(300)
      const encoder = new TextEncoder()
      return new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(
              encoder.encode(
                `data: ${JSON.stringify({
                  id: "chatcmpl-workflow-pty",
                  object: "chat.completion.chunk",
                  created: 0,
                  model: "workflow-pty",
                  choices: [
                    {
                      index: 0,
                      delta: { role: "assistant", content: "stage complete" },
                      finish_reason: null,
                    },
                  ],
                })}\n\n`,
              ),
            )
            controller.enqueue(
              encoder.encode(
                `data: ${JSON.stringify({
                  id: "chatcmpl-workflow-pty",
                  object: "chat.completion.chunk",
                  created: 0,
                  model: "workflow-pty",
                  choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
                })}\n\n`,
              ),
            )
            controller.enqueue(encoder.encode("data: [DONE]\n\n"))
            controller.close()
          },
        }),
        { headers: { "content-type": "text/event-stream" } },
      )
    },
  })
  await writeFile(
    path.join(configHome, "hya/config.yaml"),
    [
      "default_model: workflow-pty/workflow-pty",
      "providers:",
      "  workflow-pty:",
      "    kind: openai-completion",
      `    base_url: ${provider.url.toString()}v1`,
      "    api_key: test-key",
      "    models: [workflow-pty]",
      "permission:",
      "  model: allow",
      "",
    ].join("\n"),
  )
  await writeFile(
    path.join(project, ".hya/workflows/visual-fanout.hya.md"),
    `---
kind: Workflow
name: visual-fanout
description: PTY verification graph.
inputs:
  request: Visual request.
on_failure: collect_all
nodes:
  plan:
    title: Plan
    agent: plan-impl-review-planner
    directive: Plan {{input.request}}.
  alpha:
    title: Alpha
    agent: plan-impl-review-implementer
    directive: Implement alpha.
  beta:
    title: Beta
    agent: plan-impl-review-implementer
    directive: Implement beta.
  review:
    title: Review
    agent: plan-impl-review-reviewer
    directive: Review predecessor evidence.
---
flowchart TD
  plan --> alpha & beta
  alpha & beta --> review
`,
  )

  const env = {
    ...Bun.env,
    HOME: path.join(temp, "home"),
    XDG_CACHE_HOME: path.join(temp, "cache"),
    XDG_CONFIG_HOME: configHome,
    XDG_DATA_HOME: path.join(temp, "data"),
    XDG_STATE_HOME: path.join(temp, "state"),
  }
  const server = Bun.spawn(
    [backend, "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"],
    { cwd: project, env, stdout: "pipe", stderr: "pipe" },
  )
  let first: ReturnType<typeof Bun.spawn> | undefined
  let restored: ReturnType<typeof Bun.spawn> | undefined
  let narrow: ReturnType<typeof Bun.spawn> | undefined

  try {
    const url = await serverUrl(server.stdout)
    const headers = { "content-type": "application/json", "x-opencode-directory": project }
    const created = await fetch(`${url}/session`, {
      method: "POST",
      headers,
      body: JSON.stringify({ title: "Workflow PTY proof" }),
    })
    expect(created.ok).toBe(true)
    const session = (await created.json()) as { id: string }
    const selected = await fetch(`${url}/session/${session.id}/workflow`, {
      method: "POST",
      headers,
      body: JSON.stringify({ command: "select", name: "visual-fanout" }),
    })
    expect(selected.ok).toBe(true)

    const firstTranscript = path.join(temp, "first")
    first = startTui({ columns: 140, env, project, session: session.id, transcript: firstTranscript, url })
    await waitFor(
      async () => stripAnsi(await readFile(firstTranscript, "utf8").catch(() => "")).includes("visual-fanout · ready"),
      "selected Workflow sidebar",
    )

    const started = await fetch(`${url}/session/${session.id}/workflow`, {
      method: "POST",
      headers,
      body: JSON.stringify({ command: "run", name: null, inputs: { request: "verify Workflow PTY" } }),
    })
    expect(started.ok).toBe(true)
    await waitFor(
      async () => stripAnsi(await readFile(firstTranscript, "utf8")).includes("visual-fanout · completed"),
      "terminal Workflow sidebar",
    )
    const lifecycle = stripAnsi(await readFile(firstTranscript, "utf8"))
    expect(lifecycle).toContain("active plan")
    // `script` records cursor-diff payloads, so an in-place Plan → Alpha redraw may retain only `apha +1`.
    expect(lifecycle).toMatch(/(?:active al)?pha \+1/)
    expect(lifecycle).toContain("visual-fanout · completed")
    await stopTui(first)
    first = undefined

    const restoredTranscript = path.join(temp, "restored")
    restored = startTui({ columns: 140, env, project, session: session.id, transcript: restoredTranscript, url })
    await waitFor(
      async () => stripAnsi(await readFile(restoredTranscript, "utf8").catch(() => "")).includes("visual-fanout · completed"),
      "replayed terminal Workflow sidebar",
    )
    const replayed = stripAnsi(await readFile(restoredTranscript, "utf8"))
    expect(replayed).toContain("revision")
    expect(replayed).toContain("4/4 stages")
    await stopTui(restored)
    restored = undefined

    const narrowTranscript = path.join(temp, "narrow")
    narrow = startTui({ columns: 80, env, project, session: session.id, transcript: narrowTranscript, url })
    await waitFor(
      async () => stripAnsi(await readFile(narrowTranscript, "utf8").catch(() => "")).includes("commands"),
      "narrow Session prompt",
    )
    expect(stripAnsi(await readFile(narrowTranscript, "utf8"))).not.toContain("visual-fanout · completed")
  } finally {
    if (first) await stopTui(first)
    if (restored) await stopTui(restored)
    if (narrow) await stopTui(narrow)
    server.kill()
    await Promise.race([server.exited, Bun.sleep(2_000).then(() => server.kill(9))])
    provider.stop(true)
    await rm(temp, { recursive: true, force: true })
  }
}, 45_000)
