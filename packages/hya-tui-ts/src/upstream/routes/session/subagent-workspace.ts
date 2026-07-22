export type MemberRunStatus = "spawning" | "running" | "done" | "failed" | "cancelled"
export type RosterStatus = "idle" | "busy" | "done" | "failed"

export type RunTreeMember = {
  member: string
  child?: string
  subagent_type: string
  description: string
  depth: number
  status: MemberRunStatus
  summary: string
}

export type RosterEntry = {
  handle: string
  session: string
  agent_type: string
  mode: "transient" | "resident"
  status: RosterStatus
  current_task?: string
}

export type RunTreeNode = {
  session?: string
  agent?: string
  model?: string
  title?: string
  member?: RunTreeMember
  roster?: RosterEntry
  children: RunTreeNode[]
}

type LifecyclePresentation = {
  label: "Working" | "Finished" | "Failed" | "Cancelled" | "Idle"
  working: boolean
}

export function resolveLifecyclePresentation(node: {
  member?: Pick<RunTreeMember, "status">
  roster?: Pick<RosterEntry, "status">
}): LifecyclePresentation {
  switch (node.member?.status ?? node.roster?.status) {
    case "spawning":
    case "running":
    case "busy":
      return { label: "Working", working: true }
    case "done":
      return { label: "Finished", working: false }
    case "failed":
      return { label: "Failed", working: false }
    case "cancelled":
      return { label: "Cancelled", working: false }
    default:
      return { label: "Idle", working: false }
  }
}

export class RunTreeParseError extends Error {
  constructor(path: string, message: string) {
    super(`${path}: ${message}`)
    this.name = "RunTreeParseError"
  }
}

export function parseRunTree(value: unknown): RunTreeNode {
  return parseNode(value, "tree")
}

export type RunTreeResource = {
  status: "loading" | "ready" | "error"
  tree?: RunTreeNode
  error?: Error
}

export function createRunTreeLoader(options: {
  fetchTree: (sessionID: string) => Promise<unknown>
  onTree: (tree: RunTreeNode) => void
  onState?: (state: RunTreeResource) => void
}) {
  let sessionID: string | undefined
  let generation = 0
  let state: RunTreeResource = { status: "loading" }
  let running: Promise<void> | undefined
  let queued = false
  const update = (next: RunTreeResource) => {
    state = next
    options.onState?.(state)
  }

  async function loadOnce() {
    if (!sessionID) return
    const current = sessionID
    const requestGeneration = generation
    update({ status: "loading", tree: state.tree })
    try {
      const value = await options.fetchTree(current)
      if (requestGeneration !== generation || current !== sessionID) return
      const tree = parseRunTree(value)
      update({ status: "ready", tree })
      options.onTree(tree)
    } catch (error) {
      if (requestGeneration !== generation || current !== sessionID) return
      update({ status: "error", tree: state.tree, error: asError(error) })
    }
  }

  return {
    get state() {
      return state
    },
    setSession(next: string) {
      sessionID = next
      generation++
      update({ status: "loading" })
    },
    refresh() {
      if (running) {
        queued = true
        return running
      }
      running = (async () => {
        do {
          queued = false
          await loadOnce()
        } while (queued)
      })().finally(() => {
        running = undefined
      })
      return running
    },
  }
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error))
}

export type RunTreeRow = {
  node: RunTreeNode
  depth: number
  selectable: boolean
  searchText: string
}

export function flattenRunTree(tree: RunTreeNode): RunTreeRow[] {
  const rows: RunTreeRow[] = []
  const visit = (node: RunTreeNode, depth: number) => {
    rows.push({
      node,
      depth,
      // Depth-0 Main is selectable so the roster can return focus to Main after a split.
      selectable: node.session !== undefined,
      searchText: [
        node.roster?.handle,
        node.roster?.agent_type ?? node.member?.subagent_type ?? node.agent,
        node.roster?.status ?? node.member?.status,
        node.roster?.current_task,
        node.member?.description,
      ]
        .filter(Boolean)
        .join(" "),
    })
    for (const child of node.children) visit(child, depth + 1)
  }
  visit(tree, 0)
  return rows
}

export function treeSessionIDs(tree: RunTreeNode): Set<string> {
  return new Set(flattenRunTree(tree).flatMap((row) => (row.node.session ? [row.node.session] : [])))
}

export type RunTreeEventEffect = { refresh: boolean }

export function runTreeEventEffect(value: unknown): RunTreeEventEffect {
  const none = { refresh: false }
  if (!isRecord(value) || !isRecord(value.properties) || typeof value.type !== "string") return none
  if (["session.created", "session.updated", "session.deleted"].includes(value.type)) {
    return { refresh: true }
  }
  if (value.type !== "hya.envelope" || !isRecord(value.properties.event)) return none
  const event = value.properties.event
  if (typeof event.type !== "string" || typeof event.session !== "string") return none

  switch (event.type) {
    case "member_spawned":
      return { refresh: true }
    case "member_status_changed":
      if (typeof event.member !== "string" || !isMemberStatus(event.status)) return none
      return { refresh: true }
    case "member_finished":
      if (
        typeof event.member !== "string" ||
        !isMemberStatus(event.status) ||
        (event.child !== undefined && typeof event.child !== "string")
      )
        return none
      return { refresh: true }
    case "agent_registered":
      if (
        typeof event.agent_session !== "string" ||
        typeof event.handle !== "string" ||
        typeof event.agent_type !== "string" ||
        !isOneOf(event.mode, ["transient", "resident"])
      )
        return none
      return { refresh: true }
    case "agent_activity_changed":
      if (
        typeof event.handle !== "string" ||
        !isRosterStatus(event.status) ||
        (event.current_task !== undefined && typeof event.current_task !== "string")
      )
        return none
      return { refresh: true }
    default:
      return none
  }
}

function isMemberStatus(value: unknown): value is MemberRunStatus {
  return isOneOf(value, ["spawning", "running", "done", "failed", "cancelled"])
}

function isRosterStatus(value: unknown): value is RosterStatus {
  return isOneOf(value, ["idle", "busy", "done", "failed"])
}

function isOneOf<const T extends readonly string[]>(value: unknown, values: T): value is T[number] {
  return typeof value === "string" && values.includes(value)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value)
}

export type MainPane = { type: "main"; id: "main" }
export type ObservationPane = { type: "observation"; id: string; sessionID: string }
export type WorkspacePane = MainPane | ObservationPane | SplitPane
export type SplitPane = {
  type: "split"
  axis: "vertical" | "horizontal"
  first: WorkspacePane
  second: WorkspacePane
}
export type WorkspaceTab = { id: string; root: WorkspacePane }
export type WorkspaceState = {
  mainSessionID: string
  tabs: WorkspaceTab[]
  activeTabID: string
  focusedPaneID: string
  /**
   * Stable open order of observation session IDs. Cycle / pane-strip navigation
   * use this so promoting a subagent into the Main split never reorders focus.
   */
  observationOrder: string[]
}
export type WorkspaceAction =
  | { type: "close"; paneID: string }
  | { type: "openTab"; sessionID: string }
  | { type: "openSplit"; axis: "vertical" | "horizontal"; sessionID: string }
  | { type: "focus"; paneID: string }
  | { type: "focusMain" }
  | { type: "cycleFocus"; direction?: 1 | -1 }
  | { type: "reconcileSessions"; sessionIDs: string[] }

/** One entry in the session pane strip (tab bar) for click / cycle navigation. */
export type WorkspacePaneStripEntry = {
  paneID: string
  focused: boolean
}

/**
 * Focus targets in stable order: Main first, then observations in open order.
 * Used by the tab bar and reverse/forward cycle.
 */
export function workspaceFocusOrder(state: WorkspaceState): Array<MainPane | ObservationPane> {
  const present = new Map(
    workspaceLeaves(state)
      .filter((pane): pane is ObservationPane => pane.type === "observation")
      .map((pane) => [pane.sessionID, pane] as const),
  )
  const ordered: Array<MainPane | ObservationPane> = [{ type: "main", id: "main" }]
  for (const sessionID of state.observationOrder) {
    const pane = present.get(sessionID)
    if (pane) ordered.push(pane)
  }
  for (const [sessionID, pane] of present) {
    if (!state.observationOrder.includes(sessionID)) ordered.push(pane)
  }
  return ordered
}

export function workspacePaneStrip(state: WorkspaceState): WorkspacePaneStripEntry[] {
  return workspaceFocusOrder(state).map((pane) => ({
    paneID: pane.id,
    focused: pane.id === state.focusedPaneID,
  }))
}

export function createWorkspaceState(mainSessionID: string): WorkspaceState {
  return {
    mainSessionID,
    tabs: [{ id: "main", root: { type: "main", id: "main" } }],
    activeTabID: "main",
    focusedPaneID: "main",
    observationOrder: [],
  }
}

export function workspaceLeaves(state: WorkspaceState): Array<MainPane | ObservationPane> {
  return state.tabs.flatMap((tab) => paneLeaves(tab.root))
}

function rememberObservation(order: string[], sessionID: string): string[] {
  return order.includes(sessionID) ? order : [...order, sessionID]
}

export function reduceWorkspace(state: WorkspaceState, action: WorkspaceAction): WorkspaceState {
  switch (action.type) {
    case "close":
      return action.paneID === "main" ? state : removeFromWorkspace(state, action.paneID)
    case "focus":
      return focusPane(state, action.paneID)
    case "focusMain":
      return focusPane(state, "main")
    case "cycleFocus": {
      const leaves = workspaceFocusOrder(state)
      if (leaves.length === 0) return state
      const direction = action.direction ?? 1
      const current = leaves.findIndex((pane) => pane.id === state.focusedPaneID)
      const start = current === -1 ? 0 : current
      // Add length before modulo so reverse cycle stays non-negative.
      const next = (start + direction + leaves.length) % leaves.length
      return focusPane(state, leaves[next]?.id ?? "main")
    }
    case "reconcileSessions": {
      const valid = new Set(action.sessionIDs)
      let next = state
      for (const pane of workspaceLeaves(state)) {
        if (pane.type === "observation" && !valid.has(pane.sessionID)) {
          next = removeFromWorkspace(next, pane.id)
        }
      }
      return next
    }
    case "openTab": {
      const existing = workspaceLeaves(state).find(
        (pane) => pane.type === "observation" && pane.sessionID === action.sessionID,
      )
      if (existing) return focusPane(state, existing.id)
      const pane: ObservationPane = {
        type: "observation",
        id: `observation:${action.sessionID}`,
        sessionID: action.sessionID,
      }
      const tab = { id: `tab:${action.sessionID}`, root: pane }
      return {
        ...state,
        tabs: [...state.tabs, tab],
        activeTabID: tab.id,
        focusedPaneID: pane.id,
        observationOrder: rememberObservation(state.observationOrder, action.sessionID),
      }
    }
    case "openSplit": {
      // ADR-0003: a split always shows Main beside one observation on the main tab.
      // Never nest into the currently focused observation — that hid Main and broke
      // subsequent agent switching / focusMain after Ctrl+X V.
      return openSplitBesideMain(state, action.sessionID, action.axis)
    }
  }
}

/**
 * Put `sessionID` as the observation beside Main on the main tab.
 *
 * ADR-0003: never nest under another observation. Prior main-tab observations and
 * other open subagents are retained as tabs so users can navigate Main ↔ A ↔ B
 * without losing open views.
 */
function openSplitBesideMain(
  state: WorkspaceState,
  sessionID: string,
  axis: "vertical" | "horizontal",
): WorkspaceState {
  const pane: ObservationPane = {
    type: "observation",
    id: `observation:${sessionID}`,
    sessionID,
  }
  // Preserve open order of every other observation (tabs + prior split partner).
  // Use observationOrder first so cycle order stays stable across promotions.
  const retained = new Set(
    workspaceLeaves(state)
      .filter((leaf): leaf is ObservationPane => leaf.type === "observation" && leaf.sessionID !== sessionID)
      .map((leaf) => leaf.sessionID),
  )
  const otherSessions = [
    ...state.observationOrder.filter((id) => retained.has(id)),
    ...[...retained].filter((id) => !state.observationOrder.includes(id)),
  ]

  const mainTab: WorkspaceTab = {
    id: "main",
    root: {
      type: "split",
      axis,
      first: { type: "main", id: "main" },
      second: pane,
    },
  }
  const otherTabs: WorkspaceTab[] = otherSessions.map((id) => ({
    id: `tab:${id}`,
    root: {
      type: "observation",
      id: `observation:${id}`,
      sessionID: id,
    },
  }))

  return {
    ...state,
    tabs: [mainTab, ...otherTabs],
    activeTabID: "main",
    focusedPaneID: pane.id,
    observationOrder: rememberObservation(state.observationOrder, sessionID),
  }
}

function mainSplitAxis(state: WorkspaceState): "vertical" | "horizontal" | undefined {
  const main = state.tabs.find((tab) => tab.id === "main")
  return main?.root.type === "split" ? main.root.axis : undefined
}

function paneLeaves(pane: WorkspacePane): Array<MainPane | ObservationPane> {
  return pane.type === "split" ? [...paneLeaves(pane.first), ...paneLeaves(pane.second)] : [pane]
}

/**
 * Focus a leaf. When the main tab is in split mode, focusing any open observation
 * promotes it beside Main (Rust-style active_aux) so Left/Right/cycle always keep
 * Main visible while switching subagents.
 */
function focusPane(state: WorkspaceState, paneID: string): WorkspaceState {
  if (paneID === "main") {
    const main = state.tabs.find((tab) => tab.id === "main")
    return main ? { ...state, activeTabID: "main", focusedPaneID: "main" } : state
  }

  const leaf = workspaceLeaves(state).find((pane) => pane.id === paneID)
  if (!leaf) return state
  if (leaf.type !== "observation") {
    return paneID === state.focusedPaneID ? state : { ...state, focusedPaneID: paneID }
  }

  const axis = mainSplitAxis(state)
  if (axis) {
    const main = state.tabs.find((tab) => tab.id === "main")
    const alreadyBesideMain =
      main !== undefined && paneLeaves(main.root).some((pane) => pane.id === paneID)
    if (alreadyBesideMain) {
      return { ...state, activeTabID: "main", focusedPaneID: paneID }
    }
    // Promote this observation into the Main | observation split.
    return openSplitBesideMain(state, leaf.sessionID, axis)
  }

  if (paneID === state.focusedPaneID) return state
  const tab = state.tabs.find((candidate) => paneLeaves(candidate.root).some((pane) => pane.id === paneID))
  return tab ? { ...state, activeTabID: tab.id, focusedPaneID: paneID } : state
}

function replacePane(pane: WorkspacePane, paneID: string, replace: (pane: WorkspacePane) => WorkspacePane): WorkspacePane {
  if (pane.type !== "split") return pane.id === paneID ? replace(pane) : pane
  const first = replacePane(pane.first, paneID, replace)
  if (first !== pane.first) return { ...pane, first }
  const second = replacePane(pane.second, paneID, replace)
  return second === pane.second ? pane : { ...pane, second }
}

function removeFromWorkspace(state: WorkspaceState, paneID: string): WorkspaceState {
  const tabs = state.tabs.flatMap((tab) => {
    const root = removePane(tab.root, paneID)
    return root ? [{ ...tab, root }] : []
  })
  if (tabs.length === state.tabs.length && tabs.every((tab, index) => tab.root === state.tabs[index]?.root)) return state
  const removedSessionID = paneID.startsWith("observation:") ? paneID.slice("observation:".length) : undefined
  const observationOrder = removedSessionID
    ? state.observationOrder.filter((id) => id !== removedSessionID)
    : state.observationOrder
  const focusExists = tabs.some((tab) => paneLeaves(tab.root).some((pane) => pane.id === state.focusedPaneID))
  const focusedPaneID = focusExists ? state.focusedPaneID : "main"
  const focusedTab = tabs.find((tab) => paneLeaves(tab.root).some((pane) => pane.id === focusedPaneID))
  return { ...state, tabs, observationOrder, focusedPaneID, activeTabID: focusedTab?.id ?? "main" }
}

function removePane(pane: WorkspacePane, paneID: string): WorkspacePane | undefined {
  if (pane.type !== "split") return pane.id === paneID ? undefined : pane
  const first = removePane(pane.first, paneID)
  const second = removePane(pane.second, paneID)
  if (!first) return second
  if (!second) return first
  return first === pane.first && second === pane.second ? pane : { ...pane, first, second }
}

function parseNode(value: unknown, path: string): RunTreeNode {
  const input = record(value, path)
  const session = optionalString(input.session, `${path}.session`)
  const member = input.member === undefined ? undefined : parseMember(input.member, `${path}.member`)
  if (!session && !member) throw new RunTreeParseError(path, "expected session or member")
  const children = input.children === undefined ? [] : array(input.children, `${path}.children`)
  return {
    session,
    agent: optionalString(input.agent, `${path}.agent`),
    model: optionalString(input.model, `${path}.model`),
    title: optionalString(input.title, `${path}.title`),
    member,
    roster: input.roster === undefined ? undefined : parseRoster(input.roster, `${path}.roster`),
    children: children.map((child, index) => parseNode(child, `${path}.children[${index}]`)),
  }
}

function parseMember(value: unknown, path: string): RunTreeMember {
  const input = record(value, path)
  return {
    member: string(input.member, `${path}.member`),
    child: optionalString(input.child, `${path}.child`),
    subagent_type: string(input.subagent_type, `${path}.subagent_type`),
    description: string(input.description, `${path}.description`),
    depth: number(input.depth, `${path}.depth`),
    status: oneOf(input.status, ["spawning", "running", "done", "failed", "cancelled"], `${path}.status`),
    summary: optionalString(input.summary, `${path}.summary`) ?? "",
  }
}

function parseRoster(value: unknown, path: string): RosterEntry {
  const input = record(value, path)
  return {
    handle: string(input.handle, `${path}.handle`),
    session: string(input.session, `${path}.session`),
    agent_type: string(input.agent_type, `${path}.agent_type`),
    mode: oneOf(input.mode, ["transient", "resident"], `${path}.mode`),
    status: oneOf(input.status, ["idle", "busy", "done", "failed"], `${path}.status`),
    current_task: optionalString(input.current_task, `${path}.current_task`),
  }
}

function record(value: unknown, path: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new RunTreeParseError(path, "expected object")
  return value as Record<string, unknown>
}

function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) throw new RunTreeParseError(path, "expected array")
  return value
}

function string(value: unknown, path: string): string {
  if (typeof value !== "string") throw new RunTreeParseError(path, "expected string")
  return value
}

function optionalString(value: unknown, path: string): string | undefined {
  return value === undefined ? undefined : string(value, path)
}

function number(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) throw new RunTreeParseError(path, "expected number")
  return value
}

function oneOf<const T extends readonly string[]>(value: unknown, values: T, path: string): T[number] {
  if (typeof value !== "string" || !values.includes(value)) throw new RunTreeParseError(path, "unexpected value")
  return value
}
