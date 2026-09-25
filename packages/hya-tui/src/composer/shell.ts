/**
 * `!command` input: the rest of the line runs as a `ShellTurn` (the backend's
 * builtin shell tool, no model round) instead of a prompt.
 */

/** Whether the input is in shell mode (starts with `!`); drives the composer's indicator. */
export function isShellInput(text: string): boolean {
  return text.trimStart().startsWith("!")
}

/** The shell command of a `!command` input (`""` for a bare `!`), or `undefined` for other input. */
export function shellCommand(text: string): string | undefined {
  const trimmed = text.trim()
  return trimmed.startsWith("!") ? trimmed.slice(1).trim() : undefined
}
