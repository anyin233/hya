/** Canonical product name used in titles, branding, and audit surface. */
export const PRODUCT_NAME = "hya"

/** Command id for the status dialog (`/status` / palette). */
export const STATUS_COMMAND = "hya.status"

/** Default theme id selected when no user theme is configured. */
export const DEFAULT_THEME = "hya"

/** Default sound pack id for TUI audio cues. */
export const DEFAULT_SOUND_PACK = "hya.default"

/** Temporary filename stem for clipboard image capture. */
export const CLIPBOARD_TEMP_NAME = "hya-clipboard.png"

/**
 * Build the terminal window title string.
 *
 * @param title - Optional session or context suffix after the product name
 * @returns `"hya"` alone, or `"hya | <title>"` when `title` is provided
 */
export function terminalTitle(title?: string) {
  return title ? `${PRODUCT_NAME} | ${title}` : PRODUCT_NAME
}
