/**
 * What a pasted path becomes (docs/tui.md "Paste and attachments"):
 *
 * - an image on this machine → an image chip (bytes read now);
 * - a file or directory the backend can read (local mode, inside the session
 *   directory or a root of the active Project) → an `@path` mention;
 * - a text file the backend cannot read (outside them, or any local file in
 *   remote mode) → a file chip with its content;
 * - in remote mode, a path that exists only on the backend → an `@path` mention;
 * - anything else → the pasted words as text, with a notice saying why.
 */
import { stat } from "node:fs/promises"
import { homedir } from "node:os"
import { resolve } from "node:path"
import { isImagePath, maxAttachmentBytes, quoteMentionPath, validateAttachmentBytes } from "../composer/attachments"
import type { ChipTable } from "../composer/chips"
import { inside, looksLikeText, sniffImage, type PastedPath } from "../composer/paths"

/** A file chip holds at most this much text; a larger file is pasted as its path. */
export const fileChipMaxBytes = 256 * 1024

export interface PasteContext {
  chips: ChipTable
  /** Remote mode: the workspace is on the backend machine. */
  remote: boolean
  /** The open session's directory, else `--dir`. */
  workdir: string
  /** The active Project's roots. */
  roots: readonly string[]
  /** Remote mode: whether `path` exists on the backend (a one-byte `ReadFile`). */
  existsOnBackend(path: string): Promise<boolean>
}

export interface PasteResult {
  /** Text to insert at the cursor (tokens, mentions, or the pasted words), ending in a space. */
  insert: string
  /** Why a path stayed text, for the composer's notice row. */
  notices: string[]
}

const shortPath = (path: string): string => {
  const home = homedir()
  const shown = home && (path === home || path.startsWith(`${home}/`)) ? `~${path.slice(home.length)}` : path
  // eslint-disable-next-line no-control-regex
  const clean = shown.replace(/[\x00-\x1f\x7f]/g, "?")
  return clean.length > 60 ? `…${clean.slice(-59)}` : clean
}
const name = (path: string): string => path.replace(/\/+$/, "").split("/").at(-1) || path

/** An image chip for the file at `path` (read now), with an error when it cannot be sent. */
async function imageChip(chips: ChipTable, path: string, size: number) {
  const sizeError = validateAttachmentBytes(path, size)
  if (size > maxAttachmentBytes || size === 0) return chips.addImage(path, undefined, sizeError)
  const bytes = new Uint8Array(await Bun.file(path).arrayBuffer())
  const mime = sniffImage(bytes)
  if (!mime) return chips.addImage(path, undefined, "not a PNG, JPEG, GIF, or WebP image")
  return chips.addImage(path, { mime, data: Buffer.from(bytes).toString("base64"), size: bytes.byteLength })
}

/** Resolve every pasted path; never throws (a failed path stays text with a notice). */
export async function resolvePastedPaths(paths: readonly PastedPath[], context: PasteContext): Promise<PasteResult> {
  const pieces: string[] = []
  const notices: string[] = []
  const workdir = context.workdir.replace(/\/+$/, "")
  const readable = (path: string): boolean => !context.remote && path.startsWith("/") && (inside(path, workdir) || context.roots.some((root) => inside(path, root)))
  const mention = (path: string): string => `@${quoteMentionPath(!context.remote && workdir && path.startsWith(`${workdir}/`) ? path.slice(workdir.length + 1) : path)}`
  for (const pasted of paths) {
    const raw = pasted.raw
    const path = pasted.path.startsWith("/") || !workdir ? pasted.path : resolve(workdir, pasted.path)
    try {
      const info = path.startsWith("/") ? await stat(path).catch(() => undefined) : undefined
      if (info?.isFile() && isImagePath(path)) {
        pieces.push((await imageChip(context.chips, path, info.size)).token)
      } else if (info && readable(path)) {
        pieces.push(mention(path))
      } else if (info?.isFile()) {
        if (info.size > fileChipMaxBytes) {
          notices.push(`${name(path)}: larger than ${fileChipMaxBytes / 1024} KiB; pasted its path`)
          pieces.push(raw)
          continue
        }
        const bytes = new Uint8Array(await Bun.file(path).arrayBuffer())
        if (!looksLikeText(bytes)) {
          notices.push(`${name(path)}: not a text file; pasted its path`)
          pieces.push(raw)
          continue
        }
        pieces.push(context.chips.addFile(path, new TextDecoder().decode(bytes)).token)
      } else if (info) {
        notices.push(`${shortPath(path)}: a directory outside the workspace; pasted its path`)
        pieces.push(raw)
      } else if (context.remote && await context.existsOnBackend(path).catch(() => false)) {
        pieces.push(mention(path))
      } else {
        notices.push(`${shortPath(path)}: not found on this machine; pasted as text`)
        pieces.push(raw)
      }
    } catch (error) {
      notices.push(`${shortPath(path)}: ${error instanceof Error ? error.message : String(error)}; pasted as text`)
      pieces.push(raw)
    }
  }
  return { insert: pieces.length ? `${pieces.join(" ")} ` : "", notices }
}
