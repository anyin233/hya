/**
 * Image chips from bytes (a pasted file or the clipboard): check the type by
 * header, read the size in pixels, and — with `[composer.paste]
 * image_resize` (default on) — convert a type the server does not accept
 * (BMP, TIFF, HEIC, AVIF) to PNG and scale an image over 10 MiB down until it
 * fits, with `Bun.Image`. The server accepts PNG, JPEG, GIF, and WebP up to
 * 10 MiB each (docs/protocol/README.md "Prompt attachments (images)").
 */
import { maxAttachmentBytes } from "./attachments"
import type { ChipImage } from "./chips"
import { sniffImage } from "./paths"

/** Extensions a pasted path is read as an image for (converted when needed). */
export const pastedImageExtensions = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".heic", ".heif", ".avif"]

export function isPastedImagePath(path: string): boolean {
  const dot = path.lastIndexOf(".")
  return dot >= 0 && pastedImageExtensions.includes(path.slice(dot).toLowerCase())
}

export interface PreparedImage {
  image?: ChipImage & { width?: number; height?: number }
  error?: string
}

type BunImage = {
  metadata(): Promise<{ width: number; height: number; format: string }>
  resize(width: number, height?: number, options?: { fit?: "fill" | "inside"; withoutEnlargement?: boolean }): BunImage
  png(options?: { compressionLevel?: number }): BunImage
  jpeg(options?: { quality?: number }): BunImage
  bytes(): Promise<Uint8Array>
}
type ImageConstructor = new (input: Uint8Array) => BunImage

/** `Bun.Image` when this Bun has it (1.4+). */
function imageCodec(): ImageConstructor | undefined {
  return (Bun as unknown as { Image?: ImageConstructor }).Image
}

const toChip = (bytes: Uint8Array, mime: string, size?: { width: number; height: number }): PreparedImage["image"] => ({
  mime, data: Buffer.from(bytes).toString("base64"), size: bytes.byteLength, ...(size ? { width: size.width, height: size.height } : {}),
})

/** Prepare `bytes` for sending as an image; never throws. */
export async function prepareImage(bytes: Uint8Array, options: { resize: boolean }): Promise<PreparedImage> {
  if (bytes.byteLength === 0) return { error: "empty image" }
  const mime = sniffImage(bytes)
  const Codec = imageCodec()
  let size: { width: number; height: number } | undefined
  try { if (Codec) size = await new Codec(bytes).metadata() } catch { /* unknown format: handled below */ }
  if (mime && bytes.byteLength <= maxAttachmentBytes) return { image: toChip(bytes, mime, size) }
  if (!options.resize || !Codec) {
    return { error: !mime ? "not a PNG, JPEG, GIF, or WebP image" : "larger than 10 MiB" }
  }
  if (!size) return { error: "not an image this TUI can read" }
  try {
    // A type the server does not take: the same pixels as PNG first.
    if (!mime || mime === "image/gif") {
      const png = await new Codec(bytes).png().bytes()
      if (png.byteLength <= maxAttachmentBytes) return { image: toChip(png, "image/png", size) }
    }
    // Too large: scale the long side down (JPEG for photos) until it fits.
    let long = Math.min(Math.max(size.width, size.height), 4096)
    for (let attempt = 0; attempt < 6; attempt++) {
      const scaled = await new Codec(bytes).resize(long, long, { fit: "inside", withoutEnlargement: true }).jpeg({ quality: 85 }).bytes()
      if (scaled.byteLength <= maxAttachmentBytes) {
        const shrunk = await new Codec(scaled).metadata().catch(() => undefined)
        return { image: toChip(scaled, "image/jpeg", shrunk ? { width: shrunk.width, height: shrunk.height } : undefined) }
      }
      long = Math.floor(long * 0.7)
    }
    return { error: "larger than 10 MiB even when scaled down" }
  } catch (error) {
    return { error: `could not convert: ${error instanceof Error ? error.message : String(error)}` }
  }
}
