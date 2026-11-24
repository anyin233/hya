/**
 * The Skills view's calls (docs/tui.md "Skills view"; the pure state is
 * state/skills.ts, the rendering components/SkillsView.tsx).
 */
import type { SkillSummary } from "../client"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"
import { errorText } from "../state/providers"
import { hasSkillFile, initialSkillsView, settleSkillsView, skillBodyLines, skillsViewKey, type SkillsNotice, type SkillsViewState } from "../state/skills"

export interface SkillsControllerOptions {
  store: AppStore
  /** Re-read the skill catalog (`ListSkills` and `ListCommands`). */
  readSkills(signal?: AbortSignal): Promise<SkillSummary[]>
  /** Replace the message input (not sent). */
  fillInput(text: string): void
  /** OSC 52; `false` when the terminal does not take it. */
  copyText(text: string): boolean
  /** Terminal rows, for the details' page size. */
  rows?(): number
}

/** Rows around the details' body: border (2), name, file, description, rule, hint (5). */
const detailChrome = 7

export function createSkillsController({ store, readSkills, fillInput, copyText, rows = () => process.stdout.rows || 24 }: SkillsControllerOptions) {
  let abort: AbortController | undefined

  const view = (): SkillsViewState | undefined => store.state.skillsView
  const patch = (change: (current: SkillsViewState) => SkillsViewState): void => {
    const current = view()
    if (current) store.setSkillsView(change(current))
  }
  const notify = (notice: SkillsNotice | undefined): void => patch((current) => ({ ...current, notice }))

  async function reload(signal?: AbortSignal): Promise<void> {
    const skills = await readSkills(signal)
    patch((current) => settleSkillsView(current, skills))
  }

  /** `/skills [filter]`: open on the skills read last, then re-read them. */
  function open(filter = ""): void {
    store.setSkillsView(initialSkillsView(store.state.skills, filter))
    void reload().catch((error: unknown) => notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` }))
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setSkillsView(undefined)
  }

  async function refresh(): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { label: "Refreshing", startedAt: Date.now() } }))
    try {
      await reload(controller.signal)
      const count = store.state.skills.length
      patch((current) => ({ ...current, busy: undefined, notice: { tone: "ok", text: `Refreshed · ${count} skill${count === 1 ? "" : "s"}` } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      if (!controller.signal.aborted) notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  function use(name: string): void {
    close()
    fillInput(`/skill:${name} `)
    store.setStatus(`/skill:${name} · type the request, Enter sends`)
  }

  function copy(name: string): void {
    const skill = store.state.skills.find((row) => row.name === name)
    if (!skill || !hasSkillFile(skill)) {
      notify({ tone: "info", text: "Built-in skill has no file" })
      return
    }
    const path = skill.location!
    const remote = store.state.serverLabel ? " (a path on the remote backend)" : ""
    notify(copyText(path) ? { tone: "ok", text: `Copied ${path}${remote}` } : { tone: "error", text: `This terminal does not accept OSC 52 copies · ${path}` })
  }

  /** The details' scroll limits for the highlighted skill at the current size. */
  function limits(current: SkillsViewState) {
    const skill = store.state.skills.find((row) => row.name === current.skill)
    const width = Math.max(20, store.state.columns - 4)
    return { lines: skill ? skillBodyLines(skill.content ?? "", width).length : 0, page: Math.max(1, rows() - detailChrome) }
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = skillsViewKey(current, pressed, store.state.skills, limits(current))
    switch (outcome.type) {
      case "update": store.setSkillsView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void refresh(); return
      case "use": use(outcome.skill); return
      case "copy": copy(outcome.skill); return
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}
