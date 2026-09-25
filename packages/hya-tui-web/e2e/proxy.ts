// A logging HTTP pass-through for specs that assert which requests the TUI
// makes (for example: no `GET /v1/interactions` polling while a subagent
// asks). Point the TUI's `--server` at `proxy.url`; every request is
// forwarded to `target` unchanged (SSE streams included) and logged.

import { createServer, request as httpRequest, type Server } from "node:http"

export type LoggedRequest = { method: string; path: string; at: number }

export type Proxy = {
  url: string
  /** Requests so far, in arrival order. */
  log: LoggedRequest[]
  stop(): Promise<void>
}

export async function startProxy(target: string): Promise<Proxy> {
  const upstream = new URL(target)
  const log: LoggedRequest[] = []
  const server: Server = createServer((req, res) => {
    log.push({ method: req.method ?? "GET", path: req.url ?? "/", at: Date.now() })
    const forward = httpRequest(
      { hostname: upstream.hostname, port: upstream.port, method: req.method, path: req.url, headers: { ...req.headers, host: upstream.host } },
      (response) => {
        res.writeHead(response.statusCode ?? 502, response.headers)
        // Node holds headers until the first body write; an SSE stream must be "open" at once.
        res.flushHeaders()
        response.pipe(res)
      },
    )
    forward.on("error", () => res.destroy())
    res.on("close", () => forward.destroy())
    req.pipe(forward)
  })
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve))
  // A spec need not stop it (stopping mid-test would cut the TUI's stream before the final screen is captured).
  server.unref()
  const address = server.address()
  const port = typeof address === "object" && address ? address.port : 0
  return {
    url: `http://127.0.0.1:${port}`,
    log,
    stop: () => new Promise((resolve) => {
      server.closeAllConnections()
      server.close(() => resolve())
    }),
  }
}
