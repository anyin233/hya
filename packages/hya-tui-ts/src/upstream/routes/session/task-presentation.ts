import type { RunTreeNode } from "./subagent-workspace"

/** One launched subagent row rendered inside the main assistant message. */
export type TaskMemberView = {
  /** Child session id when known (metadata, outcome, or run tree). */
  sessionId?: string
  subagentType: string
  description: string
  background: boolean
  /** Terminal outcome status when the tool has finished. */
  status?: string
  summary?: string
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function stringField(value: unknown, key: string): string | undefined {
  if (!isRecord(value)) return undefined
  const field = value[key]
  return typeof field === "string" && field.length > 0 ? field : undefined
}

/**
 * Build the list of subagent rows to show for a `task` tool part.
 *
 * Prefers explicit multi-member metadata/output, then a single top-level task,
 * then `input.members` while the call is still running.
 */
export function resolveTaskMembers(options: {
  input: Record<string, unknown>
  metadata: Record<string, unknown>
  output?: string
  background?: boolean
}): TaskMemberView[] {
  const background = options.background === true || options.metadata.background === true

  const fromMetadata = membersFromValue(options.metadata.members, background)
  if (fromMetadata.length > 0) return fromMetadata

  const fromOutput = membersFromToolOutput(options.output, background)
  if (fromOutput.length > 0) return fromOutput

  const fromInput = membersFromValue(options.input.members, background)
  if (fromInput.length > 0) return fromInput

  const description = stringField(options.input, "description")
  const subagentType = stringField(options.input, "subagent_type") ?? "general"
  if (!description) return []

  return [
    {
      sessionId:
        stringField(options.metadata, "sessionId") ?? stringField(options.metadata, "session"),
      subagentType,
      description,
      background,
      status: stringField(options.metadata, "status"),
    },
  ]
}

/**
 * Resolve a child session id for a task row: prefer explicit ids, else match the
 * live run tree by short description and/or subagent type.
 */
export function resolveTaskSessionId(
  member: TaskMemberView,
  tree: RunTreeNode | undefined,
): string | undefined {
  if (member.sessionId) return member.sessionId
  if (!tree) return undefined
  const matches = flattenMemberNodes(tree).filter((node) => {
    const description = node.member?.description
    const type = node.member?.subagent_type ?? node.roster?.agent_type ?? node.agent
    const descriptionMatch =
      !!member.description && !!description && description === member.description
    const typeMatch = !!member.subagentType && !!type && type === member.subagentType
    return descriptionMatch || (typeMatch && !member.description)
  })
  // Prefer exact description match when several share a type.
  const exact = matches.find((node) => node.member?.description === member.description)
  const hit = exact ?? (matches.length === 1 ? matches[0] : undefined)
  return hit?.session ?? hit?.member?.child ?? hit?.roster?.session
}

/** Collect every direct and nested member node that may have a session. */
export function flattenMemberNodes(tree: RunTreeNode): RunTreeNode[] {
  const rows: RunTreeNode[] = []
  const visit = (node: RunTreeNode) => {
    if (node.member || (node.session && node !== tree)) rows.push(node)
    for (const child of node.children) visit(child)
  }
  for (const child of tree.children) visit(child)
  return rows
}

/**
 * When the main agent has launched subagents that are not yet represented by a
 * task tool part (or a multi-member part is still pending input), surface the
 * live run-tree members so the main message still shows status.
 */
export function launchedMembersFromTree(
  tree: RunTreeNode | undefined,
  coveredSessionIds: Set<string>,
): TaskMemberView[] {
  if (!tree) return []
  const rows: TaskMemberView[] = []
  for (const node of flattenMemberNodes(tree)) {
    const sessionId = node.session ?? node.member?.child ?? node.roster?.session
    if (sessionId && coveredSessionIds.has(sessionId)) continue
    rows.push({
      sessionId,
      subagentType: node.roster?.agent_type ?? node.member?.subagent_type ?? node.agent ?? "general",
      description: node.roster?.current_task ?? node.member?.description ?? node.title ?? "subagent",
      background: false,
      status: node.member?.status ?? node.roster?.status,
      summary: node.member?.summary,
    })
  }
  return rows
}

function membersFromValue(value: unknown, background: boolean): TaskMemberView[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((entry) => {
    if (!isRecord(entry)) return []
    const description =
      stringField(entry, "description") ?? stringField(entry, "member") ?? "subagent"
    const subagentType = stringField(entry, "subagent_type") ?? "general"
    return [
      {
        sessionId: stringField(entry, "sessionId") ?? stringField(entry, "session"),
        subagentType,
        description,
        background,
        status: stringField(entry, "status"),
        summary: stringField(entry, "summary"),
      },
    ]
  })
}

function membersFromToolOutput(output: string | undefined, background: boolean): TaskMemberView[] {
  if (!output) return []
  const trimmed = output.trim()
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return []
  try {
    const parsed = JSON.parse(trimmed) as unknown
    if (isRecord(parsed) && Array.isArray(parsed.members)) {
      return membersFromValue(parsed.members, background)
    }
    if (Array.isArray(parsed)) return membersFromValue(parsed, background)
  } catch {
    return []
  }
  return []
}
