/**
 * The browser tab title follows the terminal title (ADR-0030), as a terminal
 * emulator's tab does: xterm.js reports OSC 0/2 title changes, and the page
 * shows them as `document.title`. Generic: the page does not know who set
 * the title or what it means, only that the program on the PTY set one.
 */

/** The tab title for a terminal title: an empty or blank one shows the page's own title again. */
export function pageTitle(terminalTitle: string, fallback: string): string {
  const title = terminalTitle.trim()
  return title || fallback
}
