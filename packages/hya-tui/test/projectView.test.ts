import { expect, test } from "bun:test"
import type { ProjectInfo } from "../src/client"
import { initialProjectView, projectViewKey, settleProjectView, type ProjectViewState } from "../src/state/projectView"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra })

const projects: ProjectInfo[] = [
  { id: "a", name: "alpha", roots: ["/a1", "/a2"] },
  { id: "b", name: "beta", roots: ["/b1"] },
]

test("initialProjectView highlights the active Project, else the first row", () => {
  expect(initialProjectView(projects, "b").highlighted).toBe("b")
  expect(initialProjectView(projects, undefined).highlighted).toBe("a")
  expect(initialProjectView([], undefined).highlighted).toBeUndefined()
})

test("settleProjectView keeps the highlight, or falls back to the first row when it is gone", () => {
  const view = { highlighted: "b" }
  expect(settleProjectView(view, projects)).toBe(view)
  expect(settleProjectView({ highlighted: "gone" }, projects).highlighted).toBe("a")
  expect(settleProjectView({ highlighted: "a" }, []).highlighted).toBeUndefined()
})

test("`e` opens the roots editor pre-filled with the highlighted Project's roots, primary first", () => {
  const view = { highlighted: "a" }
  const outcome = projectViewKey(view, key("e"), projects)
  expect(outcome).toEqual({ type: "update", view: { ...view, notice: undefined, editRoots: { id: "a", roots: ["/a1", "/a2"], selected: 0, input: "" } } })
})

test("in the roots editor: `a` adds a root, `d` removes the selected one but refuses the last, Shift+Up/Down reorders", () => {
  let view: ProjectViewState = { highlighted: "a", editRoots: { id: "a", roots: ["/a1", "/a2"], selected: 0, input: "" } }
  // `a` opens a text input; typing then Enter appends the root.
  const outcome = projectViewKey(view, key("a"), projects)
  expect(outcome.type).toBe("update")
  view = (outcome as { view: ProjectViewState }).view
  expect(view.editRoots?.adding).toBe(true)
  for (const ch of "/a3") { view = (projectViewKey(view, key(ch, { sequence: ch }), projects) as { view: ProjectViewState }).view }
  view = (projectViewKey(view, key("return"), projects) as { view: ProjectViewState }).view
  expect(view.editRoots).toEqual({ id: "a", roots: ["/a1", "/a2", "/a3"], selected: 2, adding: false, input: "" })

  // Shift+Down moves the selected root later.
  view = { ...view, editRoots: { ...view.editRoots!, selected: 0 } }
  view = (projectViewKey(view, key("down", { shift: true }), projects) as { view: ProjectViewState }).view
  expect(view.editRoots?.roots).toEqual(["/a2", "/a1", "/a3"])
  expect(view.editRoots?.selected).toBe(1)

  // `d` removes the selected root.
  view = (projectViewKey(view, key("d"), projects) as { view: ProjectViewState }).view
  expect(view.editRoots?.roots).toEqual(["/a2", "/a3"])

  // Removing down to one root refuses further removal.
  view = { ...view, editRoots: { ...view.editRoots!, roots: ["/only"], selected: 0 } }
  const refused = projectViewKey(view, key("d"), projects)
  expect(refused).toEqual({ type: "update", view: { ...view, editRoots: { ...view.editRoots!, roots: ["/only"], selected: 0 }, notice: { tone: "info", text: "A Project needs at least one root" } } })

  // Enter commits the whole edited list.
  const committed = projectViewKey({ highlighted: "a", editRoots: { id: "a", roots: ["/x", "/y"], selected: 0, input: "" } }, key("return"), projects)
  expect(committed).toEqual({ type: "editRootsCommit", id: "a", roots: ["/x", "/y"] })
})

test("`n` starts the create flow: name then one root per step, at least one root required", () => {
  let view: ProjectViewState = { highlighted: undefined }
  view = (projectViewKey(view, key("n"), projects) as { view: ProjectViewState }).view
  expect(view.create).toEqual({ step: "name", name: "", roots: [], input: "" })
  // Enter on an empty name is refused.
  const refused = projectViewKey(view, key("return"), projects)
  expect(refused.type).toBe("update")
  for (const ch of "New") view = (projectViewKey(view, key(ch, { sequence: ch }), projects) as { view: ProjectViewState }).view
  view = (projectViewKey(view, key("return"), projects) as { view: ProjectViewState }).view
  expect(view.create).toEqual({ step: "root", name: "New", roots: [], input: "" })
  // Enter on an empty root with none yet is refused.
  const refusedRoot = projectViewKey(view, key("return"), projects)
  expect(refusedRoot.type).toBe("update")
  for (const ch of "/root") view = (projectViewKey(view, key(ch, { sequence: ch }), projects) as { view: ProjectViewState }).view
  view = (projectViewKey(view, key("return"), projects) as { view: ProjectViewState }).view
  expect(view.create).toEqual({ step: "root", name: "New", roots: ["/root"], input: "" })
  // Enter on an empty root once at least one exists finishes the flow.
  const done = projectViewKey(view, key("return"), projects)
  expect(done).toEqual({ type: "createRoot", name: "New", roots: ["/root"] })
})

test("`d` asks to confirm a delete; Enter commits, Esc cancels", () => {
  const view = { highlighted: "b" }
  const asked = projectViewKey(view, key("d"), projects)
  expect(asked).toEqual({ type: "update", view: { ...view, notice: undefined, confirm: "b" } })
  const confirmedView = (asked as { view: typeof view & { confirm: string } }).view
  expect(projectViewKey(confirmedView, key("return"), projects)).toEqual({ type: "delete", id: "b" })
  expect(projectViewKey(confirmedView, key("escape"), projects)).toEqual({ type: "update", view: { ...confirmedView, confirm: undefined } })
})

test("Esc on the plain list closes the view", () => {
  expect(projectViewKey({ highlighted: "a" }, key("escape"), projects)).toEqual({ type: "close" })
})
