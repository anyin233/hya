// Files a browser tab pastes or drops (ADR-0037): the host stores each one in
// a directory of its own for that connection, `$TMPDIR/hya-tui-web-<pid>/<n>/`
// (mode 0700), and answers with the stored path; the page then pastes the
// paths into the terminal, as a native terminal does with a dragged file. The
// directory goes away when the connection closes. Nothing here knows what runs
// on the PTY.

import { mkdirSync, rmSync, writeFileSync, existsSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { ServerFrame, Upload } from "./frames"

/** One file at most this large. */
export const maxUploadBytes = 20 * 1024 * 1024
/** One connection stores at most this many files. */
export const maxUploadsPerConnection = 32
/** WebSocket message limit: a full-size upload in base64 plus the frame. */
export const maxFrameBytes = Math.ceil((maxUploadBytes * 4) / 3) + 64 * 1024

/** The host's upload root for this process. */
export function uploadRoot(): string {
  return join(tmpdir(), `hya-tui-web-${process.pid}`)
}

/**
 * A safe file name: the last path segment, characters outside letters,
 * digits, space, `.`, `_`, `-`, `+`, `@` replaced by `_`, no leading dot, at
 * most 120 characters, `upload` when nothing is left.
 */
export function sanitizeUploadName(name: string): string {
  const last = name.split(/[\\/]/).at(-1) ?? ""
  const cleaned = last.normalize("NFC").replace(/[^\p{L}\p{N} ._+@-]/gu, "_").replace(/^[.\s]+/, "").trim()
  const short = cleaned.length > 120 ? cleaned.slice(cleaned.length - 120) : cleaned
  return short || "upload"
}

export class UploadStore {
  private readonly dir: string
  private count = 0

  constructor(root: string, connection: number) {
    this.dir = join(root, String(connection))
  }

  /** Store one upload; never throws. */
  store(upload: Upload): ServerFrame {
    const fail = (reason: string): ServerFrame => ({ uploadFailed: { id: upload.id, reason } })
    if (this.count >= maxUploadsPerConnection) return fail(`at most ${maxUploadsPerConnection} files per tab`)
    if (upload.data.byteLength > maxUploadBytes) return fail("larger than 20 MiB")
    if (upload.data.byteLength === 0) return fail("empty file")
    try {
      mkdirSync(this.dir, { recursive: true, mode: 0o700 })
      const name = sanitizeUploadName(upload.name)
      let path = join(this.dir, name)
      for (let n = 2; existsSync(path); n++) path = join(this.dir, `${n}-${name}`)
      writeFileSync(path, upload.data, { mode: 0o600 })
      this.count++
      return { uploaded: { id: upload.id, path } }
    } catch (error) {
      return fail(`could not store it: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  /** Remove the connection's files. */
  clear(): void {
    rmSync(this.dir, { recursive: true, force: true })
  }
}

export { pastedPathsText } from "./uploadText"
