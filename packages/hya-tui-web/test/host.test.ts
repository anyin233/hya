import { afterEach, describe, expect, test } from "bun:test"
import { decodeServerFrame, encodeClientFrame } from "../src/frames"
import { startHost, type Host } from "../src/host"

let host: Host | undefined
afterEach(async () => {
  await host?.stop()
  host = undefined
})

function collect(ws: WebSocket) {
  const state = { text: "", exit: null as number | null }
  const decoder = new TextDecoder()
  ws.addEventListener("message", (event) => {
    const frame = decodeServerFrame(String(event.data))
    if (frame && "output" in frame) state.text += decoder.decode(frame.output, { stream: true })
    if (frame && "exit" in frame) state.exit = frame.exit
  })
  return state
}

async function until(check: () => boolean, ms = 5000) {
  const deadline = Date.now() + ms
  while (!check()) {
    if (Date.now() > deadline) throw new Error("timed out")
    await Bun.sleep(20)
  }
}

describe("tui-web host", () => {
  test("serves the terminal page", async () => {
    host = startHost({ command: ["sh", "-c", "true"], port: 0 })
    const response = await fetch(`${host.url}`)
    expect(response.status).toBe(200)
    expect(await response.text()).toContain("<div id=\"terminal\"")
  })

  test("runs the command on a real PTY sized by the client and reports its exit code", async () => {
    host = startHost({ command: ["sh", "-c", "test -t 0 && echo tty; stty size; exit 3"], port: 0 })
    const ws = new WebSocket(`${host.url.replace("http", "ws")}pty?cols=91&rows=17`)
    const state = collect(ws)
    await until(() => state.exit !== null)
    expect(state.text).toContain("tty")
    expect(state.text).toContain("17 91")
    expect(state.exit).toBe(3)
  })

  test("forwards input and resize frames to the PTY", async () => {
    host = startHost({ command: ["sh", "-c", "read line; stty size; echo got:$line"], port: 0 })
    const ws = new WebSocket(`${host.url.replace("http", "ws")}pty?cols=80&rows=24`)
    const state = collect(ws)
    await new Promise((resolve) => ws.addEventListener("open", resolve))
    ws.send(encodeClientFrame({ resize: { cols: 132, rows: 43 } }))
    ws.send(encodeClientFrame({ input: new TextEncoder().encode("hello\r") }))
    await until(() => state.exit !== null)
    expect(state.text).toContain("43 132")
    expect(state.text).toContain("got:hello")
    expect(state.exit).toBe(0)
  })

  test("rejects a cross-origin WebSocket upgrade", async () => {
    host = startHost({ command: ["sh", "-c", "true"], port: 0 })
    const response = await fetch(`${host.url}pty`, {
      headers: { upgrade: "websocket", connection: "upgrade", origin: "https://evil.example", "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==", "sec-websocket-version": "13" },
    })
    expect(response.status).toBe(403)
  })
})
