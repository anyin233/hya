// Protojson codec for the hya.v1 `PtyClientFrame` / `PtyServerFrame` messages
// (proto/hya/v1/pty.proto). Bytes fields travel as standard base64, so the
// browser client speaks the same frame shapes as `/v1/pty/{id}/connect`.

export type ClientFrame =
  | { input: Uint8Array }
  | { resize: { cols: number; rows: number } }
  | { ping: true }

export type ServerFrame =
  | { output: Uint8Array }
  | { exit: number }
  | { pong: true }

function toBase64(bytes: Uint8Array): string {
  let binary = ""
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000))
  }
  return btoa(binary)
}

function fromBase64(text: string): Uint8Array {
  const binary = atob(text)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index)
  return bytes
}

function parseObject(text: string): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(text)
    return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null
  } catch {
    return null
  }
}

function dimension(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) && value > 0 && value <= 4096 ? value : null
}

export function encodeClientFrame(frame: ClientFrame): string {
  if ("input" in frame) return JSON.stringify({ input: toBase64(frame.input) })
  return JSON.stringify(frame)
}

export function decodeClientFrame(text: string): ClientFrame | null {
  const value = parseObject(text)
  if (!value) return null
  if (typeof value.input === "string") return { input: fromBase64(value.input) }
  if (value.resize && typeof value.resize === "object") {
    const resize = value.resize as Record<string, unknown>
    const cols = dimension(resize.cols)
    const rows = dimension(resize.rows)
    return cols && rows ? { resize: { cols, rows } } : null
  }
  if (value.ping === true) return { ping: true }
  return null
}

export function encodeServerFrame(frame: ServerFrame): string {
  if ("output" in frame) return JSON.stringify({ output: toBase64(frame.output) })
  return JSON.stringify(frame)
}

export function decodeServerFrame(text: string): ServerFrame | null {
  const value = parseObject(text)
  if (!value) return null
  if (typeof value.output === "string") return { output: fromBase64(value.output) }
  if (typeof value.exit === "number") return { exit: value.exit }
  if (value.pong === true) return { pong: true }
  return null
}
