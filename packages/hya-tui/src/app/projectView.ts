/**
 * The full-screen Project view's calls (docs/tui.md "Projects"; the pure
 * state is state/projectView.ts, the rendering components/ProjectView.tsx).
 */
import type { HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"
import { errorText } from "../state/providers"
import {
  initialProjectView, projectViewKey, settleProjectView,
  type ProjectViewNotice, type ProjectViewState,
} from "../state/projectView"

export interface ProjectViewControllerOptions {
  store: AppStore
  client: HyaClient
  switchProject: (id: string) => Promise<void>
  newTemporarySession: () => Promise<void>
  refreshProjects: () => Promise<void>
}

export function createProjectViewController({ store, client, switchProject, newTemporarySession, refreshProjects }: ProjectViewControllerOptions) {
  const view = (): ProjectViewState | undefined => store.state.projectView
  const patch = (change: (current: ProjectViewState) => ProjectViewState): void => {
    const current = view()
    if (current) store.setProjectView(change(current))
  }
  const notify = (notice: ProjectViewNotice | undefined): void => patch((current) => ({ ...current, notice }))

  async function reload(): Promise<void> {
    await refreshProjects()
    patch((current) => settleProjectView(current, store.state.projects))
  }

  function open(): void {
    store.setProjectView(initialProjectView(store.state.projects, store.state.activeProjectId))
    void reload()
  }

  function close(): void {
    store.setProjectView(undefined)
  }

  async function doSwitch(id: string): Promise<void> {
    close()
    await switchProject(id)
  }

  async function doCreate(name: string, roots: string[]): Promise<void> {
    patch((current) => ({ ...current, create: undefined, busy: { label: "Creating", startedAt: Date.now() } }))
    try {
      const project = await client.createProject({ name, roots })
      await refreshProjects()
      patch((current) => ({ ...settleProjectView(current, store.state.projects), busy: undefined, highlighted: project.id, notice: { tone: "ok", text: `Created ${project.name}` } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Create failed: ${errorText(error)}` })
    }
  }

  async function doRename(id: string, title: string): Promise<void> {
    patch((current) => ({ ...current, rename: undefined, busy: { label: "Renaming", startedAt: Date.now() } }))
    try {
      await client.updateProject(id, { name: title })
      await refreshProjects()
      patch((current) => ({ ...settleProjectView(current, store.state.projects), busy: undefined, notice: { tone: "ok", text: `Renamed to ${title}` } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Rename failed: ${errorText(error)}` })
    }
  }

  async function doEditRoots(id: string, roots: string[]): Promise<void> {
    patch((current) => ({ ...current, editRoots: undefined, busy: { label: "Saving roots", startedAt: Date.now() } }))
    try {
      await client.updateProject(id, { roots })
      await refreshProjects()
      patch((current) => ({ ...settleProjectView(current, store.state.projects), busy: undefined, notice: { tone: "ok", text: "Roots updated" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Update failed: ${errorText(error)}` })
    }
  }

  async function doDelete(id: string): Promise<void> {
    patch((current) => ({ ...current, confirm: undefined, busy: { label: "Deleting", startedAt: Date.now() } }))
    try {
      await client.deleteProject(id)
      await refreshProjects()
      patch((current) => ({ ...settleProjectView(current, store.state.projects), busy: undefined, notice: { tone: "ok", text: "Deleted" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      // The server refuses with `failed_precondition` while a live root session belongs to the Project; show it verbatim.
      notify({ tone: "error", text: errorText(error) })
    }
  }

  /** Tab while a root path is being typed (create's root step, or `a` in the roots editor): complete from the backend filesystem (`/v1/fs/find`). */
  async function completeRoot(input: string): Promise<void> {
    const pattern = `${input.replace(/\/+$/, "")}*`
    try {
      const matches = await client.findFiles(pattern, 1)
      const match = matches[0]
      if (!match) return
      patch((current) => {
        if (current.create?.step === "root") return { ...current, create: { ...current.create, input: match } }
        if (current.editRoots?.adding) return { ...current, editRoots: { ...current.editRoots, input: match } }
        return current
      })
    } catch {
      // No completion available (an older backend, or no match): leave the input as typed.
    }
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    if (pressed.name === "tab" && (current.create?.step === "root" || current.editRoots?.adding)) {
      const input = current.create?.step === "root" ? current.create.input : current.editRoots?.input ?? ""
      void completeRoot(input)
      return
    }
    const outcome = projectViewKey(current, pressed, store.state.projects)
    switch (outcome.type) {
      case "update": store.setProjectView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": return
      case "switch": void doSwitch(outcome.id); return
      case "createRoot": void doCreate(outcome.name, outcome.roots); return
      case "rename": void doRename(outcome.id, outcome.title); return
      case "editRootsCommit": void doEditRoots(outcome.id, outcome.roots); return
      case "delete": void doDelete(outcome.id); return
      case "temporary": close(); void newTemporarySession(); return
    }
  }

  return { open, close, key }
}

export type ProjectViewController = ReturnType<typeof createProjectViewController>
