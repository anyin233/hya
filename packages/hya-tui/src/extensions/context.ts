/**
 * The host state extensions read (`ExtensionContext`), before permission
 * scoping (wire.ts `scopeContext`). The sections behind the built-in panes
 * (`sessions`, `projects`, `todos`, `status`) carry what those panes showed
 * when the TUI drew them itself; hya/basic-tui-components draws them now.
 */
import { contextUsage, modelEffortLabel, modelReference, sessionTokens, todoStatusText, webLabel } from "../state/format"
import { effectiveMode, modeDisplay } from "../state/modes"
import { waitingKind } from "../state/prompts"
import { sessionsInScope } from "../state/projects"
import { mergeTranscript } from "../state/overlay"
import { forkSourceText } from "../state/revert"
import type { AppState } from "../state/store"
import type { ExtensionContext } from "./wire"
import { tuiVersion } from "../version"

/** What the context needs from outside the store: the shown server, and the status items extensions contribute. */
export interface ExtensionStatusFeed {
  readonly server: string
  readonly items: readonly { readonly label: string; readonly text: string; readonly priority: number }[]
}

const hostOf = (url: string): string => url.replace(/^https?:\/\//, "").replace(/\/$/, "")
type TodoStatus = NonNullable<ExtensionContext["todos"]>[number]["status"]
const todoStatuses: readonly string[] = ["pending", "in_progress", "blocked", "completed"] satisfies TodoStatus[]
const isTodoStatus = (status: string): status is TodoStatus => todoStatuses.includes(status)

function sessionsSection(state: AppState): NonNullable<ExtensionContext["sessions"]> {
  const items = sessionsInScope(state.sessions, state.activeProjectId, false).map((row) => ({
    id: row.id,
    ...(row.title ? { title: row.title } : {}),
    agent: row.agent,
    temporary: row.kind === "SESSION_KIND_TEMPORARY",
    archived: row.archived === true,
    busy: row.busy === true,
    waiting: Boolean(waitingKind(state.interactions, row.id)),
    ...(row.parent ? { parent: row.parent } : {}),
    ...(Number.isFinite(Date.parse(row.timeCreated ?? "")) ? { created: Date.parse(row.timeCreated ?? "") } : {}),
    ...(row.members?.length ? {
      members: row.members.flatMap((member) => member.child ? [{
        child: member.child,
        ...(member.handle ? { handle: member.handle } : {}),
        ...(member.description ? { description: member.description } : {}),
        ...(member.agent ? { agent: member.agent } : {}),
      }] : []),
    } : {}),
  }))
  return { ready: state.ready, ...(state.selected ? { selected: state.selected.id } : {}), items }
}

function statusSection(state: AppState, feed: ExtensionStatusFeed): NonNullable<ExtensionContext["status"]> {
  const mode = modeDisplay(effectiveMode(state), state.permissionModes)
  const session = state.selected
  const usage = session ? contextUsage(state) : undefined
  const tokens = session ? sessionTokens(session.usage) : undefined
  const forked = session ? forkSourceText(session.forkedFrom, state.sessions) : undefined
  const done = state.todos.filter((item) => todoStatusText(item.status) === "completed").length
  return {
    ready: state.ready,
    ...(state.vim ? { vim: { normal: state.vimMode === "normal", ...(state.vimPending ? { pending: state.vimPending } : {}) } } : {}),
    mode: { text: mode.text, tone: mode.tone === "error" ? "error" : mode.tone === "accent" ? "accent" : "strong" },
    ...(session ? {
      session: {
        id: session.id,
        ...(session.title ? { title: session.title } : {}),
        agent: session.agent,
        ...(forked ? { forked } : {}),
        model: modelEffortLabel(session),
        modelShort: modelEffortLabel(session, true),
        messages: mergeTranscript(state.messages, state.overlay).length,
        ...(session.workdir ? { workdir: session.workdir } : {}),
        ...(usage ? { context: usage } : {}),
        ...(tokens === undefined ? {} : { tokens }),
      },
    } : {}),
    ...(state.gitBranch ? { branch: state.gitBranch } : {}),
    ...(state.todos.length ? { todos: { done, total: state.todos.length } } : {}),
    items: feed.items,
    server: hostOf(feed.server),
    ...(state.web ? { web: { ...(state.web.url ? { host: hostOf(state.web.url) } : {}), label: webLabel(state.web) ?? "" } } : {}),
    versions: { tui: tuiVersion, ...(state.serverVersion ? { backend: state.serverVersion } : {}) },
    connection: state.backendStopped ? "stopped" : state.connected ? "connected" : "disconnected",
  }
}

/** The complete host context; `scopeContext` drops what an extension's permissions do not grant. */
export function extensionContext(state: AppState, terminal: { width: number; height: number }, feed: ExtensionStatusFeed): ExtensionContext {
  const session = state.selected
  const project = state.projects.find((row) => row.id === state.activeProjectId)
  const directory = session?.workdir || project?.roots[0] || ""
  const model = session ? modelReference(session) : undefined
  return {
    terminal: { columns: terminal.width, rows: terminal.height },
    ...(session ? {
      session: {
        id: session.id, busy: state.running, agent: session.agent,
        ...(session.title ? { title: session.title } : {}),
        ...(model ? { model } : {}),
        ...(session.permissionMode ? { mode: session.permissionMode } : {}),
        ...(session.workdir ? { workdir: session.workdir } : {}),
      },
      transcript: mergeTranscript(state.messages, state.overlay).slice(-50).map((message) => ({
        role: message.role.replace(/^ROLE_/, "").toLowerCase(),
        text: (message.parts ?? []).map((part) => part.text?.text ?? "").join("").slice(0, 4_000),
      })),
    } : {}),
    ...(directory ? { workspace: { directory, ...(project ? { project: { id: project.id, name: project.name } } : {}) } } : {}),
    ...(state.vcs ? {
      git: {
        ...(state.vcs.branch ? { branch: state.vcs.branch } : {}),
        ...(state.vcs.head ? { head: state.vcs.head } : {}),
        dirty: state.vcs.dirty ?? 0, ahead: state.vcs.ahead ?? 0, behind: state.vcs.behind ?? 0,
      },
    } : {}),
    sessions: sessionsSection(state),
    projects: {
      ready: state.ready,
      ...(state.activeProjectId ? { active: state.activeProjectId } : {}),
      ...(state.projectsError ? { error: state.projectsError } : {}),
      items: state.projects.map((row) => ({ id: row.id, name: row.name, roots: row.roots, busy: row.busy === true, ...(row.sessionCount === undefined ? {} : { sessionCount: row.sessionCount }) })),
    },
    todos: state.todos.flatMap((todo) => {
      const status = todoStatusText(todo.status)
      return isTodoStatus(status) ? [{ status, content: todo.content }] : []
    }),
    status: statusSection(state, feed),
  }
}
