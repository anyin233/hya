import { expect, test } from "bun:test"

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
