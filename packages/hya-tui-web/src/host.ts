// Web host for a terminal program: serves the xterm.js page and bridges each
// WebSocket connection on `/pty` to one fresh PTY process running the fixed
// host command. The browser can never choose what runs.

import type { Server, ServerWebSocket, Subprocess } from "bun"
import page from "../web/index.html"
import { decodeClientFrame, encodeServerFrame } from "./frames"

export type HostOptions = {
  /** argv spawned for every browser connection. */
  command: string[]
  /** Working directory of the spawned command (default: process cwd). */
  cwd?: string
  /** Extra environment for the spawned command. */
  env?: Record<string, string>
  /** Bind address (default 127.0.0.1). */
  hostname?: string
  /** Bind port; 0 picks a free port (default 7681). */
  port?: number
  /** `stop()`: wait after SIGHUP before SIGKILL for PTY processes still running (default 3000 ms). */
  stopGraceMs?: number
}

export type Host = {
  /** Base URL with a trailing slash, e.g. `http://127.0.0.1:7681/`. */
  url: string
  /**
   * Stop listening and end every PTY process: SIGHUP (what closing a tab
   * sends), then SIGKILL to the process group of any still running after
   * `stopGraceMs`. Resolves once all of them have exited.
   */
  stop(): Promise<void>
}

type Connection = { cols: number; rows: number; proc?: Subprocess }

function size(value: string | null, fallback: number): number {
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 && parsed <= 4096 ? parsed : fallback
}

function sameOrigin(request: Request): boolean {
  const origin = request.headers.get("origin")
  if (!origin) return true
  try {
    return new URL(origin).host === request.headers.get("host")
  } catch {
    return false
  }
}

export function startHost(options: HostOptions): Host {
  const connections = new Set<ServerWebSocket<Connection>>()
  const processes = new Set<Subprocess>()
  const env = { ...process.env, TERM: "xterm-256color", COLORTERM: "truecolor", ...options.env }

  function open(ws: ServerWebSocket<Connection>) {
    connections.add(ws)
    const proc = Bun.spawn(options.command, {
      cwd: options.cwd,
      env,
      terminal: {
        cols: ws.data.cols,
        rows: ws.data.rows,
        data(_terminal, bytes) {
          ws.send(encodeServerFrame({ output: bytes }))
        },
      },
    })
    ws.data.proc = proc
    processes.add(proc)
    void proc.exited.then(async (code) => {
      processes.delete(proc)
      // Let the PTY reader flush the child's last output before reporting exit.
      await Bun.sleep(50)
      proc.terminal?.close()
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(encodeServerFrame({ exit: code }))
        ws.close(1000, "process exited")
      }
    })
  }

  function message(ws: ServerWebSocket<Connection>, text: string | Buffer) {
    const frame = decodeClientFrame(String(text))
    const terminal = ws.data.proc?.terminal
    if (!frame || !terminal || terminal.closed) return
    if ("input" in frame) terminal.write(frame.input)
    else if ("resize" in frame) terminal.resize(frame.resize.cols, frame.resize.rows)
    else ws.send(encodeServerFrame({ pong: true }))
  }

  function close(ws: ServerWebSocket<Connection>) {
    connections.delete(ws)
    const proc = ws.data.proc
    if (proc && proc.exitCode === null) proc.kill("SIGHUP")
  }

  const server: Server<Connection> = Bun.serve<Connection>({
    hostname: options.hostname ?? "127.0.0.1",
    port: options.port ?? 7681,
    development: false,
    routes: { "/": page },
    fetch(request, server) {
      const url = new URL(request.url)
      if (url.pathname !== "/pty") return new Response("Not found", { status: 404 })
      if (!sameOrigin(request)) return new Response("Forbidden", { status: 403 })
      const data = { cols: size(url.searchParams.get("cols"), 80), rows: size(url.searchParams.get("rows"), 24) }
      if (server.upgrade(request, { data })) return undefined
      return new Response("Expected a WebSocket upgrade", { status: 400 })
    },
    websocket: { open, message, close },
  })

  return {
    url: server.url.toString(),
    async stop() {
      for (const ws of connections) close(ws)
      const running = [...processes]
      for (const proc of running) if (proc.exitCode === null && proc.signalCode === null) proc.kill("SIGHUP")
      const exited = Promise.all(running.map((proc) => proc.exited))
      const grace = options.stopGraceMs ?? 3000
      if (await Promise.race([exited.then(() => true), Bun.sleep(grace).then(() => false)]) === false) {
        for (const proc of running) {
          if (proc.exitCode !== null || proc.signalCode !== null) continue
          try {
            // The PTY child leads its own session and process group: end its children too.
            process.kill(-proc.pid, "SIGKILL")
          } catch {
            proc.kill("SIGKILL")
          }
        }
        await exited
      }
      // Let each tab receive its `exit` frame before the sockets close.
      if (running.length > 0) await Bun.sleep(100)
      await server.stop(true)
    },
  }
}
