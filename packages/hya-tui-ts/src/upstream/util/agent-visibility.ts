/**
 * TUI selector visibility: role-mapped mode `primary` (Bundle role `main`) only.
 * `hidden` is never a second selector rule.
 */
export function isTuiSelectableAgent(agent: { mode: string }): boolean {
  return agent.mode === "primary"
}

/**
 * Subagent autocomplete over v2 `/api/agent` rows.
 * Mode is non-primary (role subagent) and wire `hidden` encodes ordinary
 * can_spawn reachability from the same catalog authority.
 */
export function isSubagentAutocompleteAgent(agent: {
  mode: string
  hidden?: boolean
}): boolean {
  return agent.mode !== "primary" && !agent.hidden
}
