// Browser side: xterm.js fitted to the window, attached to the host's `/pty`
// WebSocket. `window.hyaTerm` exposes the terminal to test drivers.

import { FitAddon } from "@xterm/addon-fit"
import { Terminal } from "@xterm/xterm"
import { decodeServerFrame, encodeClientFrame } from "../src/frames"

export type HyaTermHook = {
  term: Terminal
  /** True once the WebSocket is open. */
  connected: boolean
  /** Child exit code once the host reports it, else null. */
  exitCode: number | null
  /** Count of output frames written, for settle detection. */
  frames: number
}

declare global {
  interface Window {
    hyaTerm: HyaTermHook
  }
}

const params = new URLSearchParams(location.search)
const term = new Terminal({
  allowProposedApi: true,
  cursorBlink: false,
  fontFamily: params.get("font") ?? "Menlo, 'DejaVu Sans Mono', monospace",
  fontSize: Number(params.get("fontSize") ?? 14),
  theme: { background: "#11151b" },
})
const fit = new FitAddon()
term.loadAddon(fit)
term.open(document.getElementById("terminal")!)
fit.fit()

const hook: HyaTermHook = { term, connected: false, exitCode: null, frames: 0 }
window.hyaTerm = hook

const socketUrl = new URL("pty", location.href)
socketUrl.protocol = location.protocol === "https:" ? "wss:" : "ws:"
socketUrl.searchParams.set("cols", String(term.cols))
socketUrl.searchParams.set("rows", String(term.rows))
const socket = new WebSocket(socketUrl)
const encoder = new TextEncoder()

socket.addEventListener("open", () => {
  hook.connected = true
})
socket.addEventListener("message", (event) => {
  const frame = decodeServerFrame(String(event.data))
  if (!frame) return
  if ("output" in frame) {
    term.write(frame.output)
    hook.frames++
  } else if ("exit" in frame) {
    hook.exitCode = frame.exit
    term.write(`\r\n\x1b[2m[process exited with code ${frame.exit}]\x1b[0m\r\n`)
  }
})

function send(text: string) {
  if (socket.readyState === WebSocket.OPEN) socket.send(text)
}

term.onData((data) => send(encodeClientFrame({ input: encoder.encode(data) })))
term.onBinary((data) => send(encodeClientFrame({ input: Uint8Array.from(data, (char) => char.charCodeAt(0)) })))
term.onResize(({ cols, rows }) => send(encodeClientFrame({ resize: { cols, rows } })))
new ResizeObserver(() => fit.fit()).observe(document.getElementById("terminal")!)
term.focus()
