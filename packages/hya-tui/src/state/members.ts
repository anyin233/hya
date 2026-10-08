/**
 * Subagents (members) of the open session and the links from `task` tool
 * cards to their child sessions (docs/protocol/README.md "Subagents").
 *
 * - `memberUpdated` frames fold by `member`; partial frames (status, finish)
 *   keep the fields they do not carry. `SessionInfo.members` seeds the rows
 *   on open, so a reconnect mid-task still has them.
 * - A task card links to its member by `callId`; a resident spawn records
 *   no call id, so the card's child session from the task output
 *   (`metadata.sessionId`) links it too.
 * - The child's status: a finished member status (done / failed /
 *   cancelled) wins; otherwise the child session's `busy` flag (read by the
 *   controller while the child is tracked) says running or idle.
 *
 * Pure TypeScript (no Solid).
 */
import type { MemberInfo, MessageInfo } from "../client"
import { toolCard, type TaskInfo, type TaskMemberInfo } from "./tools"

/** What the controller last read about a child session. */
export interface ChildState {
  /** `SessionInfo.busy`: a run owns the child session now. */
  busy: boolean
  /** Newest tool call or text line of the child (`childActivity`). */
  activity?: string
  /** The child's newest assistant message failed. */
  failed?: boolean
  /** Agent bound to the child session. */
  agent?: string
}

export type ChildStatus = "starting" | "running" | "idle" | "done" | "failed" | "cancelled"

/** Fold one member frame into the rows (by `member`); empty fields keep the known value. */
export function foldMember(rows: readonly MemberInfo[], update: MemberInfo): MemberInfo[] {
  const index = rows.findIndex((row) => row.member === update.member)
  const known = Object.fromEntries(Object.entries(update).filter(([, value]) => value !== undefined && value !== "" && value !== 0))
  if (index < 0) return [...rows, { ...known, member: update.member }]
  return rows.map((row, at) => at === index ? { ...row, ...known } : row)
}

/** An explicit child must match exactly; use the call ID only before a child is known. */
export function taskLink(card: { callId?: string; child?: string }, members: readonly MemberInfo[]): { child?: string; member?: MemberInfo } {
  const member = card.child ? members.find((row) => row.child === card.child)
    : card.callId ? members.find((row) => row.callId === card.callId) : undefined
  const child = card.child ?? member?.child
  return { ...(child ? { child } : {}), ...(member ? { member } : {}) }
}

/** One independently navigable task member, reconciled with live roster state. */
export interface TaskMemberLink extends TaskMemberInfo {
  member?: MemberInfo
}

export function taskLinks(task: TaskInfo, callId: string | undefined, members: readonly MemberInfo[]): TaskMemberLink[] {
  const spawned = callId ? members.filter((row) => row.callId === callId) : []
  const entries: TaskMemberInfo[] = task.members ?? (spawned.length > 1 ? spawned.map((row) => ({
    agent: row.agent || task.agent, description: row.description ?? "", child: row.child,
  })) : [task])
  const used = new Set<MemberInfo>()
  const links = entries.map((entry, index): TaskMemberLink => {
    const member = entry.child ? members.find((row) => row.child === entry.child)
      : entry.name ? members.find((row) => row.handle === entry.name || row.member === entry.name)
      : !entry.status ? spawned[index] : undefined
    if (member) used.add(member)
    return { ...entry, ...(member ? {
      member, child: entry.child || member.child, name: member.handle || entry.name,
      agent: member.agent || entry.agent, description: entry.description || member.description || "",
    } : {}) }
  })
  for (const member of spawned) {
    if (!used.has(member)) links.push({ agent: member.agent || task.agent, description: member.description ?? "", child: member.child, name: member.handle, member })
  }
  return links
}

/** Launch-result fallback while live roster/child state has not arrived. */
export function taskChildStatus(link: TaskMemberLink, child: ChildState | undefined): ChildStatus {
  if (link.member || child) return childStatus(link.member, child)
  switch (link.status) {
    case "running": return "running"
    case "done": case "completed": return "done"
    case "error": case "failed": return "failed"
    case "cancelled": return "cancelled"
    default: return "starting"
  }
}

/** The child's status for its task card. */
export function childStatus(member: MemberInfo | undefined, child: Pick<ChildState, "busy" | "failed"> | undefined): ChildStatus {
  switch (member?.status) {
    case "MEMBER_STATUS_DONE": return "done"
    case "MEMBER_STATUS_FAILED": return "failed"
    case "MEMBER_STATUS_CANCELLED": return "cancelled"
  }
  if (child) return child.busy ? "running" : child.failed ? "failed" : "idle"
  return member?.status === "MEMBER_STATUS_RUNNING" ? "running" : "starting"
}

/** The newest thing the child did: its last tool call (`tool summary`) or the first line of its last text. */
export function childActivity(messages: readonly MessageInfo[]): string | undefined {
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index]!
    if (message.role !== "ROLE_ASSISTANT") continue
    const parts = message.parts ?? []
    for (let at = parts.length - 1; at >= 0; at--) {
      const part = parts[at]!
      if (part.toolCall) {
        const card = toolCard(part.toolCall)
        return [card.tool, card.summary].filter(Boolean).join(" ")
      }
      const text = part.text?.text?.split("\n").map((line) => line.trim()).find(Boolean)
      if (text) return text
    }
  }
  return undefined
}

/** Child session ids of the open session: its members, then task outputs not covered by a member. */
export function childSessionIds(members: readonly MemberInfo[], messages: readonly MessageInfo[]): string[] {
  const ids = new Set(members.map((row) => row.child).filter((id): id is string => Boolean(id)))
  for (const message of messages) {
    for (const part of message.parts ?? []) {
      if (part.toolCall?.tool !== "task") continue
      const task = toolCard(part.toolCall).task
      if (task?.child) ids.add(task.child)
      for (const member of task?.members ?? []) if (member.child) ids.add(member.child)
    }
  }
  return [...ids]
}
