/** Descendant sessions only: previewing a child never changes the open conversation. */
import type { AppState } from "./store"
import { childSessionIds, childStatus } from "./members"

export interface SubagentRow { id: string; label: string; depth: number; status: string; activity?: string }
export function subagentRows(state: AppState): SubagentRow[] {
  const root = state.selected?.id
  if (!root) return []
  const sessions = new Map(state.sessions.map((session) => [session.id, session]))
  const direct = new Set(childSessionIds(state.members, state.messages))
  for (const session of state.sessions) if (session.parent === root) direct.add(session.id)
  const seen = new Set([root]), rows: SubagentRow[] = []
  const visit = (id: string, depth: number) => {
    if (seen.has(id)) return
    seen.add(id)
    const session = sessions.get(id), member = state.members.find((row) => row.child === id)
    const child = state.children.get(id) ?? (session ? { busy: session.busy === true } : undefined)
    rows.push({ id, depth, label: member?.description || session?.title || member?.agent || session?.agent || id,
      status: state.interactions.some((ask) => ask.session === id) ? "waiting" : childStatus(member, child), activity: child?.activity })
    for (const nested of state.sessions) if (nested.parent === id) visit(nested.id, depth + 1)
  }
  for (const id of direct) visit(id, 0)
  return rows
}
