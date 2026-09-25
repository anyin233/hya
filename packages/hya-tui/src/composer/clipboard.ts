/**
 * Clipboard copy (docs/tui.md "Copy"): the TUI writes the text to the
 * terminal as an OSC 52 sequence (`CliRenderer.copyToClipboardOSC52`), and
 * the terminal — xterm.js in the WebUI, or a local terminal that allows it —
 * puts it on the system clipboard. It works over SSH too, since the
 * sequence travels with the output.
 */

/** Status line text after a copy: `Copied N chars`, or why nothing was sent. */
export function copyNotice(text: string, sent: boolean): string {
  if (!sent) return "Copy failed: this terminal does not accept OSC 52 clipboard writes"
  const count = [...text].length
  return `Copied ${count} char${count === 1 ? "" : "s"}`
}
