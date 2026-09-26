// Browser side: xterm.js fitted to the window, attached to the host's `/pty`
// WebSocket. `window.hyaTerm` exposes the terminal to test drivers.

import { FitAddon } from "@xterm/addon-fit"
import { Terminal } from "@xterm/xterm"
import { decodeServerFrame, encodeClientFrame } from "../src/frames"
import { createNotificationDeduper, notificationTag, osc777Notification, osc9Notification, shouldShowNotification, type NotificationRequest } from "../src/notify"

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

// Desktop notifications (docs/tui-web.md "Desktop notifications", ADR-0021):
// generic mapping of OSC 9 / OSC 777 to a browser Notification, regardless
// of what runs on the PTY. A program may send more than one notify sequence
// for the same event (OSC 9 and OSC 777 together, for terminals that honor
// only one); the deduper drops a same-body repeat within a short window,
// and `tag` also asks the browser itself to coalesce duplicates it lets
// through (a second tab, or one past the window).
const notificationDeduper = createNotificationDeduper()
function showNotification(request: NotificationRequest): void {
  if (typeof Notification === "undefined" || Notification.permission !== "granted") return
  if (!shouldShowNotification({ hidden: document.hidden, focused: document.hasFocus() })) return
  if (!notificationDeduper.shouldShow(request.body, Date.now())) return
  new Notification(request.title || document.title || "Notification", { body: request.body, tag: notificationTag(request) })
}

// The Notification permission prompt only opens on a user gesture; ask once,
// the first time the user interacts with the page. A denial or an
// unsupported browser is silent — no retry, no error.
function requestNotificationPermissionOnce(): void {
  if (typeof Notification === "undefined" || Notification.permission !== "default") return
  void Notification.requestPermission().catch(() => undefined)
}
const terminalElement = document.getElementById("terminal")!
terminalElement.addEventListener("pointerdown", requestNotificationPermissionOnce, { once: true })
terminalElement.addEventListener("keydown", requestNotificationPermissionOnce, { once: true })

term.parser.registerOscHandler(9, (payload) => {
  showNotification(osc9Notification(payload))
  return true
})
term.parser.registerOscHandler(777, (payload) => {
  const request = osc777Notification(payload)
  if (request) showNotification(request)
  return true
})
