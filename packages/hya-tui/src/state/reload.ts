/**
 * The `/reload` targets and the summary of a reload (pure; app/reload.ts
 * does the work, commands/reload.ts registers the commands).
 */

export const reloadTargets = ["config", "keybindings", "theme", "extensions", "bundles", "mcp", "commands", "skills", "context", "layout", "tui"] as const
export type ReloadTarget = (typeof reloadTargets)[number]

/** What `/reload` reloads, in order: everything but the layout (user state, not a resource) and the TUI's own code. */
export const reloadAllTargets: readonly ReloadTarget[] = ["config", "keybindings", "theme", "bundles", "extensions", "mcp", "commands", "skills", "context"]

/** One line per `/reload-<target>` (the command menu's description). */
export const reloadTargetDescriptions: Record<ReloadTarget, string> = {
  config: "Reload tui.toml (the same as /config reload)",
  keybindings: "Reload the custom keybindings from tui.json",
  theme: "Reload the theme from tui.json (auto follows the terminal background)",
  extensions: "Reload every bundle TUI extension of the scope",
  bundles: "Refresh bundles, plugins, MCP declarations, and the Project's bundles on the backend (RefreshBundles)",
  mcp: "Refresh the MCP declarations and show each server's state",
  commands: "Re-read the command catalog (custom commands and /skill:* entries)",
  skills: "Re-read the skill catalog and re-record the open session's skill index at its next turn",
  context: "Re-record the open session's AGENTS.md, environment, and skill index at its next turn",
  layout: "Reload the saved pane layout (the same as /layout reload)",
  tui: "Restart this TUI from its files on disk, on the open session with the unsent draft",
}

export interface ReloadOutcome {
  target: ReloadTarget
  ok: boolean
  /** What happened, e.g. `14 commands` or `bundles: hya/x: failed`. */
  text: string
}

/**
 * The notice of a reload: `Reloaded a, b, c`, then `Failed: x` per failure.
 * `Reloaded nothing` when every target failed.
 */
export function reloadSummary(outcomes: readonly ReloadOutcome[]): string {
  const done = outcomes.filter((outcome) => outcome.ok).map((outcome) => outcome.text)
  const failures = outcomes.filter((outcome) => !outcome.ok).map((outcome) => `Failed: ${outcome.text}`)
  return [done.length ? `Reloaded ${done.join(", ")}` : failures.length ? "Reloaded nothing" : "", ...failures].filter(Boolean).join("\n")
}
