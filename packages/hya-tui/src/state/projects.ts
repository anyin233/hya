/**
 * Projects in the TUI (ADR-0024, docs/tui.md "Projects"): which Project is
 * active, which directory scope and session placement follow from it, and
 * which session to open in it. Pure helpers; the controller
 * (app/controller.ts) owns the calls.
 */
import type { ProjectInfo, SessionInfo, SessionPlacement } from "../client"

/** Status line when a new session is asked for without an active Project (a `--remote` start). */
export const noProjectStatus = "No project is open · choose a project or start a temporary session"

function trimSlashes(path: string): string {
  return path.length > 1 ? path.replace(/\/+$/, "") || "/" : path
}

/** Whether `path` is `root` or lies below it (a sibling sharing a name prefix does not count). */
export function pathInside(path: string, root: string): boolean {
  const base = trimSlashes(root)
  const target = trimSlashes(path)
  if (base === "/") return target.startsWith("/")
  return target === base || target.startsWith(`${base}/`)
}

/** Whether `path` lies inside one of the Project's roots. */
export function projectContains(project: ProjectInfo, path: string): boolean {
  return project.roots.some((root) => pathInside(path, root))
}

/** The active Project's row, when it is set and listed. */
export function activeProject(state: { projects: readonly ProjectInfo[]; activeProjectId: string | undefined }): ProjectInfo | undefined {
  const id = state.activeProjectId
  return id ? state.projects.find((project) => project.id === id) : undefined
}

/** Whether a session of the Project runs a turn now (`ProjectInfo.busy`, kept live by `projectsUpdated`). */
export function projectBusy(state: { projects: readonly ProjectInfo[] }, id: string): boolean {
  return state.projects.find((project) => project.id === id)?.busy === true
}

/**
 * The client's directory scope in `project`: `--dir` on a local start when
 * it lies inside the Project, else the primary root.
 */
export function projectScope(project: ProjectInfo, directory: string, remote: boolean): string {
  if (!remote && projectContains(project, directory)) return directory
  return project.roots[0] ?? directory
}

/**
 * Where `CreateSession` puts the next session: a temporary session when
 * asked; in the active Project — working in `--dir` on a local start inside
 * it, else in the primary root (workdir left to the server); without an
 * active Project a local start sends `--dir` (the server ensures its
 * Project), and a remote one is refused (`undefined`).
 */
export function sessionPlacement(input: { project: ProjectInfo | undefined; directory: string; remote: boolean; temporary?: boolean }): SessionPlacement | undefined {
  const { project, directory, remote } = input
  if (input.temporary) return { temporary: true }
  if (project) return !remote && projectContains(project, directory) ? { projectId: project.id, workdir: directory } : { projectId: project.id }
  return remote ? undefined : { workdir: directory }
}

/**
 * Sessions in scope for a session list: with an active Project and
 * `allProjects` false, only its root sessions (plus their subagents) and
 * every temporary root session (it has no Project); `allProjects` (the
 * `/sessions` picker's toggle) or no active Project shows every session.
 */
export function sessionsInScope(sessions: readonly SessionInfo[], activeProjectId: string | undefined, allProjects: boolean): SessionInfo[] {
  if (allProjects || !activeProjectId) return [...sessions]
  const kept = new Set(
    sessions
      .filter((session) => !session.parent && (session.kind === "SESSION_KIND_TEMPORARY" || session.projectId === activeProjectId))
      .map((session) => session.id),
  )
  return sessions.filter((session) => (session.parent ? kept.has(session.parent) : kept.has(session.id)))
}

/** The Project's most recently updated top-level session (list order breaks ties). */
export function newestTopLevelSession(sessions: readonly SessionInfo[], projectId: string): SessionInfo | undefined {
  const time = (session: SessionInfo): number => Date.parse(session.timeUpdated ?? "") || 0
  return sessions
    .filter((session) => !session.parent && !session.archived && session.projectId === projectId)
    .reduce<SessionInfo | undefined>((best, session) => (!best || time(session) > time(best) ? session : best), undefined)
}
