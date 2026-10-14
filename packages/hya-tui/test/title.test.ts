import { expect, test } from "bun:test"
import { createAppStore } from "../src/state/store"
import { popTitleSequence, pushTitleSequence, terminalTitle, titleActivity, titleFromState, titleSequence, TitleWriter } from "../src/title"

test("terminalTitle puts the marker and session first, then the Project", () => {
  expect(terminalTitle({ session: "Fix login bug", project: "web", activity: "idle" })).toBe("Fix login bug · web")
  expect(terminalTitle({ session: "Fix login bug", project: "web", activity: "working" })).toBe("● Fix login bug · web")
  expect(terminalTitle({ session: "Fix login bug", project: "web", activity: "waiting" })).toBe("? Fix login bug · web")
})

test("terminalTitle drops the Project for an unseen outcome", () => {
  expect(terminalTitle({ session: "Fix login bug", project: "web", activity: "finished" })).toBe("✓ Fix login bug")
  expect(terminalTitle({ session: "Fix login bug", project: "web", activity: "failed" })).toBe("✗ Fix login bug")
})

test("terminalTitle falls back to hya without a titled session, and to hya alone without a Project", () => {
  expect(terminalTitle({ session: "", project: "web", activity: "idle" })).toBe("hya · web")
  expect(terminalTitle({ project: "web", activity: "working" })).toBe("● hya · web")
  expect(terminalTitle({ activity: "idle" })).toBe("hya")
  // A Project named like the session is not repeated.
  expect(terminalTitle({ session: "web", project: "web", activity: "idle" })).toBe("web")
})

test("terminalTitle strips C0 and C1 controls so a title cannot end the OSC early", () => {
  const title = terminalTitle({ session: "evil\x07\x1b]0;pwned\x9c title", project: "p\x1bq", activity: "idle" })
  expect(title).not.toMatch(/[\x00-\x1f\x7f-\x9f]/)
  expect(title).toBe("evil ]0;pwned title · p q")
})

test("terminalTitle caps the session so the Project still fits", () => {
  const title = terminalTitle({ session: "s".repeat(200), project: "my-project", activity: "working" })
  expect(title.endsWith(" · my-project")).toBe(true)
  expect(title.length).toBeLessThanOrEqual(80)
})

test("titleActivity: an ask wins over a running turn, which wins over an unseen outcome", () => {
  expect(titleActivity({ waiting: true, working: true, unseenOutcome: "finished" })).toBe("waiting")
  expect(titleActivity({ waiting: false, working: true, unseenOutcome: "failed" })).toBe("working")
  expect(titleActivity({ waiting: false, working: false, unseenOutcome: "failed" })).toBe("failed")
  expect(titleActivity({ waiting: false, working: false })).toBe("idle")
})

test("titleFromState reads the open session, its asks, and the active Project", () => {
  const store = createAppStore()
  expect(titleFromState(store.state, "/home/me/code/web")).toBe("hya · web")
  store.setSessions([{ id: "s1", agent: "hya-main", workdir: "/home/me/code/web", title: "Fix login bug" }])
  store.setSelected({ id: "s1", agent: "hya-main", workdir: "/home/me/code/web", title: "Fix login bug" })
  store.setProjects([{ id: "p1", name: "Website", roots: ["/home/me/code/web"] }])
  store.setActiveProject("p1")
  expect(titleFromState(store.state, "/x")).toBe("Fix login bug · Website")
  store.beginTurn()
  expect(titleFromState(store.state, "/x")).toBe("● Fix login bug · Website")
  store.setInteractions([{ id: "ask1", session: "s1", type: "permission", title: "bash" }])
  expect(titleFromState(store.state, "/x")).toBe("? Fix login bug · Website")
  // Another session's ask does not mark this tab.
  store.setInteractions([{ id: "ask2", session: "other", type: "permission", title: "bash" }])
  expect(titleFromState(store.state, "/x")).toBe("● Fix login bug · Website")
})

test("an unseen outcome is cleared when the terminal is focused again", () => {
  const store = createAppStore()
  store.setSelected({ id: "s1", agent: "hya-main", workdir: "/w/web", title: "Parser" })
  store.setFocused(false)
  store.setUnseenOutcome("finished")
  expect(titleFromState(store.state, "/w/web")).toBe("✓ Parser")
  store.setFocused(true)
  expect(store.state.unseenOutcome).toBeUndefined()
  expect(titleFromState(store.state, "/w/web")).toBe("Parser · web")
})

test("TitleWriter saves the title, writes changes only, and restores it on stop", () => {
  const written: string[] = []
  const writer = new TitleWriter((sequence) => written.push(sequence))
  writer.update("before start")
  expect(written).toEqual([])
  writer.start()
  writer.update("a")
  writer.update("a")
  writer.update("b")
  writer.stop()
  writer.stop()
  expect(written).toEqual([pushTitleSequence, titleSequence("a"), titleSequence("b"), popTitleSequence])
})

test("titleSequence is OSC 0 terminated by BEL", () => {
  expect(titleSequence("hya")).toBe("\x1b]0;hya\x07")
})
