/**
 * Image attachments (docs/protocol/README.md "Prompt attachments (images)").
 *
 * An `@path` mention that composer/mention.ts inserted (`@<relative path> `)
 * and that resolves to an image file is sent with the prompt as a
 * `PromptAttachment`; the `@path` text stays in the prompt as-is — it is
 * useful context for the model ("look at @shot.png") and the server ignores
 * `path`, so keeping it costs nothing. A pasted image path (terminals paste a
 * dragged file as its path, quoted or with escaped spaces) is turned into an
 * `@path ` mention instead of being inserted as raw text, so it goes through
 * the same resolution.
 */

/** One image the server accepts (docs/protocol/README.md limits). */
export const maxAttachmentBytes = 10 * 1024 * 1024
/** All images on one turn together. */
export const maxTurnAttachmentBytes = 20 * 1024 * 1024

const mimeByExtension: Record<string, string> = {
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
}

/** The extension's declared mime type (`image/png`, …), or `undefined` for an unsupported extension. */
export function mimeForPath(path: string): string | undefined {
  const dot = path.lastIndexOf(".")
  if (dot < 0) return undefined
  return mimeByExtension[path.slice(dot).toLowerCase()]
}

/** Whether `path`'s extension is one of the four supported image types. */
export function isImagePath(path: string): boolean {
  return mimeForPath(path) !== undefined
}

/** The file name (last path segment) shown in the transcript and sent to the model. */
export function attachmentName(path: string): string {
  return path.split("/").at(-1) || path
}

/**
 * Every `@path` mention in submitted text, in appearance order: an `@` at the
 * start of the text or after white space, then a run of non-space
 * characters. Mirrors `mentionAt`/`insertMention` (composer/mention.ts),
 * which is what puts these tokens in the text in the first place.
 */
export function mentionPaths(text: string): string[] {
  const paths: string[] = []
  const pattern = /(?:^|\s)@(\S+)/g
  for (let match = pattern.exec(text); match; match = pattern.exec(text)) paths.push(match[1]!)
  return paths
}

/** Image-file mentions in `text`, first occurrence order, de-duplicated. */
export function imageMentionPaths(text: string): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const path of mentionPaths(text)) {
    if (isImagePath(path) && !seen.has(path)) {
      seen.add(path)
      result.push(path)
    }
  }
  return result
}

/**
 * A pasted image path: terminals paste a file dragged into the window as its
 * path, sometimes wrapped in quotes and/or with spaces backslash-escaped
 * (`/a/b\ c.png` or `'/a/b c.png'`). Returns the unescaped path when the
 * whole paste (trimmed) is exactly one such path to a name with an image
 * extension; `undefined` for anything else (an ordinary text paste), so the
 * caller falls back to inserting the raw text. Existence on disk is checked
 * separately (this is pure text sniffing).
 */
export function pastedImagePath(text: string): string | undefined {
  const trimmed = text.trim()
  if (!trimmed || /[\r\n]/.test(trimmed)) return undefined
  let path: string
  if (trimmed.length > 1 && trimmed.startsWith("'") && trimmed.endsWith("'")) {
    path = trimmed.slice(1, -1).replace(/\\'/g, "'")
  } else if (trimmed.length > 1 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    path = trimmed.slice(1, -1).replace(/\\"/g, '"')
  } else {
    if (!/\\ /.test(trimmed) && / /.test(trimmed)) return undefined
    path = trimmed.replace(/\\(.)/g, "$1")
  }
  return isImagePath(path) ? path : undefined
}

/** One image ready to send or shown as a validation error in the composer. */
export interface AttachmentPreview {
  /** The `@path` text as it appears in the prompt (also `PromptAttachment.path`). */
  path: string
  name: string
  mime?: string
  /** Bytes, once read. */
  size?: number
  /** Standard base64 of the file bytes, once read. */
  data?: string
  /** Set instead of `data`/`size` when the file failed to read or validate. */
  error?: string
}

/** Validate one file's bytes against the server's per-attachment limits (type already checked by the caller via `isImagePath`). `undefined` on success. */
export function validateAttachmentBytes(path: string, byteLength: number): string | undefined {
  if (byteLength === 0) return `${attachmentName(path)}: file is empty`
  if (byteLength > maxAttachmentBytes) return `${attachmentName(path)}: larger than 10 MiB`
  return undefined
}

/** Whether adding `size` bytes would push the turn's attachments over the 20 MiB total. */
export function exceedsTurnBudget(existingSizes: readonly number[], size: number): boolean {
  return existingSizes.reduce((sum, item) => sum + item, 0) + size > maxTurnAttachmentBytes
}

/** `240 KB`, `1.4 MB`, `512 B`: the composer's pending-attachment row and error text. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${Math.round(kb)} KB`
  return `${(kb / 1024).toFixed(1)} MB`
}

/** One pending-attachment row, e.g. `[image] shot.png · 240 KB` (an ASCII tag: no glyph-width surprises in a narrow terminal). */
export function attachmentLabel(item: Pick<AttachmentPreview, "name" | "size" | "error">): string {
  if (item.error) return `[image] ${item.name} · ${item.error}`
  return `[image] ${item.name}${item.size !== undefined ? ` · ${formatBytes(item.size)}` : ""}`
}
