import { expect, test } from "bun:test"
import { createOpencodeClient } from "@opencode-ai/sdk/v2/client"

import { mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import stripAnsi from "strip-ansi"

const root = path.resolve(import.meta.dir, "../../..")
const backend = path.join(root, "target/debug/hya-backend")
const launcher = path.join(root, "target/debug/hya-ts")

type FileSinkLike = {
  write(value: string): number | Promise<number>
  flush(): number | Promise<number>
}

async function writeSemanticInput(stdin: FileSinkLike, value: string) {
  await stdin.write(value)
  await stdin.flush()
}

test("semantic_input_flushes_before_next_action", async () => {
  let pending = ""
  const batches: string[] = []
  const stdin: FileSinkLike = {
    async write(value) {
      await Promise.resolve()
      pending += value
      return value.length
    },
    flush() {
      batches.push(pending)
      pending = ""
      return 0
    },
  }

  await writeSemanticInput(stdin, "chord-a")
  await writeSemanticInput(stdin, "chord-b")

  expect(batches).toEqual(["chord-a", "chord-b"])
})

test("Linux PTY renders home, opens a session, and restores the terminal", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-pty-smoke-")))
  const project = path.join(temp, "project")
  const transcript = path.join(temp, "typescript")
  await mkdir(project)
  await mkdir(path.join(temp, "home"))

  const env = {
    ...Bun.env,
    HOME: path.join(temp, "home"),
    XDG_CACHE_HOME: path.join(temp, "cache"),
    XDG_CONFIG_HOME: path.join(temp, "config"),
    XDG_STATE_HOME: path.join(temp, "state"),
  }
  const server = Bun.spawn([backend, "--yolo", "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"], {
    cwd: project,
    env,
    stdout: "pipe",
    stderr: "pipe",
  })

  try {
    const reader = server.stdout.getReader()
    const decoder = new TextDecoder()
    let readiness = ""
    const url = await Promise.race([
      (async () => {
        while (true) {
          const chunk = await reader.read()
          if (chunk.done) throw new Error(`hya-backend exited before readiness: ${readiness}`)
          readiness += decoder.decode(chunk.value, { stream: true })
          const match = readiness.match(/hya server listening on (http:\/\/127\.0\.0\.1:\d+)/)
          if (match) return match[1]
        }
      })(),
      Bun.sleep(10_000).then(() => {
        throw new Error(`timed out waiting for hya-backend: ${readiness}`)
      }),
    ])
    const process = Bun.spawn(
      [
        "/usr/bin/script",
        "-q",
        "-e",
        "-f",
        "-c",
        'stty rows 30 cols 100; before=$(stty -g); before_fg=$(ps -o tpgid= -p $$ | tr -d " "); "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --prompt "$HYA_PTY_PROMPT"; code=$?; after=$(stty -g); after_fg=$(ps -o tpgid= -p $$ | tr -d " "); [ "$before" = "$after" ] || exit 97; [ "$before_fg" = "$after_fg" ] || exit 98; exit "$code"',
        transcript,
      ],
      {
        cwd: path.join(root, "packages/hya-tui-ts"),
        env: {
          ...env,
          HYA_PTY_PROJECT: project,
          HYA_PTY_PROMPT: "PTY session smoke",
          HYA_PTY_URL: url,
          HYA_TS: launcher,
          HYA_TUI_TS_DIR: path.join(root, "packages/hya-tui-ts"),
          TERM: "xterm-256color",
        },
        stdin: "pipe",
        stdout: "ignore",
        stderr: "pipe",
      },
    )

    const marker = "(hya dev provider) You said"
    const deadline = Date.now() + 10_000
    while (!stripAnsi(await readFile(transcript, "utf8").catch(() => "")).includes(marker)) {
      const exited = await Promise.race([
        process.exited.then((status) => ({ status })),
        Bun.sleep(100).then(() => undefined),
      ])
      if (exited) throw new Error(`PTY exited before rendering the response with status ${exited.status}`)
      if (Date.now() >= deadline) {
        process.kill(9)
        throw new Error("PTY did not render the response before timeout")
      }
    }
    await writeSemanticInput(process.stdin, "\x03")
    process.stdin.end()
    const status = await Promise.race([
      process.exited,
      Bun.sleep(15_000).then(() => {
        process.kill(9)
        throw new Error("PTY smoke timed out")
      }),
    ])
    const output = stripAnsi(await readFile(transcript, "utf8"))

    expect(status).toBe(0)
    expect(output.toLowerCase()).toContain("hya")
    expect(output).toContain("PTY session smoke")
    expect(output).toContain(marker)
    expect(output).toContain("Session")
  } finally {
    server.kill()
    await Promise.race([server.exited, Bun.sleep(2_000).then(() => server.kill(9))])
    await rm(temp, { recursive: true, force: true })
  }
}, 45_000)

async function runChildObservation(columns: number) {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-pty-child-")))
  const project = path.join(temp, "project")
  const transcript = path.join(temp, "typescript")
  await mkdir(project)
  await mkdir(path.join(temp, "home"))

  const env = {
    ...Bun.env,
    HOME: path.join(temp, "home"),
    XDG_CACHE_HOME: path.join(temp, "cache"),
    XDG_CONFIG_HOME: path.join(temp, "config"),
    XDG_STATE_HOME: path.join(temp, "state"),
  }
  const server = Bun.spawn([backend, "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"], {
    cwd: project,
    env,
    stdout: "pipe",
    stderr: "pipe",
  })

  try {
    const reader = server.stdout.getReader()
    const decoder = new TextDecoder()
    let readiness = ""
    const url = await Promise.race([
      (async () => {
        while (true) {
          const chunk = await reader.read()
          if (chunk.done) throw new Error(`hya-backend exited before readiness: ${readiness}`)
          readiness += decoder.decode(chunk.value, { stream: true })
          const match = readiness.match(/hya server listening on (http:\/\/127\.0\.0\.1:\d+)/)
          if (match) return match[1]
        }
      })(),
      Bun.sleep(10_000).then(() => {
        throw new Error(`timed out waiting for hya-backend: ${readiness}`)
      }),
    ])

    const client = createOpencodeClient({ baseUrl: url, directory: project })
    const rootSession = (await client.session.create({ title: "PTY observation root" }, { throwOnError: true })).data!
    const childSession = (
      await client.session.create(
        { title: "@worker subagent", parentID: rootSession.id },
        { throwOnError: true },
      )
    ).data!
    const secondChildSession = (
      await client.session.create(
        { title: "@reviewer subagent", parentID: rootSession.id },
        { throwOnError: true },
      )
    ).data!
    const scrollChildSession = (
      await client.session.create(
        { title: "@scroll subagent", parentID: rootSession.id },
        { throwOnError: true },
      )
    ).data!
    const grandchildSession = (
      await client.session.create(
        { title: "@researcher subagent", parentID: childSession.id },
        { throwOnError: true },
      )
    ).data!
    const resetRootSession = (await client.session.create({ title: "PTY reset root" }, { throwOnError: true })).data!
    const rootTranscript = "ROOT_TRANSCRIPT_7f32"
    const childTranscript = "CHILD_TRANSCRIPT_98ac"
    const secondChildTranscript = "SECOND_CHILD_TRANSCRIPT_42de"
    const grandchildTranscript = "GRANDCHILD_TRANSCRIPT_51bf"
    const scrollChildTranscript = "SCROLL_CHILD_TRANSCRIPT_51bf"
    const scrollChildTail = "SCROLL_CHILD_TAIL_b419"
    const resetRootTranscript = "RESET_ROOT_TRANSCRIPT_4ae1"
    await client.session.promptAsync(
      { sessionID: rootSession.id, parts: [{ type: "text", text: rootTranscript }] },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      { sessionID: childSession.id, parts: [{ type: "text", text: childTranscript }] },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      { sessionID: secondChildSession.id, parts: [{ type: "text", text: secondChildTranscript }] },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      { sessionID: grandchildSession.id, parts: [{ type: "text", text: grandchildTranscript }] },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      {
        sessionID: scrollChildSession.id,
        parts: [{ type: "text", text: `${scrollChildTranscript}\n${"SCROLL_FILLER\n".repeat(40)}${scrollChildTail}` }],
      },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      { sessionID: resetRootSession.id, parts: [{ type: "text", text: resetRootTranscript }] },
      { throwOnError: true },
    )
    // CI runners are slower under sequential PTY cases; keep local waits tight.
    const waitTimeout = Bun.env.CI ? 60_000 : 20_000
    const waitPoll = Bun.env.CI ? 100 : 50
    const waitFor = async (check: () => boolean | Promise<boolean>, message: string) => {
      const deadline = Date.now() + waitTimeout
      while (!(await check())) {
        if (Date.now() >= deadline) throw new Error(`timed out waiting for ${message}`)
        await Bun.sleep(waitPoll)
      }
    }
    /** Root session is ready once the fixture text paints, or once message-hydrated chrome is on screen.
     * At wider columns, virtualized history may never emit the older user turn into the PTY capture
     * even though the session is open (task presentation + footer chrome). */
    const rootSessionFrameReady = (frame: string) =>
      frame.includes(rootTranscript) ||
      (frame.includes("Inspect worker path") &&
        frame.includes("ctrl+x o") &&
        (frame.includes("subagent roster") || frame.includes("commands")))
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: rootSession.id })).data
      return JSON.stringify(messages).includes(`(hya dev provider) You said: \\"${rootTranscript}\\"`)
    }, "root transcript fixture")
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: childSession.id })).data
      return JSON.stringify(messages).includes(`(hya dev provider) You said: \\"${childTranscript}\\"`)
    }, "child transcript fixture")
    for (const [sessionID, marker] of [
      [secondChildSession.id, secondChildTranscript],
      [grandchildSession.id, grandchildTranscript],
      [resetRootSession.id, resetRootTranscript],
    ]) {
      await waitFor(async () => {
        const messages = (await client.session.messages({ sessionID })).data
        return JSON.stringify(messages).includes(`(hya dev provider) You said: \\"${marker}\\"`)
      }, `${marker} fixture`)
    }
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: scrollChildSession.id })).data
      const value = JSON.stringify(messages)
      return value.includes("(hya dev provider) You said") && value.includes(scrollChildTranscript) && value.includes(scrollChildTail)
    }, "observation scroll fixture")

    const caseID = `child-observation-${columns}`
    const phaseTrace: Array<{
      at: number
      callsite: string
      caseID: string
      detail?: string
      phase: string
    }> = []
    let lastSuccessfulPhase: (typeof phaseTrace)[number] | undefined
    let activeCallsite = "fixture"
    const tracePhase = (callsite: string, phase: string, detail?: string) => {
      const entry = { at: Number(performance.now().toFixed(3)), callsite, caseID, detail, phase }
      phaseTrace.push(entry)
      if (phaseTrace.length > 64) phaseTrace.shift()
      if (phase === "backend_request_observed" || phase === "flush_completed" || phase.endsWith("_observed")) {
        lastSuccessfulPhase = entry
      }
    }
    const requests: Array<{ method: string; path: string }> = []
    let treeUnavailable = false
    const escapeKey = "\x1b[27;1;27~"
    const proxy = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      idleTimeout: 30,
      async fetch(request) {
        const incoming = new URL(request.url)
        requests.push({ method: request.method, path: incoming.pathname })
        tracePhase(
          activeCallsite,
          "backend_request_observed",
          `${request.method} ${incoming.pathname}${request.headers.get("x-request-id") ? ` request=${request.headers.get("x-request-id")}` : ""}`,
        )
        if (request.method === "GET" && incoming.pathname === `/session/${childSession.id}/tree`) {
          return new Response("unavailable", { status: 503 })
        }
        if (request.method === "GET" && incoming.pathname === `/session/${resetRootSession.id}/tree`) {
          return Response.json({ session: resetRootSession.id, agent: "build", children: [] })
        }
        if (
          request.method === "GET" &&
          incoming.pathname === `/session/${rootSession.id}/tree`
        ) {
          if (treeUnavailable) return new Response("unavailable", { status: 503 })
          return Response.json({
            session: rootSession.id,
            agent: "build",
            children: [
              {
                session: childSession.id,
                member: {
                  member: "member-worker",
                  child: childSession.id,
                  subagent_type: "explore",
                  description: "Inspect worker path",
                  depth: 1,
                  status: "running",
                  summary: "",
                },
                roster: {
                  handle: "worker-1",
                  session: childSession.id,
                  agent_type: "explore",
                  mode: "transient",
                  status: "busy",
                  current_task: "Inspect worker path",
                },
                children: [
                  {
                    session: grandchildSession.id,
                    member: {
                      member: "member-researcher",
                      child: grandchildSession.id,
                      subagent_type: "research",
                      description: "Trace nested path",
                      depth: 2,
                      status: "running",
                      summary: "",
                    },
                    roster: {
                      handle: "researcher-1",
                      session: grandchildSession.id,
                      agent_type: "research",
                      mode: "transient",
                      status: "busy",
                      current_task: "Trace nested path",
                    },
                  },
                ],
              },
              {
                member: {
                  member: "member-pending",
                  subagent_type: "plan",
                  description: "Waiting for slot",
                  depth: 1,
                  status: "spawning",
                  summary: "",
                },
              },
              {
                session: secondChildSession.id,
                member: {
                  member: "member-reviewer",
                  child: secondChildSession.id,
                  subagent_type: "review",
                  description: "Review changes",
                  depth: 1,
                  status: "running",
                  summary: "",
                },
                roster: {
                  handle: "reviewer-1",
                  session: secondChildSession.id,
                  agent_type: "review",
                  mode: "transient",
                  status: "idle",
                },
              },
              {
                session: scrollChildSession.id,
                member: {
                  member: "member-scroll",
                  child: scrollChildSession.id,
                  subagent_type: "explore",
                  description: "Inspect scrolling",
                  depth: 1,
                  status: "running",
                  summary: "",
                },
                roster: {
                  handle: "scroll-1",
                  session: scrollChildSession.id,
                  agent_type: "explore",
                  mode: "transient",
                  status: "busy",
                  current_task: "Inspect scrolling",
                },
              },
            ],
          })
        }
        const headers = new Headers(request.headers)
        headers.delete("host")
        const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer()
        const response = await fetch(new URL(incoming.pathname + incoming.search, url), {
          method: request.method,
          headers,
          body,
          redirect: "manual",
        })
        if (request.method === "GET" && incoming.pathname === `/session/${rootSession.id}/message` && response.ok) {
          const messages = (await response.json()) as Array<{ info: { id: string; role: string }; parts: unknown[] }>
          const assistant = messages.findLast((message) => message.info.role === "assistant")
          assistant?.parts.push({
            id: "pty-task-part",
            sessionID: rootSession.id,
            messageID: assistant.info.id,
            type: "tool",
            callID: "pty-task-call",
            tool: "task",
            state: {
              status: "completed",
              input: { description: "Inspect worker path", subagent_type: "explore" },
              output: "",
              title: "",
              metadata: { sessionId: childSession.id },
              time: { start: Date.now(), end: Date.now() },
            },
          })
          return Response.json(messages)
        }
        return response
      },
    })

    try {
      if (columns === 80) {
        const recoveryTranscript = path.join(temp, "typescript-child-recovery")
        const recovery = Bun.spawn(
          [
            "/usr/bin/script",
            "-q",
            "-e",
            "-f",
            "-c",
            `stty rows 30 cols ${columns}; "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_CHILD_SESSION"`,
            recoveryTranscript,
          ],
          {
            cwd: path.join(root, "packages/hya-tui-ts"),
            env: {
              ...env,
              HYA_PTY_PROJECT: project,
              HYA_PTY_URL: `http://127.0.0.1:${proxy.port}`,
              HYA_CHILD_SESSION: childSession.id,
              HYA_TS: launcher,
              HYA_TUI_TS_DIR: path.join(root, "packages/hya-tui-ts"),
              TERM: "xterm-256color",
            },
            stdin: "pipe",
            stdout: "ignore",
            stderr: "pipe",
          },
        )
        try {
          const recoveryOutput = async () => stripAnsi(await readFile(recoveryTranscript, "utf8").catch(() => ""))
          await waitFor(async () => {
            const frame = await recoveryOutput()
            return frame.includes(childTranscript) && frame.includes("Subagent tree unavailable")
          }, "child read-only recovery")
          const revertPath = `/session/${childSession.id}/revert`
          const revertsBefore = requests.filter((request) => request.path === revertPath).length
          await writeSemanticInput(recovery.stdin, "\x18")
          await Bun.sleep(100)
          await writeSemanticInput(recovery.stdin, "u")
          await Bun.sleep(300)
          expect(requests.filter((request) => request.path === revertPath)).toHaveLength(revertsBefore)
          await writeSemanticInput(recovery.stdin, "\x03")
          recovery.stdin.end()
          expect(await recovery.exited).toBe(0)
        } finally {
          recovery.kill()
          await Promise.race([recovery.exited, Bun.sleep(2_000).then(() => recovery.kill(9))])
        }
      }

      const process = Bun.spawn(
        [
          "/usr/bin/script",
          "-q",
          "-e",
          "-f",
          "-c",
          `stty rows 30 cols ${columns}; "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_ROOT_SESSION"`,
          transcript,
        ],
        {
          cwd: path.join(root, "packages/hya-tui-ts"),
          env: {
            ...env,
            HYA_PTY_PROJECT: project,
            HYA_PTY_URL: `http://127.0.0.1:${proxy.port}`,
            HYA_ROOT_SESSION: rootSession.id,
            HYA_TS: launcher,
            HYA_TUI_TS_DIR: path.join(root, "packages/hya-tui-ts"),
            TERM: "xterm-256color",
          },
          stdin: "pipe",
          stdout: "ignore",
          stderr: "pipe",
        },
      )

      try {
        const output = async () => stripAnsi(await readFile(transcript, "utf8").catch(() => ""))
        const writeInput = (value: string) => writeSemanticInput(process.stdin, value)
        const waitFailure = async (callsite: string, start: number, error: unknown) => {
          tracePhase(callsite, "wait_timeout")
          const transcriptRaw = await readFile(transcript, "utf8").catch(() => "")
          const lastFrame = stripAnsi(transcriptRaw).slice(start).slice(-5000)
          return new Error(
            [
              `${callsite}: ${error instanceof Error ? error.message : error}`,
              `current_phase=${JSON.stringify(phaseTrace.at(-1))}`,
              `last_successful_phase=${JSON.stringify(lastSuccessfulPhase)}`,
              `last_frame=${JSON.stringify(lastFrame)}`,
              `phase_trace=${JSON.stringify(phaseTrace)}`,
            ].join("\n"),
          )
        }
        const rootCallsite = `root-session-frame/${columns}`
        const previousRootCallsite = activeCallsite
        activeCallsite = rootCallsite
        tracePhase(rootCallsite, "phase_start")
        try {
          tracePhase(rootCallsite, "ui_state_wait", "root-session-ready")
          await waitFor(async () => rootSessionFrameReady(await output()), rootCallsite)
          const observed = await output()
          tracePhase(
            rootCallsite,
            "ui_state_observed",
            observed.includes(rootTranscript) ? rootTranscript : "task-chrome-ready",
          )
        } catch (error) {
          throw await waitFailure(rootCallsite, 0, error)
        } finally {
          activeCallsite = previousRootCallsite
        }
        await waitFor(async () => (await output()).includes("commands"), "root prompt")
        expect(requests.filter((request) => request.method === "GET" && request.path === `/session/${rootSession.id}/tree`)).toHaveLength(1)

        const rootDraft = "ROOT_DRAFT_c281"
        const beforeDraft = (await output()).length
        await waitFor(async () => {
          const frame = (await output()).slice(beforeDraft)
          if (frame.includes(rootDraft)) return true
          await writeInput(rootDraft)
          return false
        }, "root draft").catch(async (error) => {
          const frame = (await output()).slice(-5000)
          throw new Error(`${error instanceof Error ? error.message : error}\n${frame}`)
        })
        const waitForMain = async (start: number, message: string) => {
          await waitFor(async () => (await output()).slice(start).includes(rootDraft), message)
        }
        const focusMain = async (start: number, label: string) => {
          await writeInput(escapeKey)
          await waitForMain(start, `${label} Main focus`)
        }
        const confirmMainInput = async (start: number, marker: string) => {
          try {
            await writeInput(escapeKey)
            await writeInput(marker)
            await waitFor(async () => {
              const frame = (await output()).slice(start)
              return frame.includes(marker) && frame.includes(rootDraft)
            }, `${marker} in Main`)
          } catch (error) {
            const frame = (await output()).slice(start)
            const outcome = !frame.includes(marker)
              ? "CONFIRM_MAIN_MARKER_MISSING"
              : !frame.includes(rootDraft)
                ? "CONFIRM_MAIN_DRAFT_MISSING"
                : "CONFIRM_MAIN_ORACLE_TIMEOUT"
            throw new Error(`${outcome}: ${error instanceof Error ? error.message : error}\n${frame.slice(-5000)}`)
          }
        }
        const rootFrame = await output()
        expect(rootFrame).toContain("ctrl+x o")
        expect(rootFrame).toContain("subagent roster")
        expect(rootFrame).not.toContain("view subagents")
        const descendantRoutes = new Set([
          childSession.id,
          grandchildSession.id,
          secondChildSession.id,
          scrollChildSession.id,
        ])
        const descendantGets = () =>
          requests.filter(
            (request) =>
              request.method === "GET" &&
              [...descendantRoutes].some((sessionID) => request.path === `/session/${sessionID}`),
          ).length
        const descendantGetsBefore = descendantGets()
        for (const key of ["\x1b[B", "\x1b[C", "\x1b[D", "\x1b[A"]) {
          await writeInput("\x18")
          await Bun.sleep(100)
          await writeInput(key)
          await Bun.sleep(100)
        }
        const legacySafe = "_LEGACY_SAFE_0eb1"
        const legacyStart = (await output()).length
        await writeInput(legacySafe)
        await waitFor(async () => (await output()).slice(legacyStart).includes(legacySafe), "legacy commands leave Main editable")
        expect(descendantGets()).toBe(descendantGetsBefore)

        const checkRetainedTreeError = columns === 80
        treeUnavailable = checkRetainedTreeError
        const failedRefreshCount = requests.filter(
          (request) => request.method === "GET" && request.path === `/session/${rootSession.id}/tree`,
        ).length
        await writeInput("\x18")
        await Bun.sleep(100)
        const managerStart = (await output()).length
        await writeInput("o")
        await waitFor(async () => {
          const frame = (await output()).slice(managerStart)
          return ["Subagent roster", "worker-1", "researcher-1", "pending", "reviewer-1", "Waiting for slot"].every((value) =>
            frame.includes(value),
          )
        }, "recursive subagent manager").catch(async (error) => {
          const frame = (await output()).slice(managerStart).slice(-5000)
          throw new Error(`${error instanceof Error ? error.message : error}\n${frame}`)
        })
        if (checkRetainedTreeError) {
          await waitFor(
            () =>
              requests.filter(
                (request) => request.method === "GET" && request.path === `/session/${rootSession.id}/tree`,
              ).length ===
              failedRefreshCount + 1,
            "failed retained-tree refresh",
          )
          await waitFor(
            async () => (await output()).slice(managerStart).includes("Subagent tree unavailable"),
            "retained-tree error row",
          )
          treeUnavailable = false
          await writeInput("r")
          await waitFor(
            () =>
              requests.filter(
                (request) => request.method === "GET" && request.path === `/session/${rootSession.id}/tree`,
              ).length ===
              failedRefreshCount + 2,
            "retained-tree retry",
          )
        }
        const managerFrame = (await output()).slice(managerStart)
        expect(managerFrame.indexOf("worker-1")).toBeLessThan(managerFrame.indexOf("researcher-1"))
        expect(managerFrame.indexOf("researcher-1")).toBeLessThan(managerFrame.indexOf("pending"))
        expect(managerFrame.indexOf("pending")).toBeLessThan(managerFrame.indexOf("reviewer-1"))
        await writeInput("/")
        await Bun.sleep(100)
        await writeInput("researcher-1")
        await waitFor(async () => (await output()).slice(managerStart).includes("researcher-1"), "filtered grandchild")
        await writeInput(escapeKey)
        await Bun.sleep(100)
        const closeFilteredManagerStart = (await output()).length
        await writeInput(escapeKey)
        await waitForMain(closeFilteredManagerStart, "Main after filtered manager")

        for (const [command, placement] of [
          ["Open subagent in tab", "Tab"],
          ["Open subagent in vertical split", "Vertical"],
          ["Open subagent in horizontal split", "Horizontal"],
        ]) {
          await writeInput("\x10")
          await Bun.sleep(100)
          await writeInput(command)
          await Bun.sleep(100)
          const directStart = (await output()).length
          await writeInput("\r")
          await waitFor(
            async () => (await output()).slice(directStart).includes(`Subagent roster - ${placement}`),
            `${placement} placement manager`,
          ).catch(async (error) => {
            const frame = (await output()).slice(directStart).slice(-5000)
            throw new Error(`direct placement failed: ${error instanceof Error ? error.message : error}\n${frame}`)
          })
          const closePlacementManagerStart = (await output()).length
          await writeInput(escapeKey)
          await waitForMain(closePlacementManagerStart, `Main after ${placement} manager`)
        }

        const hydrationPaths = [
          `/session/${grandchildSession.id}`,
          `/session/${grandchildSession.id}/message`,
          `/session/${grandchildSession.id}/todo`,
          `/session/${grandchildSession.id}/diff`,
        ]
        const waitForFocusedHeader = async (start: number, handle: string, callsite: string) => {
          try {
            await waitFor(async () => {
              const frame = (await output()).slice(start)
              return frame.includes(handle) && frame.includes("focused") && /read-\s*only/.test(frame)
            }, callsite)
          } catch (error) {
            tracePhase(callsite, "final_render_missing")
            const transcriptRaw = await readFile(transcript, "utf8").catch(() => "")
            const lastFrame = stripAnsi(transcriptRaw).slice(start).slice(-5000)
            const children = [
              {
                exitCode: server.exitCode,
                killed: server.killed,
                name: "backend",
                pid: server.pid,
                signalCode: server.signalCode,
              },
              {
                exitCode: process.exitCode,
                killed: process.killed,
                name: "pty",
                pid: process.pid,
                signalCode: process.signalCode,
              },
            ]
            throw new Error(
              [
                `${callsite}: ${error instanceof Error ? error.message : error}`,
                `last_frame=${JSON.stringify(lastFrame)}`,
                `transcript_tail=${JSON.stringify(transcriptRaw.slice(-2000))}`,
                `children=${JSON.stringify(children)}`,
                `phase_trace=${JSON.stringify(phaseTrace)}`,
              ].join("\n"),
            )
          }
        }
        const openSubagentByHandle = async (handle: string) => {
          const callsite = `open-by-handle/${handle}-focused-header`
          const previousCallsite = activeCallsite
          activeCallsite = callsite
          tracePhase(callsite, "phase_start")
          try {
            const treePath = `/session/${rootSession.id}/tree`
            const treeRequestsBefore = requests.filter(
              (request) => request.method === "GET" && request.path === treePath,
            ).length
            const rosterStart = (await output()).length
            tracePhase(callsite, "semantic_write", "ctrl+x o")
            await writeInput("\x18")
            await writeInput("o")
            tracePhase(callsite, "flush_completed", "ctrl+x o")
            await waitFor(
              () =>
                requests.filter((request) => request.method === "GET" && request.path === treePath).length >
                treeRequestsBefore,
              `${callsite}/backend-request`,
            )
            await waitFor(async () => {
              const frame = (await output()).slice(rosterStart)
              return frame.includes("Subagent roster") && frame.includes(handle)
            }, `${callsite}/roster-visible`)
            tracePhase(callsite, "ui_state_observed", `${handle} listed`)

            const filterStart = (await output()).length
            tracePhase(callsite, "semantic_write", `filter ${handle}`)
            await writeInput("/")
            await writeInput(handle)
            tracePhase(callsite, "flush_completed", `filter ${handle}`)
            await waitFor(
              async () => (await output()).slice(filterStart).includes(handle),
              `${callsite}/child-openable`,
            )
            tracePhase(callsite, "ui_state_observed", `${handle} openable`)

            const focusStart = (await output()).length
            tracePhase(callsite, "focus_write", "enter")
            await writeInput("\r")
            tracePhase(callsite, "flush_completed", "enter")
            await waitForFocusedHeader(focusStart, handle, callsite)
            tracePhase(callsite, "final_render_observed")
          } finally {
            activeCallsite = previousCallsite
          }
        }
        const openGrandchild = () => openSubagentByHandle("researcher-1")
        await writeInput("\x18")
        await Bun.sleep(100)
        await writeInput("o")
        await Bun.sleep(100)
        await writeInput("/")
        await Bun.sleep(100)
        await writeInput("scroll-1")
        await Bun.sleep(100)
        const scrollPaneStart = (await output()).length
        await writeInput("\r")
        await waitFor(
          async () => (await output()).slice(scrollPaneStart).includes(scrollChildTail),
          "tall observation transcript",
        )
        await waitForFocusedHeader(scrollPaneStart, "scroll-1", "open-scroll/scroll-1-focused-header")
        await Bun.sleep(100)
        const scrollTopStart = (await output()).length
        await writeInput("\x1b[H")
        await waitFor(
          async () => (await output()).slice(scrollTopStart).includes(scrollChildTranscript),
          "focused observation scroll to first message",
        )
        const scrollBottomStart = (await output()).length
        await writeInput("\x1b[F")
        await waitFor(
          async () => (await output()).slice(scrollBottomStart).includes(scrollChildTail),
          "focused observation scroll to last message",
        )
        const closeScrollStart = (await output()).length
        await confirmMainInput(closeScrollStart, "m62d1")

        const observationStart = (await output()).length
        await openGrandchild()
        await waitFor(
          () => hydrationPaths.every((path) => requests.filter((request) => request.method === "GET" && request.path === path).length === 1),
          "grandchild hydration",
        )
        await waitFor(async () => {
          const frame = (await output()).slice(observationStart)
          return frame.includes(grandchildTranscript) && frame.includes("researcher-1") && /read-\s*only/i.test(frame)
        }, "grandchild observation transcript")
        await openGrandchild()
        for (const path of hydrationPaths) {
          expect(requests.filter((request) => request.method === "GET" && request.path === path)).toHaveLength(1)
        }

        const observationSentinel = "OBSERVATION_INPUT_639a"
        const observationPromptRequests = requests.filter(
          (request) => request.method === "POST" && /\/session\/[^/]+\/(?:message|prompt_async)$/.test(request.path),
        ).length
        await writeInput(observationSentinel)
        await writeInput("\r")
        await Bun.sleep(300)
        expect(
          requests.filter((request) => request.method === "POST" && /\/session\/[^/]+\/(?:message|prompt_async)$/.test(request.path)),
        ).toHaveLength(observationPromptRequests)
        const permissionCommand = "printf nested > nested-permission.txt"
        const permissionStart = (await output()).length
        const shell = client.session.shell(
          { sessionID: grandchildSession.id, command: permissionCommand },
          { throwOnError: true },
        )
        void shell.catch(() => {})
        await waitFor(async () => (await client.permission.list({}, { throwOnError: true })).data?.length === 1, "grandchild permission")
        await Bun.sleep(200)
        expect((await output()).slice(permissionStart)).not.toContain("Permission required")
        const pendingBeforeEscape = (await client.permission.list({}, { throwOnError: true })).data ?? []
        expect(pendingBeforeEscape).toHaveLength(1)
        const pendingPermission = pendingBeforeEscape[0]
        if (!pendingPermission) throw new Error("pending permission disappeared before Escape")
        const pendingPermissionID = pendingPermission.id
        const permissionReplyPath = `/permission/${encodeURIComponent(pendingPermissionID)}/reply`
        const permissionRequestCursor = requests.length
        const permissionOutputCursor = (await output()).length
        const focusMainStart = permissionOutputCursor
        const permissionCallsite = `grandchild-permission-in-main/${columns}`
        const previousPermissionCallsite = activeCallsite
        activeCallsite = permissionCallsite
        tracePhase(permissionCallsite, "phase_start")
        tracePhase(
          permissionCallsite,
          "pending_permission_locked",
          JSON.stringify({ outputCursor: permissionOutputCursor, permissionID: pendingPermissionID, requestCursor: permissionRequestCursor }),
        )
        try {
          tracePhase(permissionCallsite, "focus_write", "escape")
          await writeInput(escapeKey)
          tracePhase(permissionCallsite, "flush_completed", "escape")
          await waitFor(
            async () => {
              const frame = (await output()).slice(permissionOutputCursor)
              const promptRendered = frame.includes("Permission required")
              const matchingReplyRequests = requests
                .slice(permissionRequestCursor)
                .filter((request) => request.method === "POST" && request.path === permissionReplyPath)
              const pendingNow = (await client.permission.list({}, { throwOnError: true })).data ?? []
              const permissionStillPending = pendingNow.some((permission) => permission.id === pendingPermissionID)

              if (promptRendered) {
                if (!permissionStillPending) {
                  throw new Error(
                    [
                      "PERMISSION_RENDERED_WITHOUT_PENDING_REQUEST",
                      `permission_id=${JSON.stringify(pendingPermissionID)}`,
                      `matching_requests=${JSON.stringify(matchingReplyRequests)}`,
                      `callsite=${permissionCallsite}`,
                      `phase=${JSON.stringify(phaseTrace.at(-1))}`,
                      `last_frame=${JSON.stringify(frame.slice(-5000))}`,
                    ].join("\n"),
                  )
                }
                return true
              }

              if (matchingReplyRequests.length > 0 || !permissionStillPending) {
                tracePhase(
                  permissionCallsite,
                  "escape_propagated_to_new_permission_prompt",
                  JSON.stringify({ matchingReplyRequests, permissionID: pendingPermissionID, permissionStillPending }),
                )
                throw new Error(
                  [
                    "ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT",
                    `permission_id=${JSON.stringify(pendingPermissionID)}`,
                    `matching_requests=${JSON.stringify(matchingReplyRequests)}`,
                    `permission_still_pending=${permissionStillPending}`,
                    `callsite=${permissionCallsite}`,
                    `phase=${JSON.stringify(phaseTrace.at(-1))}`,
                    `last_frame=${JSON.stringify(frame.slice(-5000))}`,
                  ].join("\n"),
                )
              }
              return false
            },
            permissionCallsite,
          )
          tracePhase(permissionCallsite, "final_render_observed", "Permission required")
        } catch (error) {
          if (error instanceof Error && error.message.startsWith("ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT")) {
            throw error
          }
          if (error instanceof Error && error.message.startsWith("PERMISSION_RENDERED_WITHOUT_PENDING_REQUEST")) {
            throw error
          }
          const matchingReplyRequests = requests
            .slice(permissionRequestCursor)
            .filter((request) => request.method === "POST" && request.path === permissionReplyPath)
          const pendingAfterFailure = (await client.permission.list({}, { throwOnError: true })).data ?? []
          const permissionStillPending = pendingAfterFailure.some((permission) => permission.id === pendingPermissionID)
          if (matchingReplyRequests.length === 0 && permissionStillPending) {
            tracePhase(permissionCallsite, "event_propagation_hypothesis_disproven", pendingPermissionID)
            throw new Error(
              [
                "EVENT_PROPAGATION_HYPOTHESIS_DISPROVEN",
                `permission_id=${JSON.stringify(pendingPermissionID)}`,
                `matching_requests=${JSON.stringify(matchingReplyRequests)}`,
                `permission_still_pending=${permissionStillPending}`,
                `callsite=${permissionCallsite}`,
                `phase=${JSON.stringify(phaseTrace.at(-1))}`,
                `last_frame=${JSON.stringify((await output()).slice(permissionOutputCursor).slice(-5000))}`,
              ].join("\n"),
            )
          }
          throw await waitFailure(permissionCallsite, focusMainStart, error)
        } finally {
          activeCallsite = previousPermissionCallsite
        }
        await writeInput("\r")
        await shell
        await waitFor(async () => (await output()).slice(focusMainStart).includes(rootDraft), "focus Main with preserved draft")
        const observationRootEvents = await (await fetch(`${url}/sessions/${rootSession.id}/events`)).text()
        const observationChildEvents = await (await fetch(`${url}/sessions/${grandchildSession.id}/events`)).text()
        expect(observationRootEvents).not.toContain(observationSentinel)
        expect(observationChildEvents).not.toContain(observationSentinel)

        const splitStart = (await output()).length
        await writeInput("\x18")
        await Bun.sleep(100)
        await writeInput("o")
        await Bun.sleep(200)
        await writeInput("v")
        await Bun.sleep(200)
        await writeInput("\x18")
        await Bun.sleep(100)
        await writeInput("o")
        await Bun.sleep(200)
        await writeInput("\x1b[B")
        await Bun.sleep(100)
        await writeInput("s")
        await waitFor(async () => {
          const frame = (await output()).slice(splitStart)
          return (
            frame.includes(childTranscript) &&
            frame.includes("printf nested") &&
            frame.includes("permission.txt") &&
            frame.includes("worker-1") &&
            frame.includes("researcher-1")
          )
        }, "live recursive split transcripts").catch(async (error) => {
          const frame = (await output()).slice(splitStart).slice(-5000)
          throw new Error(`${error instanceof Error ? error.message : error}\n${frame}`)
        })
        const workerLate = "WORKER_LATE_83bc"
        const researcherLate = "RESEARCHER_LATE_27ad"
        const redrawStart = (await output()).length
        await client.session.promptAsync(
          { sessionID: childSession.id, parts: [{ type: "text", text: workerLate }] },
          { throwOnError: true },
        )
        await client.session.promptAsync(
          { sessionID: grandchildSession.id, parts: [{ type: "text", text: researcherLate }] },
          { throwOnError: true },
        )
        await waitFor(async () => {
          const [worker, researcher] = await Promise.all([
            client.session.messages({ sessionID: childSession.id }),
            client.session.messages({ sessionID: grandchildSession.id }),
          ])
          return JSON.stringify(worker.data).includes(workerLate) && JSON.stringify(researcher.data).includes(researcherLate)
        }, "late observation messages")
        // ADR-0003 paints Main + one observation; other open agents stay as tabs.
        // Focus each late pane explicitly so markers paint regardless of cycle order.
        for (const [handle, marker] of [
          ["worker-1", workerLate],
          ["researcher-1", researcherLate],
        ] as const) {
          await openSubagentByHandle(handle)
          await waitFor(
            async () => (await output()).slice(redrawStart).includes(marker),
            `late ${handle} redraw`,
          ).catch(async (error) => {
            const frame = (await output()).slice(redrawStart).slice(-5000)
            throw new Error(`${error instanceof Error ? error.message : error}\n${frame}`)
          })
        }
        const reviewerTabStart = (await output()).length
        await openSubagentByHandle("reviewer-1")
        await waitFor(
          async () => (await output()).slice(reviewerTabStart).includes(secondChildTranscript),
          "auxiliary reviewer tab",
        )

        const workerCallsite = "ctrl-x-dot/worker-1-focused-header"
        const previousCallsite = activeCallsite
        activeCallsite = workerCallsite
        tracePhase(workerCallsite, "phase_start")
        try {
          await openGrandchild()
          tracePhase(workerCallsite, "ui_state_observed", "researcher-1 focused predecessor")
          await waitFor(
            async () => (await output()).slice(-5000).includes("worker-1"),
            `${workerCallsite}/worker-listed`,
          )
          tracePhase(workerCallsite, "ui_state_observed", "worker-1 listed")

          const workerFocusStart = (await output()).length
          tracePhase(workerCallsite, "focus_write", "ctrl+x .")
          await writeInput("\x18")
          await writeInput(".")
          tracePhase(workerCallsite, "flush_completed", "ctrl+x .")
          await waitForFocusedHeader(workerFocusStart, "worker-1", workerCallsite)
          tracePhase(workerCallsite, "final_render_observed")
        } finally {
          activeCallsite = previousCallsite
        }
        const closeWorkerStart = (await output()).length
        await confirmMainInput(closeWorkerStart, "m59e0")
        const researcherFocusStart = (await output()).length
        await openGrandchild()
        await waitForFocusedHeader(
          researcherFocusStart,
          "researcher-1",
          "open-by-handle/researcher-1-secondary-focused-header",
        )
        await waitFor(
          async () => {
            const frame = (await output()).slice(researcherFocusStart)
            return (
              ["researcher-1", "research", "Working", "Trace nested path", "focused"].every((value) =>
                frame.includes(value),
              ) &&
              /read-\s*only/.test(frame) &&
              ["tab", "vertical", "horizontal"].some((placement) => frame.includes(placement))
            )
          },
          "focused observation header",
        ).catch(async (error) => {
          const frame = (await output()).slice(researcherFocusStart).slice(-5000)
          throw new Error(`${error instanceof Error ? error.message : error}\n${frame}`)
        })
        const collapsedMainStart = (await output()).length
        await confirmMainInput(collapsedMainStart, "m1c54")
        const reviewerCycleStart = (await output()).length
        await openSubagentByHandle("reviewer-1")
        await waitForFocusedHeader(
          reviewerCycleStart,
          "reviewer-1",
          "open-by-handle/reviewer-1-secondary-focused-header",
        )
        expect((await output()).slice(reviewerCycleStart)).toContain(secondChildTranscript)
        const closeTabStart = (await output()).length
        await confirmMainInput(closeTabStart, "m763f")

        await writeInput("\x18")
        await Bun.sleep(100)
        await writeInput("l")
        await Bun.sleep(200)
        await writeInput("PTY reset root")
        await Bun.sleep(200)
        const resetStart = (await output()).length
        await writeInput("\r")
        await waitFor(async () => (await output()).slice(resetStart).includes(resetRootTranscript), "fresh root workspace")
        const resetFrame = (await output()).slice(resetStart)
        expect(resetFrame).not.toContain("worker-1")
        expect(resetFrame).not.toContain("researcher-1")
        expect(resetFrame).not.toContain("reviewer-1")
        expect(resetFrame).not.toContain("scroll-1")
        const resetSentinel = "RESET_ROOT_INPUT_d3c7"
        await writeInput(resetSentinel)
        await writeInput("\r")
        await waitFor(async () => {
          const events = await (await fetch(`${url}/sessions/${resetRootSession.id}/events`)).text()
          return events.includes(resetSentinel)
        }, "root B submission")

        await writeInput("\x03")
        process.stdin.end()
        const status = await Promise.race([
          process.exited,
          Bun.sleep(15_000).then(() => {
            process.kill(9)
            throw new Error("PTY child observation timed out")
          }),
        ])
        const rootEvents = await (await fetch(`${url}/sessions/${rootSession.id}/events`)).text()

        expect(status).toBe(0)
        expect(rootEvents).not.toContain(rootDraft)
        expect(rootFrame).toContain(rootTranscript)
      } finally {
        process.kill()
        await Promise.race([process.exited, Bun.sleep(2_000).then(() => process.kill(9))])
      }
    } finally {
      proxy.stop(true)
    }
  } finally {
    server.kill()
    await Promise.race([server.exited, Bun.sleep(2_000).then(() => server.kill(9))])
    await rm(temp, { recursive: true, force: true })
  }
}

for (const columns of [80, 140]) {
  test(`Linux PTY ${columns}-column subagent workspace`, () => runChildObservation(columns), Bun.env.CI ? 120_000 : 60_000)
}
