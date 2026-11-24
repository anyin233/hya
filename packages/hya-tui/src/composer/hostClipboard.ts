/**
 * Reading the system clipboard for Ctrl+V, `/paste`, and an empty bracketed
 * paste (docs/tui.md "Paste and attachments"): an image first, else text,
 * through OpenTUI's native `createHostClipboard()`. Over SSH the clipboard
 * belongs to the SSH host, not to the machine you type on, so it is never
 * read there; the result says what to do instead.
 */
import { createHostClipboard, type HostClipboardService } from "@opentui/core"

export type ClipboardPaste =
  | { kind: "image"; mime: string; bytes: Uint8Array }
  | { kind: "text"; text: string }
  | { kind: "none"; reason: string }

/** Image types first (the server accepts these four), then text. */
export const clipboardTypes = ["image/png", "image/jpeg", "image/webp", "image/gif", "text/plain"] as const

/** A TUI started from an SSH login: `SSH_CONNECTION`, `SSH_TTY`, or `SSH_CLIENT` is set. */
export function inSshSession(env: Record<string, string | undefined>): boolean {
  return Boolean(env.SSH_CONNECTION || env.SSH_TTY || env.SSH_CLIENT)
}

export const sshClipboardHint = "The clipboard here belongs to the SSH host, not your machine · text: use your terminal's paste key · images: paste in the WebUI, or run hya --connect on your machine"
export const noClipboardHint = "No system clipboard here (no display server) · text: use your terminal's paste key · images: paste in the WebUI"

/** What a path that is missing here means over SSH: it is on the user's own machine. */
export function sshPathHint(shown: string): string {
  return `${shown} is on your machine, not on the SSH host · upload it in the WebUI, or copy it over with scp`
}

let host: HostClipboardService | undefined

/** Read the clipboard: an image, else text; never throws. `read` replaces the host clipboard in tests. */
export async function readClipboard(
  env: Record<string, string | undefined>,
  read: (types: readonly [string, ...string[]]) => ReturnType<HostClipboardService["read"]> = (types) => (host ??= createHostClipboard({ timeoutMs: 3000 })).read({ preferredTypes: types }),
): Promise<ClipboardPaste> {
  if (inSshSession(env)) return { kind: "none", reason: sshClipboardHint }
  let result: Awaited<ReturnType<HostClipboardService["read"]>>
  try {
    result = await read(clipboardTypes as unknown as readonly [string, ...string[]])
  } catch (error) {
    return { kind: "none", reason: `Clipboard read failed: ${error instanceof Error ? error.message : String(error)}` }
  }
  switch (result.status) {
    case "read": {
      const { mimeType, bytes } = result.representation
      if (mimeType.startsWith("image/")) return { kind: "image", mime: mimeType, bytes }
      return { kind: "text", text: new TextDecoder().decode(bytes) }
    }
    case "empty": return { kind: "none", reason: "The clipboard is empty" }
    case "unsupported": return { kind: "none", reason: noClipboardHint }
    case "timed-out": return { kind: "none", reason: "The clipboard did not answer in time" }
    case "limit-exceeded": return { kind: "none", reason: "The clipboard content is too large to paste" }
    case "cancelled": return { kind: "none", reason: "Clipboard read cancelled" }
    case "failed": return { kind: "none", reason: `Clipboard read failed: ${result.error.message}` }
  }
}
