import { expect, test } from "bun:test"

import { CommandMap, Definitions } from "../src/upstream/config/keybind"
import {
  RunTreeParseError,
  createRunTreeLoader,
  createWorkspaceState,
  flattenRunTree,
  parseRunTree,
  reduceWorkspace,
  resolveLifecyclePresentation,
  runTreeEventEffect,
  treeSessionIDs,
  workspaceLeaves,
  workspacePaneStrip,
} from "../src/upstream/routes/session/subagent-workspace"

test("exposes exact pane command defaults", () => {
  const expected = {
    pane_roster: ["pane.roster", "<leader>o"],
    pane_open_tab: ["pane.open.tab", "<leader>T"],
    pane_open_vertical: ["pane.open.vertical", "<leader>V"],
    pane_open_horizontal: ["pane.open.horizontal", "<leader>S"],
    pane_close: ["pane.close", "<leader>w"],
    pane_cycle: ["pane.cycle", "<leader>."],
    pane_focus_main: ["pane.focus.main", "<leader>0"],
  } as const
  for (const [name, [command, binding]] of Object.entries(expected)) {
    expect(CommandMap[name as keyof typeof expected]).toBe(command)
    expect(Definitions[name as keyof typeof expected].default).toBe(binding)
  }
  expect(new Set(Object.values(expected).map(([command]) => command)).size).toBe(7)
})

test("validates recursive run tree payloads", () => {
  const tree = parseRunTree({
    session: "root",
    agent: "build",
    children: [
      {
        session: "child",
        member: {
          member: "member-1",
          child: "child",
          subagent_type: "explore",
          description: "inspect",
          depth: 1,
          status: "running",
          summary: "",
        },
        roster: {
          handle: "explore-1",
          session: "child",
          agent_type: "explore",
          mode: "transient",
          status: "busy",
          current_task: "inspect",
        },
        children: [{ member: { member: "pending", subagent_type: "plan", description: "wait", depth: 2, status: "spawning", summary: "" } }],
      },
    ],
  })

  expect(tree.children[0]?.roster?.handle).toBe("explore-1")
  expect(tree.children[0]?.children[0]?.session).toBeUndefined()
  expect(() => parseRunTree({ session: "root", children: [{ session: 1 }] })).toThrow(RunTreeParseError)
  expect(() => parseRunTree({ session: "root", children: [{ member: { member: "bad" } }] })).toThrow(
    RunTreeParseError,
  )
})

test("normalizes an omitted live member summary", () => {
  const member = {
    member: "member-1",
    child: "child",
    subagent_type: "build",
    description: "RUST-RUNTIME",
    depth: 1,
    status: "running",
  }
  const tree = parseRunTree({
    session: "root",
    children: [{ session: "child", member }],
  })

  expect(tree.children[0]?.member?.summary).toBe("")
  expect(() =>
    parseRunTree({
      session: "root",
      children: [{ session: "child", member: { ...member, summary: 1 } }],
    }),
  ).toThrow(RunTreeParseError)
})

test("resolves member-first lifecycle presentation", () => {
  expect(resolveLifecyclePresentation({ member: { status: "running" }, roster: { status: "idle" } })).toEqual({
    label: "Working",
    working: true,
  })
  expect(resolveLifecyclePresentation({ member: { status: "spawning" } })).toEqual({
    label: "Working",
    working: true,
  })
  expect(resolveLifecyclePresentation({ roster: { status: "busy" } })).toEqual({ label: "Working", working: true })
  expect(resolveLifecyclePresentation({ member: { status: "done" } })).toEqual({ label: "Finished", working: false })
  expect(resolveLifecyclePresentation({ member: { status: "failed" } })).toEqual({ label: "Failed", working: false })
  expect(resolveLifecyclePresentation({ member: { status: "cancelled" } })).toEqual({
    label: "Cancelled",
    working: false,
  })
  expect(resolveLifecyclePresentation({ roster: { status: "idle" } })).toEqual({ label: "Idle", working: false })
  expect(resolveLifecyclePresentation({})).toEqual({ label: "Idle", working: false })
})

test("keeps one uncloseable main leaf", () => {
  const state = createWorkspaceState("root")
  expect(workspaceLeaves(state)).toEqual([{ type: "main", id: "main" }])
  expect(reduceWorkspace(state, { type: "close", paneID: "main" })).toBe(state)
})

test("opens and focuses one observation per session", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child" })
  state = reduceWorkspace(state, { type: "focus", paneID: "main" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child" })

  expect(workspaceLeaves(state).filter((pane) => pane.type === "observation")).toHaveLength(1)
  expect(state.tabs).toHaveLength(2)
  expect(state.activeTabID).toBe("tab:child")
  expect(state.focusedPaneID).toBe("observation:child")
})

test("split always places observation beside Main, never nests under another observation", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  // While observation is focused, opening another split must rebuild Main | child-b
  // rather than nesting under child-a (which broke agent switching after Ctrl+X V).
  // The previous observation is retained as a tab so users can still navigate to it.
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })

  expect(
    workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID)),
  ).toEqual(["main", "child-b", "child-a"])
  expect(state.tabs[0]?.root).toMatchObject({
    type: "split",
    axis: "horizontal",
    first: { type: "main", id: "main" },
    second: { type: "observation", sessionID: "child-b" },
  })
  expect(state.activeTabID).toBe("main")
  expect(state.focusedPaneID).toBe("observation:child-b")

  // focusMain still works after a post-split agent switch
  state = reduceWorkspace(state, { type: "focusMain" })
  expect(state.focusedPaneID).toBe("main")
  expect(state.activeTabID).toBe("main")
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual([
    "main",
    "observation:child-b",
    "observation:child-a",
  ])
})

test("focusing another open subagent in split mode swaps it beside Main", () => {
  // Users must be able to walk Main ↔ A ↔ B while a split is open without losing either subagent.
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-b" })
  expect(state.focusedPaneID).toBe("observation:child-b")
  expect(workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID))).toEqual([
    "main",
    "child-b",
    "child-a",
  ])
  // Stable open order is open-sequence, not current split partner order.
  expect(state.observationOrder).toEqual(["child-a", "child-b"])

  state = reduceWorkspace(state, { type: "focus", paneID: "observation:child-a" })
  expect(state.activeTabID).toBe("main")
  expect(state.focusedPaneID).toBe("observation:child-a")
  expect(state.tabs[0]?.root).toMatchObject({
    type: "split",
    axis: "vertical",
    second: { type: "observation", sessionID: "child-a" },
  })
  // child-b remains open as a retained tab
  expect(workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID))).toEqual([
    "main",
    "child-a",
    "child-b",
  ])

  // Cycle walks every open leaf in stable open order so Left/Right reaches every subagent.
  state = reduceWorkspace(state, { type: "focusMain" })
  const order = []
  for (let index = 0; index < 3; index++) {
    state = reduceWorkspace(state, { type: "cycleFocus", direction: 1 })
    order.push(state.focusedPaneID)
  }
  expect(order).toEqual(["observation:child-a", "observation:child-b", "main"])
})

test("closes split observation and returns to Main", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "close", paneID: "observation:child-a" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main"])
  expect(state.focusedPaneID).toBe("main")

  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  state = reduceWorkspace(state, { type: "close", paneID: "observation:child-c" })
  expect(state.tabs.map((tab) => tab.id)).toEqual(["main"])
  expect(state.activeTabID).toBe("main")
})

test("cycles focus across split observation and tab observations", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  // leaves: main, child-a (split), child-c (tab)
  state = reduceWorkspace(state, { type: "focusMain" })

  const order = []
  for (let index = 0; index < 3; index++) {
    state = reduceWorkspace(state, { type: "cycleFocus" })
    order.push(state.focusedPaneID)
  }
  // Focusing child-c while split mode is active promotes it beside Main, so leaf
  // order becomes main, child-c, child-a. Cycle still visits every open subagent.
  expect(order).toEqual(["observation:child-a", "observation:child-c", "main"])
  state = reduceWorkspace(state, { type: "focus", paneID: "observation:child-a" })
  expect(state.tabs[0]?.root).toMatchObject({
    type: "split",
    second: { type: "observation", sessionID: "child-a" },
  })
  expect(reduceWorkspace(state, { type: "focusMain" }).focusedPaneID).toBe("main")
})

test("cycles focus backward across split main and observation panes", () => {
  // After a left/right split, reverse cycle must walk observation → main so
  // keyboard left/right navigation can move between the two visible panes.
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  expect(state.focusedPaneID).toBe("observation:child-a")

  state = reduceWorkspace(state, { type: "cycleFocus", direction: -1 })
  expect(state.focusedPaneID).toBe("main")
  expect(state.activeTabID).toBe("main")

  state = reduceWorkspace(state, { type: "cycleFocus", direction: -1 })
  expect(state.focusedPaneID).toBe("observation:child-a")

  state = reduceWorkspace(state, { type: "cycleFocus", direction: 1 })
  expect(state.focusedPaneID).toBe("main")
})

test("pane strip lists every leaf with focus markers for tab-bar navigation", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-b" })

  expect(workspacePaneStrip(state)).toEqual([
    { paneID: "main", focused: false },
    { paneID: "observation:child-a", focused: false },
    { paneID: "observation:child-b", focused: true },
  ])

  state = reduceWorkspace(state, { type: "focus", paneID: "main" })
  expect(workspacePaneStrip(state).find((entry) => entry.paneID === "main")?.focused).toBe(true)
  expect(workspacePaneStrip(state).filter((entry) => entry.focused)).toHaveLength(1)
})

test("Esc-equivalent focusMain exits every observation placement back to Main", () => {
  // Session Escape binding dispatches focusMain; this is the workspace half of the
  // "cannot exit subagent view" regression (ADR-0003: Esc returns to Main).
  for (const open of [
    { type: "openTab" as const, sessionID: "child-tab" },
    { type: "openSplit" as const, axis: "vertical" as const, sessionID: "child-v" },
    { type: "openSplit" as const, axis: "horizontal" as const, sessionID: "child-h" },
  ]) {
    let state = createWorkspaceState("root")
    state = reduceWorkspace(state, open)
    expect(state.focusedPaneID).toBe(`observation:${open.sessionID}`)
    state = reduceWorkspace(state, { type: "focusMain" })
    expect(state.focusedPaneID).toBe("main")
    expect(state.activeTabID).toBe("main")
  }
})

test("retains focused and unfocused terminal observations after focus leaves", () => {
  const tree = parseRunTree({
    session: "root",
    children: [
      {
        session: "child-a",
        member: { member: "a", child: "child-a", subagent_type: "build", description: "done", depth: 1, status: "done" },
      },
      {
        session: "child-b",
        member: { member: "b", child: "child-b", subagent_type: "build", description: "failed", depth: 1, status: "failed" },
      },
    ],
  })
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "reconcileSessions", sessionIDs: [...treeSessionIDs(tree)] })

  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual([
    "main",
    "observation:child-a",
    "observation:child-b",
  ])
  state = reduceWorkspace(state, { type: "focusMain" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual([
    "main",
    "observation:child-a",
    "observation:child-b",
  ])
  expect(state.focusedPaneID).toBe("main")
})

test("retains an already-terminal observation after focus leaves", () => {
  const tree = parseRunTree({
    session: "root",
    children: [
      {
        session: "child",
        member: {
          member: "member-1",
          child: "child",
          subagent_type: "explore",
          description: "done",
          depth: 1,
          status: "done",
          summary: "complete",
        },
      },
    ],
  })
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child" })
  state = reduceWorkspace(state, { type: "reconcileSessions", sessionIDs: [...treeSessionIDs(tree)] })

  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main", "observation:child"])
  state = reduceWorkspace(state, { type: "focusMain" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main", "observation:child"])
})

test("prunes sessions missing from a successful tree", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  // Switch the split observation to child-b (child-a is retained as a tab)
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "reconcileSessions", sessionIDs: ["root", "child-b"] })

  expect(workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID))).toEqual([
    "main",
    "child-b",
  ])
  expect(state.tabs).toHaveLength(1)
  // child-b remains valid; focus stays on the surviving observation
  expect(state.focusedPaneID).toBe("observation:child-b")
  expect(state.activeTabID).toBe("main")
  // And focusMain still works after prune
  expect(reduceWorkspace(state, { type: "focusMain" }).focusedPaneID).toBe("main")
})

test("main tree row is selectable so roster can return to Main", () => {
  const tree = parseRunTree({
    session: "root",
    agent: "build",
    children: [
      {
        session: "child",
        member: {
          member: "m1",
          child: "child",
          subagent_type: "explore",
          description: "x",
          depth: 1,
          status: "running",
        },
      },
    ],
  })
  const rows = flattenRunTree(tree)
  expect(rows[0]?.depth).toBe(0)
  expect(rows[0]?.selectable).toBe(true)
  expect(rows[0]?.node.session).toBe("root")
  expect(rows[1]?.selectable).toBe(true)
})

test("retains the last valid tree after a failed refresh", async () => {
  let calls = 0
  const applied: string[] = []
  const loader = createRunTreeLoader({
    async fetchTree() {
      calls++
      if (calls === 1) throw new Error("offline")
      if (calls === 2) return { session: "root" }
      return { session: 42 }
    },
    onTree(tree) {
      applied.push(tree.session!)
    },
  })
  loader.setSession("root")

  await loader.refresh()
  expect(loader.state.status).toBe("error")
  expect(loader.state.tree).toBeUndefined()
  await loader.refresh()
  expect(loader.state.status).toBe("ready")
  expect(loader.state.tree?.session).toBe("root")
  await loader.refresh()
  expect(loader.state.status).toBe("error")
  expect(loader.state.tree?.session).toBe("root")
  expect(applied).toEqual(["root"])
  expect(calls).toBe(3)
  await Bun.sleep(10)
  expect(calls).toBe(3)
})

test("allows one in flight and one trailing refresh", async () => {
  let calls = 0
  let active = 0
  let maxActive = 0
  const resolve: Array<() => void> = []
  const loader = createRunTreeLoader({
    fetchTree() {
      calls++
      active++
      maxActive = Math.max(maxActive, active)
      return new Promise((done) => {
        resolve.push(() => {
          active--
          done({ session: "root" })
        })
      })
    },
    onTree() {},
  })
  loader.setSession("root")

  const requests = [loader.refresh(), loader.refresh(), loader.refresh()]
  expect(calls).toBe(1)
  expect(maxActive).toBe(1)
  resolve.shift()!()
  await Bun.sleep(0)
  expect(calls).toBe(2)
  expect(active).toBe(1)
  resolve.shift()!()
  await Promise.all(requests)
  expect(calls).toBe(2)
  expect(maxActive).toBe(1)
})

test("ignores stale generation responses", async () => {
  const resolve = new Map<string, (value: unknown) => void>()
  const applied: string[] = []
  const loader = createRunTreeLoader({
    fetchTree(sessionID) {
      return new Promise((done) => resolve.set(sessionID, done))
    },
    onTree(tree) {
      applied.push(tree.session!)
    },
  })

  loader.setSession("root-a")
  const first = loader.refresh()
  loader.setSession("root-b")
  const second = loader.refresh()
  resolve.get("root-a")!({ session: "root-a" })
  await Bun.sleep(0)
  expect(applied).toEqual([])
  expect(loader.state.status).toBe("loading")
  resolve.get("root-b")!({ session: "root-b" })
  await Promise.all([first, second])
  expect(applied).toEqual(["root-b"])
  expect(loader.state.tree?.session).toBe("root-b")
})

test("recognizes only run-tree invalidation events", () => {
  const native = (event: Record<string, unknown>) => ({
    type: "hya.envelope",
    properties: { event: { session: "root", ...event } },
  })
  const invalidators = [
    { type: "session.created", properties: {} },
    { type: "session.updated", properties: {} },
    { type: "session.deleted", properties: {} },
    native({ type: "member_spawned" }),
    native({ type: "member_status_changed", member: "member-1", status: "running" }),
    native({ type: "member_finished", member: "member-1", status: "done", child: "child" }),
    native({ type: "agent_registered", agent_session: "child", handle: "explore-1", agent_type: "explore", mode: "transient" }),
    native({ type: "agent_activity_changed", handle: "explore-1", status: "busy" }),
  ]
  expect(invalidators.filter((event) => runTreeEventEffect(event).refresh)).toHaveLength(8)

  for (const status of ["done", "failed", "cancelled"]) {
    expect(runTreeEventEffect(native({ type: "member_status_changed", member: "member-1", status }))).toEqual({
      refresh: true,
    })
  }
  expect(runTreeEventEffect(native({ type: "member_finished", member: "missing", child: "child", status: "done" }))).toEqual({
    refresh: true,
  })
  for (const status of ["done", "failed"]) {
    expect(runTreeEventEffect(native({ type: "agent_activity_changed", handle: "explore-1", status }))).toEqual({
      refresh: true,
    })
  }
  for (const status of ["idle", "busy"]) {
    expect(runTreeEventEffect(native({ type: "agent_activity_changed", handle: "explore-1", status }))).toEqual({
      refresh: true,
    })
  }

  for (const event of [
    { type: "session.status", properties: { sessionID: "child", status: { type: "idle" } } },
    { type: "hya.envelope", properties: { event: { type: "member_spawned" } } },
    native({ type: "member_status_changed", member: "member-1", status: "unknown" }),
    native({ type: "unrelated" }),
    { type: "other", properties: { type: "session.created" } },
  ]) {
    expect(runTreeEventEffect(event)).toEqual({ refresh: false })
  }
})

test("flattens nested rows and marks non-session nodes unselectable", () => {
  const tree = parseRunTree({
    session: "root",
    children: [
      {
        session: "child",
        member: { member: "m1", child: "child", subagent_type: "explore", description: "inspect", depth: 1, status: "running", summary: "" },
        roster: { handle: "explore-1", session: "child", agent_type: "explore", mode: "transient", status: "busy", current_task: "inspect tree" },
        children: [
          {
            session: "grandchild",
            member: { member: "m2", child: "grandchild", subagent_type: "plan", description: "plan", depth: 2, status: "done", summary: "ok" },
            children: [],
          },
        ],
      },
      {
        member: { member: "pending", subagent_type: "quick", description: "wait", depth: 1, status: "spawning", summary: "" },
      },
    ],
  })

  const rows = flattenRunTree(tree)
  expect(rows.map((row) => [row.node.session, row.depth, row.selectable])).toEqual([
    ["root", 0, true],
    ["child", 1, true],
    ["grandchild", 2, true],
    [undefined, 1, false],
  ])
  expect(rows[1]?.searchText).toContain("explore-1 explore busy inspect tree")
  expect(treeSessionIDs(tree)).toEqual(new Set(["root", "child", "grandchild"]))
})
