/**
 * Terminal-control stripping for text from another process (the relay
 * bridge's stderr lines and readiness fields, src/bridge.ts) before the TUI
 * shows or keeps it: such text may carry escape sequences from the remote
 * side (a relay's error text, a link's room name).
 */

/**
 * ESC sequences (CSI, OSC, DCS/SOS/PM/APC, two- and three-byte escapes) and
 * their 8-bit C1 forms. An unterminated string sequence runs to the end of
 * the text; a lone ESC matches on its own.
 */
const escapeSequence = new RegExp([
  // CSI: ESC [ or 0x9b, parameters, intermediates, final byte.
  "(?:\\x1b\\[|\\x9b)[0-?]*[ -/]*[@-~]?",
  // OSC: ESC ] or 0x9d, up to BEL, ST (ESC \\ or 0x9c), or the end.
  "(?:\\x1b\\]|\\x9d)[^\\x07\\x1b\\x9c]*(?:\\x07|\\x1b\\\\|\\x9c)?",
  // DCS, SOS, PM, APC: ESC P/X/^/_ or 0x90/0x98/0x9e/0x9f, up to ST or the end.
  "(?:\\x1b[PX^_]|[\\x90\\x98\\x9e\\x9f])[^\\x1b\\x9c]*(?:\\x1b\\\\|\\x9c)?",
  // Other escapes: ESC, intermediates, final byte (ESC 7, ESC ( B, ...), or a lone ESC.
  "\\x1b[ -/]*[0-~]?",
].join("|"), "g")

/** C0 controls except TAB, DEL, and C1 controls. */
const controlCharacter = /[\x00-\x08\x0a-\x1f\x7f-\x9f]/g

/**
 * `text` without terminal control sequences or characters: whole ESC / CSI /
 * OSC / DCS sequences (7- and 8-bit), C0 controls except TAB (CR and LF
 * too: callers pass single lines), DEL, and C1 controls.
 */
export function stripTerminalControls(text: string): string {
  return text.replace(escapeSequence, "").replace(controlCharacter, "")
}
