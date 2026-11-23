/**
 * Paste and attachment chips (docs/tui.md "Paste and attachments"; p2
 * paste design): a large paste, a file the backend cannot read, or a pasted
 * image becomes one token in the input — `[paste #1 · 150 lines]`,
 * `[file #2 notes.md]`, `[image #3 shot 1.png]` — backed by an entry of the
 * composer's chip table. The token is plain text, so history, the external
 * editor, undo, and a queued prompt carry it like any other text; a token that
 * is no longer in the text (deleted, or cut) drops its attachment, and
 * Backspace or Delete next to a token removes it whole.
 *
 * Submitting expands the tokens still present, in order of appearance and
 * numbered 1..K per kind: a paste or file becomes an `<attachment>` block
 * (`wrap`) or its raw text, an image becomes `[image #N: name]` in the text and
 * a `PromptAttachment` in the same order. The `hya.v1` turn contract does not
 * change.
 */

export type ChipKind = "paste" | "file" | "image"

export interface ChipImage {
  mime: string
  /** Standard base64 of the bytes. */
  data: string
  size: number
}

export interface Chip {
  kind: ChipKind
  /** Unique for this process: tokens of history entries keep their meaning. */
  id: number
  /** The exact text in the input. */
  token: string
  /** A paste's or file's text. */
  text?: string
  /** File or image name (last path segment). */
  name?: string
  /** Where a file or image came from (this machine). */
  path?: string
  image?: ChipImage
  /** Why it cannot be sent (shown in the warning color; Enter refuses). */
  error?: string
}

/** Thresholds and wrapping (`[composer.paste]` of `tui.toml`). */
export interface PasteOptions {
  /** More lines than this fold into a chip; 0 never folds. */
  collapse_lines: number
  /** Or more characters than this. */
  collapse_chars: number
  /** Expand into `<attachment>` blocks (true) or the raw text. */
  wrap: boolean
}

export const defaultPasteOptions: PasteOptions = { collapse_lines: 10, collapse_chars: 1000, wrap: true }

const lineCount = (text: string): number => text === "" ? 0 : text.split("\n").length - (text.endsWith("\n") ? 1 : 0)

/** Whether a sanitized text paste folds into a chip. */
export function foldsPaste(text: string, options: PasteOptions): boolean {
  if (options.collapse_lines <= 0) return false
  return lineCount(text) > options.collapse_lines || text.length > options.collapse_chars
}

/**
 * Clean a text paste before it is inserted: drop ANSI escape sequences, turn
 * CRLF and CR into LF, NFC-normalize, and remove control characters other
 * than line feeds and tabs. Tabs stay; a chip keeps its text otherwise as is.
 */
export function sanitizePaste(text: string): string {
  return text
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
    .replace(/\r\n?/g, "\n")
    .normalize("NFC")
    // eslint-disable-next-line no-control-regex
    .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "")
}

/** `line 1: const value = …` for the attachment row: the first non-blank line, cut. */
export function firstLine(text: string, width = 48): string {
  const line = text.split("\n").find((item) => item.trim()) ?? ""
  const trimmed = line.trim().replace(/\t/g, " ")
  return trimmed.length > width ? `${trimmed.slice(0, width - 1)}…` : trimmed
}

const quoteAttr = (value: string): string => value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;")

/** One expanded attachment of a submitted prompt. */
export interface ExpandedImage {
  name: string
  mime: string
  data: string
  path: string
}

export interface Expanded {
  text: string
  images: ExpandedImage[]
  /** Chips that were present, in order. */
  chips: Chip[]
}

export class ChipTable {
  private readonly chips = new Map<number, Chip>()
  private nextId = 1

  /** Every chip ever created by id (history entries may still name an old one). */
  get(id: number): Chip | undefined { return this.chips.get(id) }

  private add(chip: Omit<Chip, "id" | "token">, label: (id: number) => string): Chip {
    const id = this.nextId++
    const full: Chip = { ...chip, id, token: label(id) }
    this.chips.set(id, full)
    return full
  }

  addPaste(text: string): Chip {
    const lines = lineCount(text)
    return this.add({ kind: "paste", text }, (id) => `[paste #${id} · ${lines > 1 ? `${lines} lines` : `${text.length} chars`}]`)
  }

  addFile(path: string, text: string): Chip {
    const name = baseName(path)
    return this.add({ kind: "file", text, name, path }, (id) => `[file #${id} ${name}]`)
  }

  addImage(path: string, image: ChipImage | undefined, error?: string): Chip {
    const name = baseName(path)
    return this.add({ kind: "image", name, path, ...(image ? { image } : {}), ...(error ? { error } : {}) }, (id) => `[image #${id} ${name}]`)
  }

  /** Chips whose token is in `text`, in order of first appearance. */
  present(text: string): Chip[] {
    const found: { at: number; chip: Chip }[] = []
    for (const chip of this.chips.values()) {
      const at = text.indexOf(chip.token)
      if (at >= 0) found.push({ at, chip })
    }
    return found.sort((left, right) => left.at - right.at).map((item) => item.chip)
  }

  /** The token that ends exactly at `cursor` (Backspace), as a range, or `undefined`. */
  tokenEndingAt(text: string, cursor: number): { start: number; end: number } | undefined {
    for (const chip of this.present(text)) {
      for (let at = text.indexOf(chip.token); at >= 0; at = text.indexOf(chip.token, at + 1)) {
        if (at + chip.token.length === cursor) return { start: at, end: cursor }
      }
    }
    return undefined
  }

  /** The token that starts exactly at `cursor` (Delete). */
  tokenStartingAt(text: string, cursor: number): { start: number; end: number } | undefined {
    for (const chip of this.present(text)) {
      if (text.startsWith(chip.token, cursor)) return { start: cursor, end: cursor + chip.token.length }
    }
    return undefined
  }

  /** The token around `cursor` (strictly inside it), for a click or `/paste expand`. */
  tokenAround(text: string, cursor: number): { chip: Chip; start: number; end: number } | undefined {
    for (const chip of this.present(text)) {
      for (let at = text.indexOf(chip.token); at >= 0; at = text.indexOf(chip.token, at + 1)) {
        if (cursor > at && cursor < at + chip.token.length) return { chip, start: at, end: at + chip.token.length }
      }
    }
    return undefined
  }

  /**
   * Expand the present tokens for sending: pastes and files become
   * `<attachment kind="paste" id="1" lines="150">…</attachment>` (or their raw
   * text without `wrap`), images `[image #N: name]` plus an attachment, each
   * kind numbered 1..K in order of appearance.
   */
  expand(text: string, options: Pick<PasteOptions, "wrap">): Expanded {
    const chips = this.present(text)
    const counters: Record<ChipKind, number> = { paste: 0, file: 0, image: 0 }
    const images: ExpandedImage[] = []
    let out = text
    for (const chip of chips) {
      const number = ++counters[chip.kind]
      let replacement: string
      if (chip.kind === "image") {
        replacement = `[image #${number}: ${chip.name ?? "image"}]`
        if (chip.image) images.push({ name: chip.name ?? "image", mime: chip.image.mime, data: chip.image.data, path: chip.path ?? chip.name ?? "image" })
      } else {
        const body = chip.text ?? ""
        if (!options.wrap) replacement = body
        else if (chip.kind === "paste") replacement = `<attachment kind="paste" id="${number}" lines="${lineCount(body)}">\n${body}${body.endsWith("\n") ? "" : "\n"}</attachment>`
        else replacement = `<attachment kind="file" name="${quoteAttr(chip.name ?? "file")}" lines="${lineCount(body)}">\n${body}${body.endsWith("\n") ? "" : "\n"}</attachment>`
      }
      out = out.split(chip.token).join(replacement)
    }
    return { text: out, images, chips }
  }
}

function baseName(path: string): string {
  return path.replace(/\/+$/, "").split("/").at(-1) || path
}

/** One fold of a sent `<attachment>` block in a transcript user message. */
export interface FoldedAttachment {
  kind: "paste" | "file"
  label: string
  start: number
  end: number
}

const attachmentBlock = /<attachment kind="(paste|file)"((?: [a-z]+="[^"]*")*)>\n[\s\S]*?\n?<\/attachment>/g

/**
 * The `<attachment>` blocks this TUI wrote into a user message, with their
 * folded labels (`[paste #1 · 150 lines]`, `[file notes.md · 12 lines]`).
 * Display only: the stored text is unchanged.
 */
export function foldedAttachments(text: string): FoldedAttachment[] {
  const found: FoldedAttachment[] = []
  for (const match of text.matchAll(attachmentBlock)) {
    const attrs = Object.fromEntries([...match[2]!.matchAll(/ ([a-z]+)="([^"]*)"/g)].map((item) => [item[1]!, item[2]!]))
    const kind = match[1] as "paste" | "file"
    const lines = attrs.lines ? ` · ${attrs.lines} line${attrs.lines === "1" ? "" : "s"}` : ""
    const label = kind === "paste" ? `[paste #${attrs.id ?? "?"}${lines}]` : `[file ${(attrs.name ?? "file").replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&amp;/g, "&")}${lines}]`
    found.push({ kind, label, start: match.index!, end: match.index! + match[0].length })
  }
  return found
}

/** `text` with every `<attachment>` block folded to its label. */
export function foldAttachments(text: string): string {
  let out = ""
  let at = 0
  for (const fold of foldedAttachments(text)) {
    out += text.slice(at, fold.start) + fold.label
    at = fold.end
  }
  return out + text.slice(at)
}
