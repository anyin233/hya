// Protojson codec for the hya.v2 `PtyClientFrame` / `PtyServerFrame` messages
// (proto/hya/v2/pty.proto). Bytes fields travel as standard base64, so the
// browser client speaks the same frame shapes as `/v2/pty/{id}/connect`.

/** A file pasted or dropped into the page (ADR-0037); the host answers `uploaded` or `uploadFailed` with the same `id`. */
export type Upload = { id: string; name: string; mime: string; data: Uint8Array }

export type ClientFrame =
  | { input: Uint8Array }
  | { resize: { cols: number; rows: number } }
  | { ping: true }
  | { upload: Upload }

export type ServerFrame =
  | { output: Uint8Array }
  | { exit: number }
  | { pong: true }
  | { uploaded: { id: string; path: string } }
  | { uploadFailed: { id: string; reason: string } }

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
  if ("upload" in frame) return JSON.stringify({ upload: { ...frame.upload, data: toBase64(frame.upload.data) } })
  return JSON.stringify(frame)
}

/** A protojson string field: absent is the empty string; anything else but a string is invalid. */
const stringField = (value: unknown): string | null => typeof value === "string" ? value : value === undefined ? "" : null

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
  if (value.upload && typeof value.upload === "object") {
    const upload = value.upload as Record<string, unknown>
    const id = stringField(upload.id), name = stringField(upload.name), mime = stringField(upload.mime), data = stringField(upload.data)
    if (id === null || name === null || mime === null || data === null) return null
    try {
      return { upload: { id, name, mime, data: fromBase64(data) } }
    } catch {
      return null
    }
  }
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
  const result = (key: "uploaded" | "uploadFailed", field: "path" | "reason") => {
    const body = value[key]
    if (!body || typeof body !== "object") return null
    const id = stringField((body as Record<string, unknown>).id), detail = stringField((body as Record<string, unknown>)[field])
    return id === null || detail === null ? null : { id, detail }
  }
  const uploaded = result("uploaded", "path")
  if (uploaded) return { uploaded: { id: uploaded.id, path: uploaded.detail } }
  const failed = result("uploadFailed", "reason")
  if (failed) return { uploadFailed: { id: failed.id, reason: failed.detail } }
  return null
}
