/**
 * The composer's attachment rows (docs/tui.md "Paste and attachments"): one
 * borderless row per chip in the input and per pending `@path` image, right
 * above the input, in the chip's identity color; at most `maxAttachmentRows`,
 * the rest folded into `+N more`. A row with an error is in the warning color
 * and Enter refuses to send.
 */
import { formatBytes, type AttachmentPreview } from "./attachments"
import { firstLine, type Chip, type ChipKind } from "./chips"

export const maxAttachmentRows = 4

export interface ChipRow {
  /** `[paste #1]`, `[image #2]`, `[image]` (a pending `@path`), `+2 more`. */
  tag: string
  detail: string
  kind?: ChipKind
  error?: boolean
  /** The chip id a click expands (pastes and files). */
  chip?: number
}

const lines = (text: string): number => text === "" ? 0 : text.split("\n").length - (text.endsWith("\n") ? 1 : 0)

/** The row of one chip. */
export function chipRow(chip: Chip): ChipRow {
  const tag = `[${chip.kind} #${chip.id}]`
  if (chip.kind === "paste") {
    const count = lines(chip.text ?? "")
    return { tag, kind: "paste", chip: chip.id, detail: `${count > 1 ? `${count} lines` : `${(chip.text ?? "").length} chars`} · ${firstLine(chip.text ?? "")}` }
  }
  if (chip.kind === "file") {
    const text = chip.text ?? ""
    return { tag, kind: "file", chip: chip.id, detail: `${chip.name} · ${lines(text)} line${lines(text) === 1 ? "" : "s"} · ${formatBytes(Buffer.byteLength(text))}` }
  }
  if (chip.error) return { tag, kind: "image", error: true, detail: `${chip.name} · ${chip.error}` }
  return { tag, kind: "image", detail: `${chip.name}${chip.image ? ` · ${formatBytes(chip.image.size)}` : ""}` }
}

/** Chip rows then pending `@path` image rows, capped at `max` rows. */
export function chipRows(chips: readonly Chip[], previews: readonly AttachmentPreview[], max = maxAttachmentRows): ChipRow[] {
  const rows = [
    ...chips.map(chipRow),
    ...previews.map((item): ChipRow => ({
      tag: "[image]",
      detail: item.error ? `${item.name} · ${item.error}` : `${item.name}${item.size !== undefined ? ` · ${formatBytes(item.size)}` : ""}`,
      ...(item.error ? { error: true } : {}),
    })),
  ]
  if (rows.length <= max) return rows
  const shown = rows.slice(0, max - 1)
  const hidden = rows.slice(max - 1)
  return [...shown, { tag: `+${hidden.length} more`, detail: hidden.some((row) => row.error) ? "· one has an error" : "", ...(hidden.some((row) => row.error) ? { error: true } : {}) }]
}
