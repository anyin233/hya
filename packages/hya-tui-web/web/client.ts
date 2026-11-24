// Browser side: xterm.js fitted to the window, attached to the host's `/pty`
// WebSocket. `window.hyaTerm` exposes the terminal to test drivers.

import { FitAddon } from "@xterm/addon-fit"
import { Terminal } from "@xterm/xterm"
import { decodeServerFrame, encodeClientFrame } from "../src/frames"
import { pastedPathsText } from "../src/uploadText"
import { pageTitle } from "../src/title"
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
/** Answers to upload frames, by upload id. */
const pendingUploads = new Map<string, (answer: { path: string } | { reason: string }) => void>()

socket.addEventListener("message", (event) => {
  const frame = decodeServerFrame(String(event.data))
  if (!frame) return
  if ("output" in frame) {
    term.write(frame.output)
    hook.frames++
  } else if ("exit" in frame) {
    hook.exitCode = frame.exit
    term.write(`\r\n\x1b[2m[process exited with code ${frame.exit}]\x1b[0m\r\n`)
  } else if ("uploaded" in frame) {
    pendingUploads.get(frame.uploaded.id)?.({ path: frame.uploaded.path })
  } else if ("uploadFailed" in frame) {
    pendingUploads.get(frame.uploadFailed.id)?.({ reason: frame.uploadFailed.reason })
  }
})

function send(text: string) {
  if (socket.readyState === WebSocket.OPEN) socket.send(text)
}

// xterm.js does not expose Shift+Enter as a distinct terminal sequence. The
// generic host enables this mapping only for programs that opt in with
// `--shift-enter-lf`; otherwise it preserves xterm.js's ordinary CR behavior.
let shiftEnterLf = false
void fetch("/input-config")
  .then(async (response) => response.ok ? await response.json() as { shiftEnterLf?: unknown } : undefined)
  .then((config) => { shiftEnterLf = config?.shiftEnterLf === true })
  .catch(() => undefined)
const terminalElement = document.getElementById("terminal")!
terminalElement.addEventListener("keydown", (event) => {
  if (!shiftEnterLf || event.isComposing || event.key !== "Enter" || !event.shiftKey || event.ctrlKey || event.altKey || event.metaKey) return
  event.preventDefault()
  event.stopImmediatePropagation()
  send(encodeClientFrame({ input: encoder.encode("\n") }))
}, true)

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
terminalElement.addEventListener("pointerdown", requestNotificationPermissionOnce, { once: true })
terminalElement.addEventListener("keydown", requestNotificationPermissionOnce, { once: true })

// The tab title follows the terminal title (OSC 0/2), as in a terminal
// emulator's tab bar (ADR-0030); a cleared title shows the page's own again.
const initialTitle = document.title
term.onTitleChange((title) => { document.title = pageTitle(title, initialTitle) })

term.parser.registerOscHandler(9, (payload) => {
  showNotification(osc9Notification(payload))
  return true
})
term.parser.registerOscHandler(777, (payload) => {
  const request = osc777Notification(payload)
  if (request) showNotification(request)
  return true
})

// Pasted and dropped files (ADR-0037): each file goes to the host as an
// `upload` frame; once the host has stored them, their paths are pasted into
// the terminal, as a native terminal pastes the path of a file dragged into it.
// Text pastes keep xterm.js's own handling. The host stores files only; what
// the program does with the pasted paths is its own business.
let uploadIds = 0
function upload(file: File): Promise<{ path: string } | { reason: string }> {
  return file.arrayBuffer().then((buffer) => new Promise((resolve) => {
    const id = `u${++uploadIds}`
    pendingUploads.set(id, (answer) => {
      pendingUploads.delete(id)
      resolve(answer)
    })
    send(encodeClientFrame({ upload: { id, name: file.name || "pasted", mime: file.type, data: new Uint8Array(buffer) } }))
  }))
}

function notice(text: string): void {
  const element = document.createElement("div")
  element.className = "notice"
  element.textContent = text
  document.body.append(element)
  setTimeout(() => element.remove(), 6000)
}

async function uploadAndPaste(files: readonly File[]): Promise<void> {
  if (!files.length) return
  const answers = await Promise.all(files.map(upload))
  const paths = answers.flatMap((answer) => "path" in answer ? [answer.path] : [])
  const failed = answers.flatMap((answer, index) => "reason" in answer ? [`${files[index]!.name}: ${answer.reason}`] : [])
  if (failed.length) notice(`Not uploaded · ${failed.join(" · ")}`)
  if (paths.length) term.paste(pastedPathsText(paths))
  term.focus()
}

terminalElement.addEventListener("paste", (event) => {
  const files = [...(event.clipboardData?.files ?? [])]
  if (!files.length) return
  event.preventDefault()
  event.stopImmediatePropagation()
  void uploadAndPaste(files)
}, true)
terminalElement.addEventListener("dragover", (event) => {
  if (event.dataTransfer?.types.includes("Files")) event.preventDefault()
})
terminalElement.addEventListener("drop", (event) => {
  const files = [...(event.dataTransfer?.files ?? [])]
  if (!files.length) return
  event.preventDefault()
  void uploadAndPaste(files)
})
