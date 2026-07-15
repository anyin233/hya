import { expect, test } from "bun:test"
import { createOpencodeClient } from "@opencode-ai/sdk/v2/client"

import { mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import stripAnsi from "strip-ansi"

const root = path.resolve(import.meta.dir, "../../..")
const backend = path.join(root, "target/debug/hya-backend")
const launcher = path.join(root, "target/debug/hya-ts")

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
    process.stdin.write("\x03")
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

test("Linux PTY child observation is visibly read-only and preserves the root draft", async () => {
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
    const rootTranscript = "ROOT_TRANSCRIPT_7f32"
    const childTranscript = "CHILD_TRANSCRIPT_98ac"
    await client.session.promptAsync(
      { sessionID: rootSession.id, parts: [{ type: "text", text: rootTranscript }] },
      { throwOnError: true },
    )
    await client.session.promptAsync(
      { sessionID: childSession.id, parts: [{ type: "text", text: childTranscript }] },
      { throwOnError: true },
    )
    const waitFor = async (check: () => boolean | Promise<boolean>, message: string) => {
      const deadline = Date.now() + 10_000
      while (!(await check())) {
        if (Date.now() >= deadline) throw new Error(`timed out waiting for ${message}`)
        await Bun.sleep(50)
      }
    }
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: rootSession.id })).data
      return JSON.stringify(messages).includes(`(hya dev provider) You said: \\"${rootTranscript}\\"`)
    }, "root transcript fixture")
    await waitFor(async () => {
      const messages = (await client.session.messages({ sessionID: childSession.id })).data
      return JSON.stringify(messages).includes(`(hya dev provider) You said: \\"${childTranscript}\\"`)
    }, "child transcript fixture")

    const requests: Array<{ method: string; path: string }> = []
    const proxy = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      async fetch(request) {
        const incoming = new URL(request.url)
        requests.push({ method: request.method, path: incoming.pathname })
        const headers = new Headers(request.headers)
        headers.delete("host")
        const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer()
        return fetch(new URL(incoming.pathname + incoming.search, url), {
          method: request.method,
          headers,
          body,
          redirect: "manual",
        })
      },
    })

    try {
      const process = Bun.spawn(
        [
          "/usr/bin/script",
          "-q",
          "-e",
          "-f",
          "-c",
          'stty rows 30 cols 100; "$HYA_TS" "$HYA_PTY_PROJECT" --server "$HYA_PTY_URL" --session "$HYA_ROOT_SESSION"',
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
        await waitFor(async () => (await output()).includes(rootTranscript), "root session frame")

        const rootDraft = "ROOT_DRAFT_c281"
        const beforeDraft = (await output()).length
        process.stdin.write(rootDraft)
        await waitFor(async () => (await output()).slice(beforeDraft).includes(rootDraft), "root draft")
        expect(await output()).toContain("commands")

        process.stdin.write("\x18")
        await Bun.sleep(100)
        const childStart = (await output()).length
        process.stdin.write("\x1b[B")
        await waitFor(async () => {
          const child = (await output()).slice(childStart)
          return child.includes(childTranscript) && child.includes("Worker") && child.includes("Parent")
        }, "child observation frame")

        const childSentinel = "CHILD_INPUT_1da9"
        const promptRequestsBefore = requests.filter(
          (request) => request.method === "POST" && /\/session\/[^/]+\/(?:message|prompt_async)$/.test(request.path),
        ).length
        process.stdin.write(childSentinel)
        process.stdin.write("\r")
        await Bun.sleep(300)
        const childFrame = (await output()).slice(childStart)

        const rootStart = (await output()).length
        process.stdin.write("\x1b[A")
        await waitFor(async () => (await output()).slice(rootStart).includes(rootDraft), "unchanged root draft")
        const rootFrame = (await output()).slice(rootStart)

        process.stdin.write("\x03")
        process.stdin.end()
        const status = await Promise.race([
          process.exited,
          Bun.sleep(15_000).then(() => {
            process.kill(9)
            throw new Error("PTY child observation timed out")
          }),
        ])
        const promptRequestsAfter = requests.filter(
          (request) => request.method === "POST" && /\/session\/[^/]+\/(?:message|prompt_async)$/.test(request.path),
        ).length
        const childEvents = await (await fetch(`${url}/sessions/${childSession.id}/events`)).text()
        const rootEvents = await (await fetch(`${url}/sessions/${rootSession.id}/events`)).text()

        expect(status).toBe(0)
        expect(childFrame).toContain(childTranscript)
        expect(childFrame).toContain("Worker")
        expect(childFrame).not.toContain(rootDraft)
        expect(childFrame).not.toContain("commands")
        expect(childFrame).not.toContain(childSentinel)
        expect(promptRequestsAfter).toBe(promptRequestsBefore)
        expect(childEvents).not.toContain(childSentinel)
        expect(rootEvents).not.toContain(childSentinel)
        expect(rootEvents).not.toContain(rootDraft)
        expect(rootFrame).not.toContain(childSentinel)
        expect(childFrame.toLowerCase()).toContain("read-only")
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
}, 60_000)
