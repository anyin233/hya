/**
 * Permission and question prompts (docs/tui.md "Permission and question
 * prompts"): which pending interactions the open view asks about, the view
 * model of one prompt, its option keys, and the `RespondInteraction` body of
 * each choice.
 *
 * - The queue holds the asks of the open session and of its subagent
 *   sessions (children by `SessionInfo.parent`, and the open session's
 *   members and `task` outputs before the session list knows them), oldest
 *   first. One is shown at a time (`1 of N`).
 * - A permission prompt renders the waiting call like its tool card
 *   (`toolCard()` over `payload.tool` / `payload.input`): bash the command,
 *   edit / write / patch the path and a diff, read / webfetch the path or
 *   URL, anything else compact arguments; an ask without a correlated call
 *   shows its resource.
 * - Keys reach a prompt only while the input is empty, so typing can never
 *   answer one by accident. Digits choose at once; Up/Down move; Enter
 *   chooses the highlighted option; Esc denies (a permission) or rejects (a
 *   question) — it never approves. With text in the input, a question takes
 *   Enter as its free-text answer (a `/command` still runs).
 *
 * Pure TypeScript (no Solid).
 */
import type { Interaction, MemberInfo, MessageInfo, SessionInfo, StreamEvent } from "../client"
import type { KeyLike } from "../keys/bindings"
import { childSessionIds } from "./members"
import { toolCard, type ToolLine } from "./tools"

export type PromptChoice =
  | { kind: "allowOnce" }
  | { kind: "allowAlways" }
  | { kind: "deny" }
  | { kind: "answer"; answer: string }
  /** "Other…": the answer is typed into the input. */
  | { kind: "other" }
  | { kind: "reject" }

export interface PromptOption {
  label: string
  /** Muted text after the label (what "Always allow" covers). */
  detail?: string
  choice: PromptChoice
}

export interface PromptView {
  id: string
  kind: "permission" | "question"
  /** Session that asks. */
  session: string
  /** Permission: `<action> <resource>`; question: the question. */
  title: string
  /** Question header (`ask_user` `header`). */
  header?: string
  /** The prompt's first line: permission `<tool>  <summary>` like its tool card (else the title); question `<header>: <question>`. */
  headline: string
  /** Permission: the tool card summary of the waiting call. */
  summary?: string
  /** Tool of the waiting call. */
  tool?: string
  /** Who asks: the agent, or `subagent <agent> · <task>`. */
  asker: string
  subagent: boolean
  /** Details of the waiting call (permission), clipped to `promptBodyLines`. */
  body: ToolLine[]
  options: PromptOption[]
  /** 0-based position in the queue and its length (`1 of N`). */
  position: number
  total: number
}

/** What a prompt needs from the store. */
export interface PromptContext {
  selected: SessionInfo | undefined
  sessions: readonly SessionInfo[]
  /** Members of the open session. */
  members: readonly MemberInfo[]
  /** Projection of the open session. */
  messages: readonly MessageInfo[]
}

export type PromptKeyResult =
  | { type: "none" }
  | { type: "move"; index: number }
  | { type: "choose"; choice: PromptChoice }

export type RespondBody =
  | { permission: { allowed: boolean; persist: boolean } }
  | { question: { answer: string } }
  | { question: { rejected: true } }

/** Most detail lines a permission prompt shows (head and tail around a hidden-lines row). */
export const promptBodyLines = 8

type Json = Record<string, unknown>

function record(value: unknown): Json {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Json : {}
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined
}

export function isQuestion(interaction: Interaction): boolean {
  return interaction.type?.includes("QUESTION") ?? false
}

const hiddenRow = /^… (\d+) lines hidden$/

/** Clip to `max` lines, keeping head and tail; an already clipped body keeps its hidden count. */
function clipBody(lines: ToolLine[], max = promptBodyLines): ToolLine[] {
  if (lines.length <= max) return lines
  const tail = Math.ceil((max - 1) / 2)
  const head = max - 1 - tail
  const hidden = lines.slice(head, lines.length - tail).reduce((count, line) => {
    const match = line.tone === "muted" ? hiddenRow.exec(line.text) : null
    return count + (match ? Number(match[1]) : 1)
  }, 0)
  return [...lines.slice(0, head), { text: `… ${hidden} lines hidden`, tone: "muted" }, ...lines.slice(lines.length - tail)]
}

/** The open session and every session below it: `parent` links, plus `direct` children the list may not have yet. */
export function treeSessionIds(root: string, sessions: readonly SessionInfo[], direct: readonly string[] = []): Set<string> {
  const tree = new Set([root, ...direct])
  let grew = true
  while (grew) {
    grew = false
    for (const session of sessions) {
      if (session.parent && tree.has(session.parent) && !tree.has(session.id)) {
        tree.add(session.id)
        grew = true
      }
    }
  }
  return tree
}

/** Pending asks of the open session's tree, in list order (oldest first). */
export function promptQueue(interactions: readonly Interaction[], context: PromptContext): Interaction[] {
  const selected = context.selected
  if (!selected) return []
  const tree = treeSessionIds(selected.id, context.sessions, childSessionIds(context.members, context.messages))
  return interactions.filter((item) => item.session !== undefined && tree.has(item.session))
}

/** What a session waits for: `approval` (a permission ask) or `answer` (a question), if anything. */
export function waitingKind(interactions: readonly Interaction[], session: string): "approval" | "answer" | undefined {
  const asks = interactions.filter((item) => item.session === session)
  if (!asks.length) return undefined
  return asks.some((item) => !isQuestion(item)) ? "approval" : "answer"
}

/**
 * A listing merged with what live frames carried: a listed row keeps the
 * options, header (`detail`), and payload of the frame with its id (the
 * listing omits a question's options). Ids answered here stay hidden, so a
 * listing read before the answer landed cannot bring them back.
 */
export function mergeInteractions(listed: readonly Interaction[], known: ReadonlyMap<string, Interaction>, resolved: ReadonlySet<string>): Interaction[] {
  return listed.filter((row) => !resolved.has(row.id)).map((row) => {
    const live = known.get(row.id)
    if (!live) return row
    return {
      ...row,
      ...(!row.options?.length && live.options?.length ? { options: live.options } : {}),
      ...(!row.detail && live.detail ? { detail: live.detail } : {}),
      ...(!row.payload && live.payload ? { payload: live.payload } : {}),
    }
  })
}

/** Who asks: the agent of the open session, or the subagent (with its task description). */
function asker(session: string, context: PromptContext): { asker: string; subagent: boolean } {
  const selected = context.selected
  const listed = context.sessions.find((row) => row.id === session)
  const member = context.members.find((row) => row.child === session)
  if (selected && session === selected.id && !selected.parent) return { asker: selected.agent || "agent", subagent: false }
  const agent = member?.agent || listed?.agent || (session === selected?.id ? selected.agent : "")
  const label = ["subagent", agent].filter(Boolean).join(" ")
  return { asker: member?.description ? `${label} · ${member.description}` : label, subagent: true }
}

/** The first question of a waiting `ask_user` call whose text is `title`: header and option labels. */
function askedQuestion(title: string, messages: readonly MessageInfo[]): { header?: string; options: string[] } | undefined {
  for (let index = messages.length - 1; index >= 0; index--) {
    for (const part of messages[index]!.parts ?? []) {
      const call = part.toolCall
      if (!call || (call.tool !== "ask_user" && call.tool !== "question")) continue
      if (call.state === "TOOL_EXECUTION_STATE_OK" || call.state === "TOOL_EXECUTION_STATE_ERROR") continue
      let input: Json
      try {
        input = record(JSON.parse(call.inputJson ?? ""))
      } catch {
        continue
      }
      const first = record(Array.isArray(input.questions) ? input.questions[0] : undefined)
      if (str(first.question) !== title) continue
      const options = (Array.isArray(first.options) ? first.options : [])
        .map((option) => typeof option === "string" ? option : str(record(option).label))
        .filter((label): label is string => Boolean(label))
      const header = str(first.header)
      return { ...(header ? { header } : {}), options }
    }
  }
  return undefined
}

type PromptBase = Omit<PromptView, "kind" | "title" | "headline" | "body" | "options">

function permissionView(interaction: Interaction, base: PromptBase): PromptView {
  const payload = record(interaction.payload)
  const action = str(payload.action) ?? ""
  const resource = str(payload.resource) ?? ""
  const tool = str(payload.tool)
  const always = (Array.isArray(payload.always) ? payload.always : []).filter((item): item is string => typeof item === "string" && item !== "")
  const title = interaction.title || [action, resource].filter(Boolean).join(" ") || "permission"
  let body: ToolLine[] = []
  let summary: string | undefined
  if (tool) {
    const input = payload.input === undefined ? undefined : record(payload.input)
    const card = toolCard({ tool, state: "TOOL_EXECUTION_STATE_RUNNING", ...(input ? { inputJson: JSON.stringify(input) } : {}) })
    summary = card.summary
    body = card.body.length ? card.body : card.summary ? [{ text: card.summary, tone: "fg" }] : []
  }
  if (!body.length && resource) body = [{ text: resource, tone: "fg" }]
  const covers = always.length ? always.join(", ") : resource
  return {
    ...base,
    kind: "permission",
    title,
    headline: tool && summary ? `${tool}  ${summary}` : title,
    ...(summary ? { summary } : {}),
    ...(tool ? { tool } : {}),
    body: clipBody(body),
    options: [
      { label: "Allow once", choice: { kind: "allowOnce" } },
      { label: "Always allow", ...(covers ? { detail: action ? `${action}: ${covers}` : covers } : {}), choice: { kind: "allowAlways" } },
      { label: "Deny", choice: { kind: "deny" } },
    ],
  }
}

function questionView(interaction: Interaction, context: PromptContext, base: PromptBase): PromptView {
  const fallback = interaction.options?.length ? undefined : askedQuestion(interaction.title, context.messages)
  const labels = interaction.options?.length ? interaction.options : fallback?.options ?? []
  const header = interaction.detail || fallback?.header
  const title = interaction.title || "Question"
  return {
    ...base,
    kind: "question",
    title,
    headline: header ? `${header}: ${title}` : title,
    ...(header ? { header } : {}),
    body: [],
    options: [
      ...labels.map((label): PromptOption => ({ label, choice: { kind: "answer", answer: label } })),
      { label: "Other…", detail: "type the answer in the input, Enter sends", choice: { kind: "other" } },
      { label: "Reject", choice: { kind: "reject" } },
    ],
  }
}

/** The view model of one pending ask at `position` of a queue of `total`. */
export function promptView(interaction: Interaction, context: PromptContext, position: number, total: number): PromptView {
  const session = interaction.session ?? ""
  const base = { id: interaction.id, session, ...asker(session, context), position, total }
  return isQuestion(interaction) ? questionView(interaction, context, base) : permissionView(interaction, base)
}

function isEnter(key: KeyLike): boolean {
  return key.name === "return" || key.name === "enter" || key.name === "kpenter"
}

/**
 * One key while a prompt is shown (no list open). `index` is the highlighted
 * option; `draft` the input's text. Keys other than the prompt's are `none`
 * and go to the input.
 */
export function promptKey(view: PromptView, { index, draft }: { index: number; draft: string }, key: KeyLike): PromptKeyResult {
  if (key.ctrl || key.meta || key.shift) return { type: "none" }
  const text = draft.trim()
  if (text) {
    if (view.kind === "question" && isEnter(key) && !text.startsWith("/")) return { type: "choose", choice: { kind: "answer", answer: text } }
    return { type: "none" }
  }
  const count = view.options.length
  const digit = /^[1-9]$/.test(key.sequence) ? Number(key.sequence) : /^[1-9]$/.test(key.name) ? Number(key.name) : undefined
  if (digit !== undefined) {
    const option = view.options[digit - 1]
    return option ? { type: "choose", choice: option.choice } : { type: "none" }
  }
  if (key.name === "up" || key.name === "down") {
    const step = key.name === "up" ? -1 : 1
    return { type: "move", index: (((index + step) % count) + count) % count }
  }
  if (isEnter(key)) {
    const option = view.options[Math.min(Math.max(0, index), count - 1)]
    return option ? { type: "choose", choice: option.choice } : { type: "none" }
  }
  if (key.name === "escape") return { type: "choose", choice: view.kind === "question" ? { kind: "reject" } : { kind: "deny" } }
  return { type: "none" }
}

/** The `RespondInteraction` body of a choice; `undefined` for "Other…" (nothing is sent yet). */
export function respondBody(choice: PromptChoice): RespondBody | undefined {
  switch (choice.kind) {
    case "allowOnce": return { permission: { allowed: true, persist: false } }
    case "allowAlways": return { permission: { allowed: true, persist: true } }
    case "deny": return { permission: { allowed: false, persist: false } }
    case "answer": return { question: { answer: choice.answer } }
    case "reject": return { question: { rejected: true } }
    case "other": return undefined
  }
}

/** The prompt shown now: the oldest ask of the open session's tree, or `undefined`. */
export function currentPrompt(state: PromptContext & { interactions: readonly Interaction[] }): { interaction: Interaction; view: PromptView } | undefined {
  const queue = promptQueue(state.interactions, state)
  const interaction = queue[0]
  return interaction ? { interaction, view: promptView(interaction, state, 0, queue.length) } : undefined
}

/**
 * How the controller routes one frame of the open session's stream, which
 * is subscribed with `includeDescendants=true`: `own` frames (the open
 * session's, or with no session) are folded as usual; a descendant's
 * (subagent's) `permissionRequested` / `questionRequested` /
 * `interactionResolved` is `descendantAsk` (only the pending list changes);
 * any other frame of another session is ignored.
 */
export function askFrameRoute(event: StreamEvent, sessionId: string): "own" | "descendantAsk" | "ignore" {
  if (!event.session || event.session === sessionId) return "own"
  if (event.permissionRequested || event.questionRequested || event.interactionResolved) return "descendantAsk"
  return "ignore"
}
