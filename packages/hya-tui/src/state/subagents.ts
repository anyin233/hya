/** Descendant sessions only: previewing a child never changes the open conversation. */
import type { AppState } from "./store"
import { childSessionIds, childStatus } from "./members"

export interface SubagentRow { id: string; label: string; depth: number; status: string; activity?: string; handle?: string; archived?: boolean; archiveReason?: string }
/** Whether a live subagent of the open session is running a turn. */
export function subagentsWorking(state: AppState): boolean {
  return subagentRows(state, { includeArchived: false }).some((row) => row.status === "running")
}

/** Hidden actors stay available to background refreshes so revival can restore them. */
export function subagentRows(state: AppState, options: { includeArchived?: boolean } = {}): SubagentRow[] {
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
      ?? (session?.parent ? state.children.get(session.parent)?.members?.find((row) => row.child === id) : undefined)
    // The session row's `busy` is pushed live (`sessionUpdated`); the polled
    // child snapshot can predate the turn it is running now.
    const polled = state.children.get(id)
    const child = session ? { ...polled, busy: session.busy ?? polled?.busy ?? false } : polled
    rows.push({ id, depth, label: member?.description || session?.title || member?.agent || session?.agent || id,
      status: state.interactions.some((ask) => ask.session === id) ? "waiting" : childStatus(member, child), activity: child?.activity,
      handle: member?.handle, archived: member?.archived, archiveReason: member?.archiveReason })
    for (const nested of state.sessions) if (nested.parent === id) visit(nested.id, depth + 1)
  }
  for (const id of direct) visit(id, 0)
  return (options.includeArchived ?? state.subagentsShowArchived) ? rows : rows.filter((row) => row.archived !== true)
}
