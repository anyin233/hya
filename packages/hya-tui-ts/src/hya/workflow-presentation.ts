const TEXT_LIMIT = 24

export type WorkflowAvailability = "available" | "stale" | "unavailable"
export type WorkflowRunStatus = "running" | "completed" | "failed" | "cancelled" | "interrupted"
export type WorkflowStageStatus = "pending" | "running" | "completed" | "failed" | "cancelled" | "skipped"

type WorkflowIdentity = {
  source: string
  name: string
  revision: string
}

type WorkflowMember = {
  member: string
  role: "worker" | "verifier"
  iteration: number
}

type SessionMember = {
  member: string
  status: "spawning" | "running"
  work: string
}

type WorkflowStage = {
  id: string
  title?: string
  agent: string
  mode: string
  level: number
  status: WorkflowStageStatus
  members: WorkflowMember[]
}

type WorkflowRun = {
  id: string
  workflow: WorkflowIdentity
  request_hash: string
  owner: string
  status: WorkflowRunStatus
  stages: WorkflowStage[]
  error?: string
}

export type WorkflowProjection = {
  selection?: WorkflowIdentity
  run?: WorkflowRun
  availability?: WorkflowAvailability
}

export type WorkflowPresentation = {
  state: "none" | "ready" | WorkflowRunStatus | WorkflowAvailability | "invalid"
  name: string
  revision?: string
  tone: "muted" | "info" | "success" | "warning" | "error"
  agentProgress?: string
  stageProgress?: string
  levelProgress?: string
  activeStages?: string
  currentWork?: string
}

/** Decode the typed Workflow projection attached to synchronized Session state. */
export function parseWorkflowProjection(value: unknown): WorkflowProjection | undefined {
  if (value === undefined || value === null) return undefined
  const input = object(value, "workflow")
  return {
    selection: optional(input.selection, parseIdentity, "workflow.selection"),
    run: optional(input.run, parseRun, "workflow.run"),
    availability: optionalEnum(input.availability, ["available", "stale", "unavailable"], "workflow.availability"),
  }
}

/** Derive bounded, deterministic sidebar text from one synchronized Workflow projection. */
export function presentWorkflow(value: unknown, membersValue: unknown = []): WorkflowPresentation {
  let workflow: WorkflowProjection | undefined
  let members: SessionMember[]
  try {
    workflow = parseWorkflowProjection(value)
    members = parseWorkflowActivity(membersValue)
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error)
    return { state: "invalid", name: "invalid", tone: "error", currentWork: truncate(detail, TEXT_LIMIT) }
  }
  if (!workflow?.selection) return { state: "none", name: "none", tone: "muted" }
  const base = {
    name: workflow.selection.name,
    revision: workflow.selection.revision.slice(0, 8),
  }
  if (workflow.availability === "stale") return { ...base, state: "stale", tone: "warning" }
  if (workflow.availability === "unavailable") return { ...base, state: "unavailable", tone: "error" }
  const run =
    workflow.run &&
    workflow.run.workflow.source === workflow.selection.source &&
    workflow.run.workflow.name === workflow.selection.name &&
    workflow.run.workflow.revision === workflow.selection.revision
      ? workflow.run
      : undefined
  if (!run) return { ...base, state: "ready", tone: "info" }

  const running: WorkflowStage[] = []
  const memberIds = new Set<string>()
  const memberById = new Map(members.map((member) => [member.member, member]))
  const activeMemberIds = new Set<string>()
  const levels = new Set<number>()
  let completedStages = 0
  let firstRunning: WorkflowStage | undefined
  let firstPending: WorkflowStage | undefined
  let firstFailed: WorkflowStage | undefined
  let currentMemberWork: string | undefined
  for (const stage of run.stages) {
    levels.add(stage.level)
    if (stage.status === "completed") completedStages += 1
    if (stage.status === "pending") firstPending ??= stage
    if (stage.status === "failed") firstFailed ??= stage
    if (stage.status === "running") {
      running.push(stage)
      firstRunning ??= stage
    }
    for (const entry of stage.members) {
      memberIds.add(entry.member)
      const member = memberById.get(entry.member)
      if (member) {
        activeMemberIds.add(entry.member)
        if (stage.status === "running") currentMemberWork ??= member.work || undefined
      }
    }
  }
  const current = firstRunning ?? (run.status === "failed" ? firstFailed : undefined) ?? firstPending
  return {
    ...base,
    state: run.status,
    tone: toneForStatus(run.status),
    agentProgress: `${activeMemberIds.size}/${memberIds.size} agents`,
    stageProgress: `${completedStages}/${run.stages.length} stages`,
    levelProgress: current ? `level ${current.level + 1}/${levels.size}` : undefined,
    activeStages: compactActiveStages(running),
    currentWork: currentMemberWork
      ? truncate(currentMemberWork, TEXT_LIMIT)
      : current
        ? truncate(current.title || current.id, TEXT_LIMIT)
        : undefined,
  }
}

/** Map durable run state to the existing semantic theme roles. */
function toneForStatus(status: WorkflowRunStatus): WorkflowPresentation["tone"] {
  switch (status) {
    case "running":
      return "info"
    case "completed":
      return "success"
    case "failed":
      return "error"
    case "cancelled":
    case "interrupted":
      return "warning"
  }
}

/** Compact declaration-ordered running Stage ids for the fixed-width sidebar. */
function compactActiveStages(stages: WorkflowStage[]): string | undefined {
  const first = stages[0]?.id
  if (!first) return undefined
  return stages.length === 1 ? first : `${first} +${stages.length - 1}`
}

/** Truncate the lowest-priority work label to the sidebar's Unicode-scalar budget. */
function truncate(value: string, limit: number): string {
  const characters = Array.from(value)
  return characters.length <= limit ? value : `${characters.slice(0, limit - 1).join("")}…`
}

/** Decode one exact Workflow identity. */
function parseIdentity(value: unknown, path: string): WorkflowIdentity {
  const input = object(value, path)
  return {
    source: string(input.source, `${path}.source`),
    name: string(input.name, `${path}.name`),
    revision: string(input.revision, `${path}.revision`),
  }
}

/** Decode the newest durable Workflow run. */
function parseRun(value: unknown, path: string): WorkflowRun {
  const input = object(value, path)
  return {
    id: string(input.id, `${path}.id`),
    workflow: parseIdentity(input.workflow, `${path}.workflow`),
    request_hash: string(input.request_hash, `${path}.request_hash`),
    owner: string(input.owner, `${path}.owner`),
    status: enumValue(input.status, ["running", "completed", "failed", "cancelled", "interrupted"], `${path}.status`),
    stages: array(input.stages, `${path}.stages`).map((stage, index) => parseStage(stage, `${path}.stages[${index}]`)),
    error: optionalString(input.error, `${path}.error`),
  }
}

/** Decode one declaration-ordered Stage projection. */
function parseStage(value: unknown, path: string): WorkflowStage {
  const input = object(value, path)
  return {
    id: string(input.id, `${path}.id`),
    title: optionalString(input.title, `${path}.title`),
    agent: string(input.agent, `${path}.agent`),
    mode: string(input.mode, `${path}.mode`),
    level: integer(input.level, `${path}.level`),
    status: enumValue(input.status, ["pending", "running", "completed", "failed", "cancelled", "skipped"], `${path}.status`),
    members: array(input.members ?? [], `${path}.members`).map((entry, index) => parseMember(entry, `${path}.members[${index}]`)),
  }
}

/** Decode one Stage-to-member reference. */
function parseMember(value: unknown, path: string): WorkflowMember {
  const input = object(value, path)
  return {
    member: string(input.member, `${path}.member`),
    role: enumValue(input.role, ["worker", "verifier"], `${path}.role`),
    iteration: integer(input.iteration, `${path}.iteration`),
  }
}

/** Decode bounded active rows joined to Workflow member references. */
function parseWorkflowActivity(value: unknown): SessionMember[] {
  return array(value, "workflowActivity").map((entry, index) => {
    const path = `workflowActivity[${index}]`
    const input = object(entry, path)
    return {
      member: string(input.member, `${path}.member`),
      status: enumValue(input.status, ["spawning", "running"], `${path}.status`),
      work: optionalString(input.work, `${path}.work`) ?? "",
    }
  })
}

/** Require a non-array object at a wire path. */
function object(value: unknown, path: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${path}: expected object`)
  return value as Record<string, unknown>
}

/** Require an array at a wire path. */
function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`${path}: expected array`)
  return value
}

/** Require a string at a wire path. */
function string(value: unknown, path: string): string {
  if (typeof value !== "string") throw new Error(`${path}: expected string`)
  return value
}

/** Decode an optional wire string. */
function optionalString(value: unknown, path: string): string | undefined {
  return value === undefined || value === null ? undefined : string(value, path)
}

/** Require a non-negative safe integer at a wire path. */
function integer(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error(`${path}: expected non-negative integer`)
  return value
}

/** Require one value from a closed wire enum. */
function enumValue<const T extends readonly string[]>(value: unknown, allowed: T, path: string): T[number] {
  if (typeof value !== "string" || !allowed.includes(value)) throw new Error(`${path}: invalid value`)
  return value as T[number]
}

/** Decode an optional value from a closed wire enum. */
function optionalEnum<const T extends readonly string[]>(value: unknown, allowed: T, path: string): T[number] | undefined {
  return value === undefined || value === null ? undefined : enumValue(value, allowed, path)
}

/** Decode an optional nested wire value. */
function optional<T>(value: unknown, parse: (value: unknown, path: string) => T, path: string): T | undefined {
  return value === undefined || value === null ? undefined : parse(value, path)
}
