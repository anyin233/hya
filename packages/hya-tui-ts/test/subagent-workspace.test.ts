import { expect, test } from "bun:test"

import { CommandMap, Definitions } from "../src/upstream/config/keybind"
import {
  RunTreeParseError,
  createRunTreeLoader,
  createWorkspaceState,
  flattenRunTree,
  parseRunTree,
  reduceWorkspace,
  runTreeEventEffect,
  terminalTreeSessionIDs,
  treeSessionIDs,
  workspaceLeaves,
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

test("splits only the focused leaf", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })

  expect(
    workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID)),
  ).toEqual(["main", "child-a", "child-b"])
  expect(state.tabs[0]?.root).toMatchObject({
    type: "split",
    axis: "vertical",
    first: { type: "main" },
    second: {
      type: "split",
      axis: "horizontal",
      first: { sessionID: "child-a" },
      second: { sessionID: "child-b" },
    },
  })
  expect(state.focusedPaneID).toBe("observation:child-b")
})

test("closes one leaf and collapses only its parent", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "close", paneID: "observation:child-a" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main", "observation:child-b"])
  expect(state.focusedPaneID).toBe("observation:child-b")

  state = reduceWorkspace(state, { type: "close", paneID: "observation:child-b" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main"])
  expect(state.focusedPaneID).toBe("main")

  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  state = reduceWorkspace(state, { type: "close", paneID: "observation:child-c" })
  expect(state.tabs.map((tab) => tab.id)).toEqual(["main"])
  expect(state.activeTabID).toBe("main")
})

test("cycles focus in tab and visual leaf order", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  state = reduceWorkspace(state, { type: "focusMain" })

  const order = []
  for (let index = 0; index < 4; index++) {
    state = reduceWorkspace(state, { type: "cycleFocus" })
    order.push(state.focusedPaneID)
  }
  expect(order).toEqual(["observation:child-a", "observation:child-b", "observation:child-c", "main"])
  state = reduceWorkspace(state, { type: "focus", paneID: "observation:child-b" })
  expect(reduceWorkspace(state, { type: "focusMain" }).focusedPaneID).toBe("main")
})

test("defers focused terminal closure until focus leaves", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "terminal", sessionIDs: ["child-a", "child-b"] })

  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main", "observation:child-b"])
  expect(workspaceLeaves(state).find((pane) => pane.id === "observation:child-b")).toMatchObject({ closeOnBlur: true })
  state = reduceWorkspace(state, { type: "focusMain" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main"])
  expect(state.focusedPaneID).toBe("main")
})

test("defers an already-terminal observation until focus leaves", () => {
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
  state = reduceWorkspace(state, { type: "terminal", sessionIDs: [...terminalTreeSessionIDs(tree)] })

  expect(workspaceLeaves(state).find((pane) => pane.id === "observation:child")).toMatchObject({ closeOnBlur: true })
  state = reduceWorkspace(state, { type: "focusMain" })
  expect(workspaceLeaves(state).map((pane) => pane.id)).toEqual(["main"])
})

test("prunes sessions missing from a successful tree", () => {
  let state = createWorkspaceState("root")
  state = reduceWorkspace(state, { type: "openSplit", axis: "vertical", sessionID: "child-a" })
  state = reduceWorkspace(state, { type: "openSplit", axis: "horizontal", sessionID: "child-b" })
  state = reduceWorkspace(state, { type: "openTab", sessionID: "child-c" })
  state = reduceWorkspace(state, { type: "reconcileSessions", sessionIDs: ["root", "child-b"] })

  expect(workspaceLeaves(state).map((pane) => (pane.type === "main" ? "main" : pane.sessionID))).toEqual([
    "main",
    "child-b",
  ])
  expect(state.tabs).toHaveLength(1)
  expect(state.focusedPaneID).toBe("main")
  expect(state.activeTabID).toBe("main")
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

test("recognizes only tree and terminal event variants", () => {
  const tree = parseRunTree({
    session: "root",
    children: [
      {
        session: "child",
        member: { member: "member-1", child: "child", subagent_type: "explore", description: "inspect", depth: 1, status: "running", summary: "" },
        roster: { handle: "explore-1", session: "child", agent_type: "explore", mode: "transient", status: "busy" },
      },
    ],
  })
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
  expect(invalidators.filter((event) => runTreeEventEffect(event, tree).refresh)).toHaveLength(8)

  for (const status of ["done", "failed", "cancelled"]) {
    expect(
      runTreeEventEffect(native({ type: "member_status_changed", member: "member-1", status }), tree)
        .terminalSessionIDs,
    ).toEqual(["child"])
  }
  expect(
    runTreeEventEffect(native({ type: "member_finished", member: "missing", child: "child", status: "done" }), tree)
      .terminalSessionIDs,
  ).toEqual(["child"])
  for (const status of ["done", "failed"]) {
    expect(runTreeEventEffect(native({ type: "agent_activity_changed", handle: "explore-1", status }), tree).terminalSessionIDs).toEqual([
      "child",
    ])
  }
  for (const status of ["idle", "busy"]) {
    const effect = runTreeEventEffect(native({ type: "agent_activity_changed", handle: "explore-1", status }), tree)
    expect(effect.refresh).toBe(true)
    expect(effect.terminalSessionIDs).toEqual([])
  }

  for (const event of [
    { type: "session.status", properties: { sessionID: "child", status: { type: "idle" } } },
    { type: "hya.envelope", properties: { event: { type: "member_spawned" } } },
    native({ type: "member_status_changed", member: "member-1", status: "unknown" }),
    native({ type: "unrelated" }),
    { type: "other", properties: { type: "session.created" } },
  ]) {
    const effect = runTreeEventEffect(event, tree)
    expect(effect.refresh).toBe(false)
    expect(effect.terminalSessionIDs).toEqual([])
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
    ["root", 0, false],
    ["child", 1, true],
    ["grandchild", 2, true],
    [undefined, 1, false],
  ])
  expect(rows[1]?.searchText).toContain("explore-1 explore busy inspect tree")
  expect(treeSessionIDs(tree)).toEqual(new Set(["root", "child", "grandchild"]))
})
