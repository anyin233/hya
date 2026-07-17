// Derived from opencode (MIT); see package LICENSE/UPSTREAM.md. Artwork replaced with hya branding.
import { EPILOGUE_ART } from "./epilogue-art.data"

const reset = "\x1b[0m"
const bold = "\x1b[1m"
const dim = "\x1b[90m"

function hexToRgb(hex: string) {
  return `${parseInt(hex.slice(1, 3), 16)};${parseInt(hex.slice(3, 5), 16)};${parseInt(hex.slice(5, 7), 16)}`
}

const ink = `\x1b[38;2;${hexToRgb(EPILOGUE_ART.color)}m`

// Render the sticker art as quadrant-glyph rows: each character already
// encodes a 2x2 sub-pixel bitmap, so every row is emitted verbatim with the
// single art color as foreground; spaces stay transparent.
function renderArt(art: { rows: string[] }) {
  return art.rows.map((row) => `  ${ink}${row}${reset}`)
}

export function sessionEpilogue(input: { title: string; sessionID?: string }) {
  const weak = (text: string) => `${dim}${text.padEnd(10, " ")}${reset}`
  return [
    ...renderArt(EPILOGUE_ART),
    "",
    `  ${dim}The 100 Agents Who Really ×∞ Want to Help You${reset}`,
    "",
    `  ${weak("Session")}${bold}${input.title}${reset}`,
    `  ${weak("Continue")}${bold}hya-ts -s ${input.sessionID}${reset}`,
    "",
  ].join("\n")
}
