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
import { toolCard } from "./tools"

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

/** The child session and member of a task card: by `callId`, else by the child session from the task output. */
export function taskLink(card: { callId?: string; child?: string }, members: readonly MemberInfo[]): { child?: string; member?: MemberInfo } {
  const member = (card.callId ? members.find((row) => row.callId === card.callId) : undefined)
    ?? (card.child ? members.find((row) => row.child === card.child) : undefined)
  const child = card.child ?? member?.child
  return { ...(child ? { child } : {}), ...(member ? { member } : {}) }
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
      const text = part.text?.text.split("\n").map((line) => line.trim()).find(Boolean)
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
      const child = toolCard(part.toolCall).task?.child
      if (child) ids.add(child)
    }
  }
  return [...ids]
}
