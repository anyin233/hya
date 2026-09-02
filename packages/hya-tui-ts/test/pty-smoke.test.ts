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

type KillableProcess = {
  exited: Promise<number>
  kill(signal?: number): void
}

/** Stop one owned child and wait until the operating system reports its exit. */
async function stopOwnedProcess(process: KillableProcess) {
  process.kill()
  const exited = await Promise.race([
    process.exited.then(() => true),
    Bun.sleep(2_000).then(() => false),
  ])
  if (exited) return
  process.kill(9)
  await process.exited
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

    const marker = "No live provider is available. Configure a provider to continue."
    const deadline = Date.now() + 10_000
    while (!stripAnsi(await readFile(transcript, "utf8").catch(() => "")).includes(marker)) {
      const exited = await Promise.race([
        process.exited.then((status) => ({ status })),
        Bun.sleep(100).then(() => undefined),
      ])
      if (exited) throw new Error(`PTY exited before rendering the response with status ${exited.status}`)
      if (Date.now() >= deadline) {
        process.kill(9)
        throw new Error(`PTY did not render the response before timeout:\n${stripAnsi(await readFile(transcript, "utf8").catch(() => "")).slice(-4_000)}`)
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

test("Linux PTY shows backend-discovered rows and auth-required offline status", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-pty-catalog-")))
  const project = path.join(temp, "project")
  const transcript = path.join(temp, "typescript-catalog")
  const offlineTranscript = path.join(temp, "typescript-offline")
  const home = path.join(temp, "home")
  const config = path.join(temp, "config")
  await mkdir(project)
  await mkdir(home)
  await mkdir(path.join(config, "hya"), { recursive: true })

  let catalogRequests = 0
  let catalogMode: "models" | "auth_required" = "models"
  const provider = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      if (new URL(request.url).pathname !== "/v1/models") {
        return new Response("not found", { status: 404 })
      }
      catalogRequests++
      if (catalogMode === "auth_required") return new Response("auth required", { status: 401 })
      return new Response(JSON.stringify({ object: "list", data: [{ id: "discovered-pty" }] }), {
        headers: { "content-type": "application/json" },
      })
    },
  })

  await Bun.write(
    path.join(config, "hya", "config.yaml"),
    [
      "default_model: fixture/discovered-pty",
      "providers:",
      "  fixture:",
      "    kind: openai",
      `    base_url: http://127.0.0.1:${provider.port}/v1`,
      "    models: []",
      "permission:",
      "  model: allow",
      "  rules: []",
      "",
    ].join("\n"),
  )

  const env = {
    PATH: Bun.env.PATH ?? "/usr/local/bin:/usr/bin:/bin",
    LANG: Bun.env.LANG ?? "C.UTF-8",
    HOME: home,
    XDG_CACHE_HOME: path.join(temp, "cache"),
    XDG_CONFIG_HOME: config,
    XDG_DATA_HOME: path.join(temp, "data"),
    XDG_STATE_HOME: path.join(temp, "state"),
  }
  let backendProcess: Bun.Subprocess | undefined
  let tuiProcess: Bun.WritableSubprocess | undefined

  /** Start a backend process and wait for its actual listening URL. */
  const startBackend = async () => {
    const process = Bun.spawn(
      [backend, "--yolo", "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"],
      {
        cwd: project,
        env,
        stdout: "pipe",
        stderr: "pipe",
      },
    )
    backendProcess = process
    const reader = process.stdout.getReader()
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
      // Backend readiness is external process I/O; fake timers cannot advance child output.
      Bun.sleep(10_000).then(() => {
        throw new Error(`timed out waiting for hya-backend: ${readiness}`)
      }),
    ])
    return { process, url }
  }

  /** Start the real launcher under `/usr/bin/script` so PTY bytes are recorded. */
  const startTui = (url: string, sessionID: string, outputPath: string) => {
    const process = Bun.spawn(
      [
        "/usr/bin/script",
        "-q",
        "-e",
        "-f",
        "-c",
        'stty rows 30 cols 100; before=$(stty -g); before_fg=$(ps -o tpgid= -p $$ | tr -d " "); "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_PTY_SESSION"; code=$?; after=$(stty -g); after_fg=$(ps -o tpgid= -p $$ | tr -d " "); [ "$before" = "$after" ] || exit 97; [ "$before_fg" = "$after_fg" ] || exit 98; exit "$code"',
        outputPath,
      ],
      {
        cwd: path.join(root, "packages/hya-tui-ts"),
        env: {
          ...env,
          HYA_PTY_PROJECT: project,
          HYA_PTY_SESSION: sessionID,
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
    tuiProcess = process
    return process
  }

  try {
    const first = await startBackend()
    const firstBootstrap = (await (await fetch(`${first.url}/tui/bootstrap`)).json()) as {
      providers: { providers: Array<{ id: string; models: Record<string, unknown> }> }
    }
    const firstProvider = firstBootstrap.providers.providers.find((item) => item.id === "fixture")
    expect(firstProvider?.models).toHaveProperty("discovered-pty")
    expect(catalogRequests).toBe(1)

    const firstSession = (await (
      await fetch(`${first.url}/session`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title: "Discovered catalog PTY" }),
      })
    ).json()) as { id: string }
    const firstTui = startTui(first.url, firstSession.id, transcript)
    const firstOutput = async () => stripAnsi(await readFile(transcript, "utf8").catch(() => ""))
    const waitForFirst = async (check: () => boolean | Promise<boolean>, label: string) => {
      const deadline = Date.now() + 10_000
      while (!(await check())) {
        const exited = await Promise.race([
          firstTui.exited.then((status) => ({ status })),
          // PTY output is external OS I/O; fake timers cannot advance the child transcript.
          Bun.sleep(50).then(() => undefined),
        ])
        if (exited) throw new Error(`discovered PTY exited before ${label} with status ${exited.status}`)
        if (Date.now() >= deadline) {
          throw new Error(`timed out waiting for discovered ${label}:\n${(await firstOutput()).slice(-4_000)}`)
        }
      }
    }
    await waitForFirst(() => firstOutput().then((frame) => frame.includes("ctrl+p commands")), "startup")
    const firstPickerStart = (await firstOutput()).length
    await writeSemanticInput(firstTui.stdin, "/model")
    await waitForFirst(
      () => firstOutput().then((frame) => frame.slice(firstPickerStart).includes("/models")),
      "discovered model autocomplete",
    )
    await writeSemanticInput(firstTui.stdin, "\r")
    await waitForFirst(
      () => firstOutput().then((frame) => frame.slice(firstPickerStart).includes("Select model")),
      "discovered model picker",
    )
    const firstPicker = (await firstOutput()).slice(firstPickerStart)
    expect(firstPicker).toContain("discovered-pty")
    await writeSemanticInput(firstTui.stdin, "\x03")
    firstTui.stdin.end()
    expect(await firstTui.exited).toBe(0)
    tuiProcess = undefined
    await stopOwnedProcess(first.process)
    backendProcess = undefined

    catalogMode = "auth_required"
    const second = await startBackend()
    const secondBootstrap = (await (await fetch(`${second.url}/tui/bootstrap`)).json()) as {
      providers: { providers: Array<{ id: string; models: Record<string, unknown>; auth: string }> }
    }
    const authProvider = secondBootstrap.providers.providers.find((item) => item.id === "fixture")
    const offlineProvider = secondBootstrap.providers.providers.find((item) => item.id === "hya")
    expect(authProvider?.models).toEqual({})
    expect(authProvider?.auth).toBe("auth_required")
    expect(offlineProvider?.models).toHaveProperty("offline")
    expect(catalogRequests).toBe(2)

    const secondSession = (await (
      await fetch(`${second.url}/session`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title: "Auth required catalog PTY" }),
      })
    ).json()) as { id: string }
    const secondTui = startTui(second.url, secondSession.id, offlineTranscript)
    const secondOutput = async () => stripAnsi(await readFile(offlineTranscript, "utf8").catch(() => ""))
    const waitForSecond = async (check: () => boolean | Promise<boolean>, label: string) => {
      const deadline = Date.now() + 10_000
      while (!(await check())) {
        const exited = await Promise.race([
          secondTui.exited.then((status) => ({ status })),
          // PTY output is external OS I/O; fake timers cannot advance the child transcript.
          Bun.sleep(50).then(() => undefined),
        ])
        if (exited) throw new Error(`offline PTY exited before ${label} with status ${exited.status}`)
        if (Date.now() >= deadline) {
          throw new Error(`timed out waiting for offline ${label}:\n${(await secondOutput()).slice(-4_000)}`)
        }
      }
    }
    await waitForSecond(
      () => secondOutput().then((frame) => frame.includes("Authentication required") && frame.includes("ctrl+p commands")),
      "auth-required status",
    )
    const secondPickerStart = (await secondOutput()).length
    await writeSemanticInput(secondTui.stdin, "/model")
    await waitForSecond(
      () => secondOutput().then((frame) => frame.slice(secondPickerStart).includes("/models")),
      "offline model autocomplete",
    )
    await writeSemanticInput(secondTui.stdin, "\r")
    await waitForSecond(
      () => secondOutput().then((frame) => frame.slice(secondPickerStart).includes("Select model")),
      "offline model picker",
    )
    const secondPicker = (await secondOutput()).slice(secondPickerStart)
    expect(secondPicker).toContain("offline")
    expect(await secondOutput()).toContain("Authentication required")
    await writeSemanticInput(secondTui.stdin, "\x03")
    secondTui.stdin.end()
    expect(await secondTui.exited).toBe(0)
    tuiProcess = undefined
    await stopOwnedProcess(second.process)
    backendProcess = undefined
  } finally {
    if (tuiProcess) await stopOwnedProcess(tuiProcess)
    if (backendProcess) await stopOwnedProcess(backendProcess)
    provider.stop(true)
    await rm(temp, { recursive: true, force: true })
  }
}, 90_000)

test("Linux PTY exact /model and /think open local pickers without a provider round", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-pty-picker-")))
  const project = path.join(temp, "project")
  const transcript = path.join(temp, "typescript")
  const home = path.join(temp, "home")
  const config = path.join(temp, "config")
  await mkdir(project)
  await mkdir(home)
  await mkdir(path.join(config, "hya"), { recursive: true })
  await mkdir(path.join(project, ".opencode", "commands"), { recursive: true })
  await Bun.write(
    path.join(project, ".opencode", "commands", "broken.md"),
    "---\ndescription: Trigger a planned command failure\n---\nBROKEN_COMMAND $ARGUMENTS\n",
  )
  await Bun.write(
    path.join(project, ".opencode", "commands", "known.md"),
    "---\ndescription: Known command sentinel\n---\nKNOWN_V1 $ARGUMENTS\n",
  )
  await Bun.write(
    path.join(project, ".opencode", "commands", "user-other.md"),
    "---\ndescription: User command sentinel\n---\nUSER_OTHER $ARGUMENTS\n",
  )
  await Bun.write(
    path.join(project, ".opencode", "commands", "remove.md"),
    "---\ndescription: Removed command sentinel\n---\nREMOVE_V1 $ARGUMENTS\n",
  )
  const userPlaybookSkill = path.join(project, ".hya", "skills", "user-playbook", "SKILL.md")
  const addedPlaybookSkill = path.join(project, ".hya", "skills", "added-playbook", "SKILL.md")
  await mkdir(path.dirname(userPlaybookSkill), { recursive: true })
  await Bun.write(
    userPlaybookSkill,
    "---\nname: user-playbook\ndescription: Project skill sentinel\n---\nUSER_PLAYBOOK_BODY $ARGUMENTS\n",
  )

  let providerRequests = 0
  const providerBodies: string[] = []
  const provider = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      providerBodies.push(await request.text())
      providerRequests++
      if (providerRequests === 1) {
        return new Response("planned command failure", { status: 400 })
      }
      return new Response(
        [
          'data: {"type":"response.output_text.delta","output_index":0,"delta":"picker recovery reply"}',
          'data: {"type":"response.output_text.done","output_index":0}',
          'data: {"type":"response.completed","response":{"status":"completed"}}',
        ].join("\n\n") + "\n\n",
        { headers: { "content-type": "text/event-stream" } },
      )
    },
  })
  await Bun.write(
    path.join(config, "hya", "config.yaml"),
    [
      "default_model: test/gpt-picker",
      "providers:",
      "  test:",
      "    kind: openai-response",
      `    base_url: http://127.0.0.1:${provider.port}/v1`,
      "    api_key: test-token",
      "    models:",
      "      - id: gpt-picker",
      "        reasoning:",
      "          default: high",
      "          variants: [low, medium, high]",
      "permission:",
      "  model: default",
      "  rules: []",
      "",
    ].join("\n"),
  )

  const env = {
    PATH: Bun.env.PATH ?? "/usr/local/bin:/usr/bin:/bin",
    LANG: Bun.env.LANG ?? "C.UTF-8",
    HOME: home,
    XDG_CACHE_HOME: path.join(temp, "cache"),
    XDG_CONFIG_HOME: config,
    XDG_DATA_HOME: path.join(temp, "data"),
    XDG_STATE_HOME: path.join(temp, "state"),
  }
  const server = Bun.spawn([backend, "--db", path.join(temp, "sessions.db"), "serve", "--bind", "127.0.0.1:0"], {
    cwd: project,
    env,
    stdout: "pipe",
    stderr: "pipe",
  })
  let ownedProcess: Bun.WritableSubprocess | undefined

  try {
    const reader = server.stdout.getReader()
    const decoder = new TextDecoder()
    let readiness = ""
    // Backend readiness is external process I/O; this deadline only bounds a hung child.
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
    const session = (await client.session.create({ title: "Picker regression" }, { throwOnError: true })).data!
    const process = Bun.spawn(
      [
        "/usr/bin/script",
        "-q",
        "-e",
        "-f",
        "-c",
        'stty rows 30 cols 100; before=$(stty -g); before_fg=$(ps -o tpgid= -p $$ | tr -d " "); "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_PTY_SESSION"; code=$?; after=$(stty -g); after_fg=$(ps -o tpgid= -p $$ | tr -d " "); [ "$before" = "$after" ] || exit 97; [ "$before_fg" = "$after_fg" ] || exit 98; exit "$code"',
        transcript,
      ],
      {
        cwd: path.join(root, "packages/hya-tui-ts"),
        env: {
          ...env,
          HYA_PTY_PROJECT: project,
          HYA_PTY_SESSION: session.id,
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
    ownedProcess = process

    const output = async () => stripAnsi(await readFile(transcript, "utf8").catch(() => ""))
    /** Poll real PTY/process I/O; fake timers cannot advance OS output or process exit. */
    const waitFor = async (check: () => boolean | Promise<boolean>, label: string) => {
      const deadline = Date.now() + 10_000
      while (!(await check())) {
        const exited = await Promise.race([
          process.exited.then((status) => ({ status })),
          Bun.sleep(50).then(() => undefined),
        ])
        if (exited) throw new Error(`PTY exited before ${label} with status ${exited.status}`)
        if (Date.now() >= deadline) throw new Error(`timed out waiting for ${label}: ${(await output()).slice(-3000)}`)
      }
    }
    await waitFor(async () => {
      const frame = await output()
      return frame.includes("ctrl+p commands") && frame.includes("gpt-picker")
    }, "picker Session")
    type SessionEvent = {
      event?: {
        type?: string
        command?: string
        name?: string
      }
    }
    /** Read fresh canonical Session event envelopes for each assertion. */
    const sessionEvents = async (): Promise<SessionEvent[]> => {
      return (await (await fetch(`${url}/sessions/${session.id}/events`)).json()) as SessionEvent[]
    }
    /** Count canonical CommandExecuted Events for one command name. */
    const commandEventCount = async (command: string) => {
      const events = await sessionEvents()
      return events.filter((item) => item.event?.type === "command_executed" && item.event.command === command).length
    }
    /** Count canonical ToolCallRequested Events for one tool name. */
    const toolCallCount = async (name: string) => {
      const events = await sessionEvents()
      return events.filter((item) => item.event?.type === "tool_call_requested" && item.event.name === name).length
    }

    /** Report whether the backend owns no active turn for the test Session. */
    const sessionIsIdle = async () => {
      const statuses = await client.session.status({}, { throwOnError: true })
      return statuses.data?.[session.id]?.type !== "busy"
    }
    const modelCommandsBefore = await commandEventCount("model")
    const thinkCommandsBefore = await commandEventCount("think")
    const workflowCommandsBefore = await commandEventCount("workflow")
    const userPlaybookCommandsBefore = await commandEventCount("user-playbook")
    const skillToolCallsBefore = await toolCallCount("skill")
    expect(providerRequests).toBe(0)

    const paletteStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x10")
    await waitFor(async () => (await output()).slice(paletteStart).includes("Commands"), "command palette")
    await writeSemanticInput(process.stdin, "user-other")
    await waitFor(async () => (await output()).slice(paletteStart).includes("No results found"), "resource-free command palette")
    const paletteFrame = (await output()).slice(paletteStart)
    expect(paletteFrame).not.toContain("User command sentinel")
    expect(paletteFrame).not.toContain("Project skill sentinel")
    const paletteCloseStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[27;1;27~")
    await waitFor(async () => (await output()).slice(paletteCloseStart).includes("ctrl+p commands"), "closed command palette")

    const resourceSlashStart = (await output()).length
    await writeSemanticInput(process.stdin, "/user")
    await waitFor(async () => (await output()).slice(resourceSlashStart).includes("/user-other"), "custom command autocomplete")
    const resourceSlashFrame = (await output()).slice(resourceSlashStart)
    expect(resourceSlashFrame).not.toContain("/user-playbook")
    expect(resourceSlashFrame).toContain("User command sentinel")
    expect(resourceSlashFrame).not.toContain("Project skill sentinel")
    const resourceSlashCloseStart = (await readFile(transcript, "utf8")).length
    await writeSemanticInput(process.stdin, "\x1b")
    await waitFor(
      async () => (await readFile(transcript, "utf8")).length > resourceSlashCloseStart,
      "closed custom command autocomplete",
    )

    const skillsStart = (await output()).length
    await writeSemanticInput(process.stdin, "/skills")
    await waitFor(async () => (await output()).slice(skillsStart).includes("/skills"), "Skills command autocomplete")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(async () => {
      const frame = (await output()).slice(skillsStart)
      return frame.includes("Skills") && frame.includes("user-playbook") && frame.includes("Project skill sentinel")
    }, "Skills dialog")
    expect(providerRequests).toBe(0)
    expect(await commandEventCount("user-playbook")).toBe(userPlaybookCommandsBefore)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    const skillsCloseStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[27;1;27~")
    await waitFor(async () => (await output()).slice(skillsCloseStart).includes("ctrl+p commands"), "closed Skills dialog")

    const modelStart = (await output()).length
    await writeSemanticInput(process.stdin, "/model")
    await waitFor(async () => (await output()).slice(modelStart).includes("/models"), "model autocomplete")
    expect((await output()).slice(modelStart)).not.toContain("switch the active model")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(async () => (await output()).slice(modelStart).includes("Select model"), "model picker")
    expect(providerRequests).toBe(0)
    expect(await commandEventCount("model")).toBe(modelCommandsBefore)

    const modelCloseStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[27;1;27~")
    await waitFor(async () => (await output()).slice(modelCloseStart).includes("ctrl+p commands"), "closed model picker")
    const variantStart = (await output()).length
    await writeSemanticInput(process.stdin, "/think")
    await waitFor(async () => (await output()).slice(variantStart).includes("/variants"), "variant autocomplete")
    expect((await output()).slice(variantStart)).not.toContain("set reasoning effort")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(async () => (await output()).slice(variantStart).includes("Select variant"), "variant picker")
    expect(providerRequests).toBe(0)
    expect(await commandEventCount("think")).toBe(thinkCommandsBefore)

    const variantSelectStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[B\r")
    await waitFor(async () => (await output()).slice(variantSelectStart).includes("·low"), "selected low variant")
    expect(providerRequests).toBe(0)

    const workflowStart = (await output()).length
    await writeSemanticInput(process.stdin, "/workflow state")
    await waitFor(async () => (await output()).slice(workflowStart).includes("/workflow state"), "Workflow command input")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(
      async () => (await commandEventCount("workflow")) === workflowCommandsBefore + 1,
      "Workflow CommandExecuted Event",
    )
    await waitFor(sessionIsIdle, "Workflow command completion")
    expect(providerRequests).toBe(0)

    const commandStart = (await output()).length
    await writeSemanticInput(process.stdin, "/broken fail")
    await waitFor(async () => (await output()).slice(commandStart).includes("/broken fail"), "broken command input")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerRequests === 1, "broken command provider request")
    await waitFor(async () => (await output()).slice(commandStart).includes("Failed to run command"), "command error toast")
    await waitFor(sessionIsIdle, "failed command recovery")

    const historyStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[A")
    await waitFor(async () => (await output()).slice(historyStart).includes("/broken fail"), "submitted command history")
    await writeSemanticInput(process.stdin, "\x01")
    await writeSemanticInput(process.stdin, "\x0b")

    const recoveryStart = (await output()).length
    await writeSemanticInput(process.stdin, "recover after command failure")
    // script logs terminal diffs; the provider payload proves the complete submitted editor value.
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(async () => (await output()).slice(recoveryStart).includes("picker recovery reply"), "recovery reply")
    await waitFor(sessionIsIdle, "recovery prompt completion")
    expect(providerRequests).toBe(2)
    expect(providerBodies[1]).toContain("recover after command failure")
    const initialSkillIndex = providerBodies.length
    const initialSkillStart = (await output()).length
    await writeSemanticInput(process.stdin, "/user-playbook initial")
    await waitFor(
      async () => (await output()).slice(initialSkillStart).includes("/user-playbook initial"),
      "initial Skill command input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > initialSkillIndex, "initial Skill command request")
    expect(providerBodies[initialSkillIndex]).toContain("USER_PLAYBOOK_BODY initial")
    await waitFor(sessionIsIdle, "initial Skill command completion")
    expect(await commandEventCount("user-playbook")).toBe(userPlaybookCommandsBefore + 1)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    expect(providerRequests).toBe(3)

    await Bun.write(
      userPlaybookSkill,
      "---\nname: user-playbook\ndescription: Project skill sentinel\n---\nUSER_PLAYBOOK_BODY_V2 $ARGUMENTS\n",
    )
    const editedSkillIndex = providerBodies.length
    const editedSkillStart = (await output()).length
    await writeSemanticInput(process.stdin, "/user-playbook edited")
    await waitFor(
      async () => (await output()).slice(editedSkillStart).includes("/user-playbook edited"),
      "edited Skill command input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > editedSkillIndex, "edited Skill command request")
    expect(providerBodies[editedSkillIndex]).toContain("USER_PLAYBOOK_BODY_V2 edited")
    await waitFor(sessionIsIdle, "edited Skill command completion")
    expect(await commandEventCount("user-playbook")).toBe(userPlaybookCommandsBefore + 2)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    expect(providerRequests).toBe(4)

    const multilineStart = (await output()).length
    await writeSemanticInput(process.stdin, "\x1b[200~/known quoted\nsecond line\x1b[201~")
    await waitFor(async () => {
      const frame = (await output()).slice(multilineStart)
      return frame.includes("/known quoted") && frame.includes("second line")
    }, "multiline command input")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(
      () => providerBodies.some((body) => body.includes("KNOWN_V1 quoted\\nsecond line")),
      "multiline command expansion",
    )
    await waitFor(sessionIsIdle, "multiline command completion")
    expect(providerRequests).toBe(5)

    await Bun.write(
      path.join(project, ".opencode", "commands", "known.md"),
      "---\ndescription: Known command sentinel\n---\nKNOWN_V2 $ARGUMENTS\n",
    )
    const changedStart = (await output()).length
    await writeSemanticInput(process.stdin, "/known changed")
    await waitFor(async () => (await output()).slice(changedStart).includes("/known changed"), "changed command input")
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.some((body) => body.includes("KNOWN_V2 changed")), "changed command expansion")
    await waitFor(sessionIsIdle, "changed command completion")
    expect(providerRequests).toBe(6)

    await Bun.write(
      path.join(project, ".opencode", "commands", "added.md"),
      "---\ndescription: Added command sentinel\n---\nADDED_BODY $ARGUMENTS\n",
    )
    await rm(path.join(project, ".opencode", "commands", "remove.md"))

    const addedBeforeRestartIndex = providerBodies.length
    const addedBeforeRestartStart = (await output()).length
    await writeSemanticInput(process.stdin, "/added pre-restart")
    await waitFor(
      async () => (await output()).slice(addedBeforeRestartStart).includes("/added pre-restart"),
      "new command before restart input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > addedBeforeRestartIndex, "new command before restart request")
    expect(providerBodies[addedBeforeRestartIndex]).toContain("/added pre-restart")
    expect(providerBodies[addedBeforeRestartIndex]).not.toContain("ADDED_BODY")
    await waitFor(sessionIsIdle, "new command fallback completion")

    const removedBeforeRestartIndex = providerBodies.length
    const removedBeforeRestartStart = (await output()).length
    await writeSemanticInput(process.stdin, "/remove stale")
    await waitFor(
      async () => (await output()).slice(removedBeforeRestartStart).includes("/remove stale"),
      "removed command before restart input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > removedBeforeRestartIndex, "removed command fallback request")
    expect(providerBodies[removedBeforeRestartIndex]).toContain("/remove stale")
    expect(providerBodies[removedBeforeRestartIndex]).not.toContain("REMOVE_V1")
    await waitFor(sessionIsIdle, "removed command fallback completion")

    expect(await commandEventCount("remove")).toBe(1)
    expect(await commandEventCount("added")).toBe(0)
    await mkdir(path.dirname(addedPlaybookSkill), { recursive: true })
    await Bun.write(
      addedPlaybookSkill,
      "---\nname: added-playbook\ndescription: Added Skill sentinel\n---\nADDED_SKILL_BODY $ARGUMENTS\n",
    )
    await rm(userPlaybookSkill)

    const addedSkillCommandsBefore = await commandEventCount("added-playbook")
    const addedSkillBeforeRestartIndex = providerBodies.length
    const addedSkillBeforeRestartStart = (await output()).length
    await writeSemanticInput(process.stdin, "/added-playbook pre-restart")
    await waitFor(
      async () => (await output()).slice(addedSkillBeforeRestartStart).includes("/added-playbook pre-restart"),
      "new Skill before restart input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > addedSkillBeforeRestartIndex, "new Skill before restart request")
    expect(providerBodies[addedSkillBeforeRestartIndex]).toContain("/added-playbook pre-restart")
    expect(providerBodies[addedSkillBeforeRestartIndex]).not.toContain("ADDED_SKILL_BODY pre-restart")
    expect(await commandEventCount("added-playbook")).toBe(addedSkillCommandsBefore)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    await waitFor(sessionIsIdle, "new Skill before restart fallback completion")
    expect(providerRequests).toBe(9)

    const removedSkillCommandsBefore = await commandEventCount("user-playbook")
    const removedSkillBeforeRestartIndex = providerBodies.length
    const removedSkillBeforeRestartStart = (await output()).length
    await writeSemanticInput(process.stdin, "/user-playbook stale")
    await waitFor(
      async () => (await output()).slice(removedSkillBeforeRestartStart).includes("/user-playbook stale"),
      "removed Skill before restart input",
    )
    await writeSemanticInput(process.stdin, "\r")
    await waitFor(() => providerBodies.length > removedSkillBeforeRestartIndex, "removed Skill before restart request")
    expect(providerBodies[removedSkillBeforeRestartIndex]).toContain("/user-playbook stale")
    expect(providerBodies[removedSkillBeforeRestartIndex]).not.toContain("USER_PLAYBOOK_BODY_V2 stale")
    expect(await commandEventCount("user-playbook")).toBe(removedSkillCommandsBefore + 1)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    await waitFor(sessionIsIdle, "removed Skill before restart fallback completion")
    expect(providerRequests).toBe(10)

    await writeSemanticInput(process.stdin, "\x03")
    process.stdin.end()
    // Process exit is external OS state; this deadline only bounds failed terminal restoration.
    const status = await Promise.race([
      process.exited,
      Bun.sleep(15_000).then(() => {
        process.kill(9)
        throw new Error("picker PTY smoke timed out")
      }),
    ])
    expect(status).toBe(0)
    ownedProcess = undefined

    const restartTranscript = path.join(temp, "typescript-restart")
    const restarted = Bun.spawn(
      [
        "/usr/bin/script",
        "-q",
        "-e",
        "-f",
        "-c",
        'stty rows 30 cols 100; before=$(stty -g); before_fg=$(ps -o tpgid= -p $$ | tr -d " "); "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_PTY_SESSION"; code=$?; after=$(stty -g); after_fg=$(ps -o tpgid= -p $$ | tr -d " "); [ "$before" = "$after" ] || exit 97; [ "$before_fg" = "$after_fg" ] || exit 98; exit "$code"',
        restartTranscript,
      ],
      {
        cwd: path.join(root, "packages/hya-tui-ts"),
        env: {
          ...env,
          HYA_PTY_PROJECT: project,
          HYA_PTY_SESSION: session.id,
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
    ownedProcess = restarted
    const restartedOutput = async () => stripAnsi(await readFile(restartTranscript, "utf8").catch(() => ""))
    /** Poll real restarted PTY/process I/O; fake timers cannot advance OS state. */
    const waitForRestart = async (check: () => boolean | Promise<boolean>, label: string) => {
      const deadline = Date.now() + 10_000
      while (!(await check())) {
        const exited = await Promise.race([
          restarted.exited.then((exitStatus) => ({ status: exitStatus })),
          Bun.sleep(50).then(() => undefined),
        ])
        if (exited) throw new Error(`restarted PTY exited before ${label} with status ${exited.status}`)
        if (Date.now() >= deadline) {
          throw new Error(`timed out waiting for restarted ${label}: ${(await restartedOutput()).slice(-3000)}`)
        }
      }
    }
    await waitForRestart(async () => {
      const frame = await restartedOutput()
      return frame.includes("ctrl+p commands") && frame.includes("gpt-picker") && frame.includes("·low")
    }, "Session")

    const addedAfterRestartIndex = providerBodies.length
    const addedAfterRestartStart = (await restartedOutput()).length
    await writeSemanticInput(restarted.stdin, "/added post-restart")
    await waitForRestart(
      async () => (await restartedOutput()).slice(addedAfterRestartStart).includes("/added post-restart"),
      "added command input",
    )
    await writeSemanticInput(restarted.stdin, "\r")
    await waitForRestart(() => providerBodies.length > addedAfterRestartIndex, "added command request")
    expect(providerBodies[addedAfterRestartIndex]).toContain("ADDED_BODY post-restart")
    const addedAfterRestartBody = JSON.parse(providerBodies[addedAfterRestartIndex]) as {
      reasoning?: { effort?: string }
    }
    expect(addedAfterRestartBody.reasoning?.effort).toBe("low")
    await waitForRestart(sessionIsIdle, "added command completion")

    const removedAfterRestartIndex = providerBodies.length
    const removedAfterRestartStart = (await restartedOutput()).length
    await writeSemanticInput(restarted.stdin, "/remove post-restart")
    await waitForRestart(
      async () => (await restartedOutput()).slice(removedAfterRestartStart).includes("/remove post-restart"),
      "removed command input",
    )
    await writeSemanticInput(restarted.stdin, "\r")
    await waitForRestart(() => providerBodies.length > removedAfterRestartIndex, "removed command ordinary prompt")
    expect(providerBodies[removedAfterRestartIndex]).toContain("/remove post-restart")
    await waitForRestart(sessionIsIdle, "removed command completion")

    expect(await commandEventCount("remove")).toBe(1)
    expect(await commandEventCount("added")).toBe(1)
    const addedSkillAfterRestartCommandsBefore = await commandEventCount("added-playbook")
    const addedSkillAfterRestartIndex = providerBodies.length
    const addedSkillAfterRestartStart = (await restartedOutput()).length
    await writeSemanticInput(restarted.stdin, "/added-playbook post-restart")
    await waitForRestart(
      async () => (await restartedOutput()).slice(addedSkillAfterRestartStart).includes("/added-playbook post-restart"),
      "new Skill after restart input",
    )
    await writeSemanticInput(restarted.stdin, "\r")
    await waitForRestart(() => providerBodies.length > addedSkillAfterRestartIndex, "new Skill after restart request")
    expect(providerBodies[addedSkillAfterRestartIndex]).toContain("ADDED_SKILL_BODY post-restart")
    expect(await commandEventCount("added-playbook")).toBe(addedSkillAfterRestartCommandsBefore + 1)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    await waitForRestart(sessionIsIdle, "new Skill after restart completion")

    const removedSkillAfterRestartCommandsBefore = await commandEventCount("user-playbook")
    const removedSkillAfterRestartIndex = providerBodies.length
    const removedSkillAfterRestartStart = (await restartedOutput()).length
    await writeSemanticInput(restarted.stdin, "/user-playbook post-restart")
    await waitForRestart(
      async () =>
        (await restartedOutput()).slice(removedSkillAfterRestartStart).includes("/user-playbook post-restart"),
      "removed Skill after restart input",
    )
    await writeSemanticInput(restarted.stdin, "\r")
    await waitForRestart(
      () => providerBodies.length > removedSkillAfterRestartIndex,
      "removed Skill after restart request",
    )
    expect(providerBodies[removedSkillAfterRestartIndex]).toContain("/user-playbook post-restart")
    expect(providerBodies[removedSkillAfterRestartIndex]).not.toContain("USER_PLAYBOOK_BODY_V2 post-restart")
    expect(await commandEventCount("user-playbook")).toBe(removedSkillAfterRestartCommandsBefore)
    expect(await toolCallCount("skill")).toBe(skillToolCallsBefore)
    await waitForRestart(sessionIsIdle, "removed Skill after restart fallback completion")

    expect(providerRequests).toBe(14)

    await writeSemanticInput(restarted.stdin, "\x03")
    restarted.stdin.end()
    // Restarted process exit is external OS state; this deadline bounds failed restoration.
    const restartedStatus = await Promise.race([
      restarted.exited,
      Bun.sleep(15_000).then(() => {
        restarted.kill(9)
        throw new Error("restarted picker PTY smoke timed out")
      }),
    ])
    expect(restartedStatus).toBe(0)
    ownedProcess = undefined
  } finally {
    if (ownedProcess) await stopOwnedProcess(ownedProcess)
    await stopOwnedProcess(server)
    provider.stop(true)
    await rm(temp, { recursive: true, force: true })
  }
}, 90_000)

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
    const offlineNotice = "No live provider is available. Configure a provider to continue."
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
      return JSON.stringify(messages).includes(`${rootTranscript}\\n\\n${offlineNotice}`)
    }, "root transcript fixture")
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: childSession.id })).data
      return JSON.stringify(messages).includes(`${childTranscript}\\n\\n${offlineNotice}`)
    }, "child transcript fixture")
    for (const [sessionID, marker] of [
      [secondChildSession.id, secondChildTranscript],
      [grandchildSession.id, grandchildTranscript],
      [resetRootSession.id, resetRootTranscript],
    ]) {
      await waitFor(async () => {
        const messages = (await client.session.messages({ sessionID })).data
        return JSON.stringify(messages).includes(`${marker}\\n\\n${offlineNotice}`)
      }, `${marker} fixture`)
    }
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: scrollChildSession.id })).data
      const value = JSON.stringify(messages)
      return value.includes(scrollChildTranscript) && value.includes(`${scrollChildTail}\\n\\n${offlineNotice}`)
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
        if (request.method === "GET" && incoming.pathname === "/tui/bootstrap" && response.ok) {
          const bundle = (await response.json()) as { sessions?: Array<{ id: string }> }
          // The client cache must normalize server order.
          if (bundle.sessions) {
            bundle.sessions = bundle.sessions.toSorted((a, b) => (a.id === b.id ? 0 : a.id < b.id ? 1 : -1))
          }
          return Response.json(bundle)
        }
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
        // The first confirmMainInput after real observation focus proves the complete draft is preserved/rendered; final root Events prove it was not submitted.
        await writeInput(rootDraft)
        const waitForMain = async (start: number, message: string, marker: string) => {
          // Fresh footer is only the render barrier; marker plus draft proves ownership.
          await waitFor(
            async () => (await output()).slice(start).includes("ctrl+p commands"),
            `${message} render`,
          )
          await writeInput(marker)
          await waitFor(async () => {
            const frame = (await output()).slice(start)
            return frame.includes(marker) && frame.includes(rootDraft)
          }, message)
        }
        const getRequestCount = (path: string) =>
          requests.filter((request) => request.method === "GET" && request.path === path).length
        const confirmMainInput = async (start: number, marker: string) => {
          try {
            const permissionRequestsBefore = getRequestCount("/permission")
            const questionRequestsBefore = getRequestCount("/question")
            await writeInput(escapeKey)
            await waitFor(
              () =>
                getRequestCount("/permission") > permissionRequestsBefore &&
                getRequestCount("/question") > questionRequestsBefore,
              "Main focus hydration",
            )
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
        await waitForMain(
          closeFilteredManagerStart,
          "Main after filtered manager",
          "MAIN_AFTER_FILTER_4f3a",
        )

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
          await waitForMain(
            closePlacementManagerStart,
            `Main after ${placement} manager`,
            `MAIN_AFTER_${placement}_9c21`,
          )
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
                      `matching_requests=${JSON.stringify(matchingReplyRequests.slice(-64))}`,
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
                    `matching_requests=${JSON.stringify(matchingReplyRequests.slice(-64))}`,
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
            tracePhase(permissionCallsite, "pending_interaction_not_rendered", pendingPermissionID)
            throw new Error(
              [
                "PENDING_INTERACTION_NOT_RENDERED",
                `permission_id=${JSON.stringify(pendingPermissionID)}`,
                `matching_requests=${JSON.stringify(matchingReplyRequests.slice(-64))}`,
                `requests_since_cursor_tail=${JSON.stringify(
                  requests.slice(Math.max(permissionRequestCursor, requests.length - 64)),
                )}`,
                `phase_trace=${JSON.stringify(phaseTrace)}`,
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

        const sessionListStart = (await output()).length
        await writeInput("\x18l")
        await waitFor(async () => (await output()).slice(sessionListStart).includes("Sessions"), "session list")
        // With two roots, current is second; Up + Enter selects newest reset root first without search debounce.
        const resetStart = (await output()).length
        const resetSessionPath = `/session/${resetRootSession.id}`
        const resetTreePath = `${resetSessionPath}/tree`
        const resetSessionRequestsBefore = getRequestCount(resetSessionPath)
        const resetTreeRequestsBefore = getRequestCount(resetTreePath)
        await writeInput("\x1b[A\r")
        await waitFor(() => getRequestCount(resetTreePath) > resetTreeRequestsBefore, "fresh reset tree request")
        await waitFor(() => getRequestCount(resetSessionPath) > resetSessionRequestsBefore, "fresh reset session request")
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
        expect(rootEvents).toContain(rootTranscript)
        expect(rootSessionFrameReady(rootFrame)).toBe(true)
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
