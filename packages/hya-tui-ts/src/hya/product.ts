export const PRODUCT_NAME = "hya"
export const STATUS_COMMAND = "hya.status"
export const DEFAULT_THEME = "hya"
export const DEFAULT_SOUND_PACK = "hya.default"
export const CLIPBOARD_TEMP_NAME = "hya-clipboard.png"

export function terminalTitle(title?: string) {
  return title ? `${PRODUCT_NAME} | ${title}` : PRODUCT_NAME
}
