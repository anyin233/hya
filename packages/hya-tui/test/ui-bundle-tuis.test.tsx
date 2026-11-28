/**
 * In-memory `@opentui/solid` `testRender` integration tests verifying all 5
 * first-party `@hya/tui-sdk` v2 TUI bundles (`basic-tui-components`,
 * `multi-agent`, `workflow`, `plan`, `agent-channels`) running end-to-end
 * inside the real `<App />` + `ExtensionManager` host:
 * - Dynamic named panes (`pane.open`, `pane.pin`, `pane.close`)
 * - Host-virtualized `{ type: "transcript", sessionId }` rendering
 * - Bundle-confined `bundle/api` and `extension_state`
 * - Extension slash commands (`/subagents`, `/workflow`)
 * - Coexisting multi-bundle `tool_call` renderers and status items
 * - Clean disappearance of dynamic panes and surfaces when their owning bundle is disabled
 */
import { afterEach, expect, test } from "bun:test"
import { extensionManager } from "../src/extensions/manager"
import { builtinTuiCatalogEntry, renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined
afterEach(async () => {
  if (app) {
    await app.dispose()
    app = undefined
  }
})

test("multi-agent bundle: /subagents opens dynamic panes, embeds child transcript, calls bundle.api, and disappears when disabled", async () => {
  app = await renderApp({
    width: 160,
    height: 40,
    preset: "workspace",
    loadExtensions: true,
    seed: {
      sessions: [
        {
          id: "hysec_1",
          title: "Lead",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          kind: "SESSION_KIND_PROJECT",
          members: [
            {
              member: "mem_1",
              child: "hysec_child_1",
              handle: "hya-scout-1",
              agent: "hya-scout",
              description: "Explore codebase",
              status: "MEMBER_STATUS_RUNNING",
              summary: "Inspecting bundles/",
            },
          ],
          extensions: {
            "hya/multi-agent": {
              members: [
                {
                  member: "mem_1",
                  child: "hysec_child_1",
                  subagent_type: "hya-scout",
                  description: "Explore codebase",
                  depth: 0,
                  status: "running",
                  summary: "Inspecting bundles/",
                },
              ],
              roster: {
                "hya-scout-1": {
                  handle: "hya-scout-1",
                  session: "hysec_child_1",
                  agent_type: "hya-scout",
                  status: "busy",
                },
              },
              archived: {},
            },
          },
        },
        {
          id: "hysec_child_1",
          title: "Scout Child",
          agent: "hya-scout",
          parent: "hysec_1",
          workdir: "/workspace",
          projectId: "prj_work",
          kind: "SESSION_KIND_PROJECT",
        },
      ],
      messages: {
        hysec_1: [
          { id: "m1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "Spawn a scout" } }], finish: "FINISH_REASON_STOP" },
        ],
        hysec_child_1: [
          {
            id: "cm1",
            role: "ROLE_ASSISTANT",
            agent: "hya-scout",
            parts: [{ id: "cp1", text: { text: "CHILD_TRANSCRIPT_LIVE_LINE_42" } }],
            finish: "FINISH_REASON_STOP",
          },
        ],
      },
      tuiExtensions: [
        builtinTuiCatalogEntry("basic-tui-components"),
        builtinTuiCatalogEntry("multi-agent"),
      ],
    },
  })

  await app.waitFor(
    () => extensionManager.panes().some((p) => p.key === "hya/multi-agent#subagents"),
    "hya/multi-agent panes registered",
  )

  // `/subagents` dispatches to the bundle's slash command, opening both dynamic panes.
  await app.controller.submit("/subagents", "command")
  await app.waitForText("hya-scout-1")
  // Host-virtualized `{ type: "transcript", sessionId: "hysec_child_1" }` renders live child messages!
  await app.waitForText("CHILD_TRANSCRIPT_LIVE_LINE_42")
  expect(app.backend.calls.some((c) => c.method === "GET" && c.path.includes("/v2/sessions/hysec_1/bundles/hya%2Fmulti-agent/members"))).toBe(true)

  // `/subagents steer` invokes POST /v2/sessions/hysec_child_1/bundles/hya%2Fmulti-agent/steer.
  await app.controller.submit("/subagents steer hysec_child_1 focus on tui", "command")
  expect(app.backend.calls.some((c) => c.method === "POST" && c.path.includes("/v2/sessions/hysec_child_1/bundles/hya%2Fmulti-agent/steer"))).toBe(true)

  // Disabling `hya/multi-agent` cleanly removes both dynamic panes from the visible workspace.
  await extensionManager.setEnabled("hya/multi-agent", false)
  await app.waitForGone("CHILD_TRANSCRIPT_LIVE_LINE_42")
  await app.waitForGone("Subagent Viewer")
}, 15_000)

test("workflow bundle: /workflow opens dynamic Workflows pane, calls bundle.api, and disappears when disabled", async () => {
  app = await renderApp({
    width: 160,
    height: 40,
    preset: "workspace",
    loadExtensions: true,
    seed: {
      sessions: [
        {
          id: "hysec_1",
          title: "Workflow Session",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          kind: "SESSION_KIND_PROJECT",
          extensions: {
            "hya/workflow": {
              selection: { name: "plan-impl-review" },
              run: {
                id: "wfr_9",
                status: "running",
                stages: [
                  { plan: { id: "plan", title: "Plan Stage" }, status: "completed" },
                  { plan: { id: "impl", title: "Impl Stage" }, status: "running" },
                ],
              },
            },
          },
        },
      ],
      workflows: [
        { name: "plan-impl-review", description: "Plan, implement, and review changes" },
      ],
      workflowState: {
        hysec_1: {
          selection: { name: "plan-impl-review" },
          run: {
            id: "wfr_9",
            status: "running",
            stages: [
              { plan: { id: "plan", title: "Plan Stage" }, status: "completed" },
              { plan: { id: "impl", title: "Impl Stage" }, status: "running" },
            ],
          },
        },
      },
      tuiExtensions: [
        builtinTuiCatalogEntry("basic-tui-components"),
        builtinTuiCatalogEntry("workflow"),
      ],
    },
  })

  await app.waitFor(
    () => extensionManager.panes().some((p) => p.key === "hya/workflow#workflows"),
    "hya/workflow pane registered",
  )

  await app.controller.submit("/workflow", "command")
  await app.waitForText("Plan Stage")
  await app.waitForText("Impl Stage")
  expect(app.backend.calls.some((c) => c.method === "GET" && c.path.includes("/v2/bundles/hya%2Fworkflow/api/workflows"))).toBe(true)
  expect(app.backend.calls.some((c) => c.method === "GET" && c.path.includes("/v2/sessions/hysec_1/bundles/hya%2Fworkflow/state"))).toBe(true)

  await app.controller.submit("/workflow select plan-impl-review", "command")
  expect(app.backend.calls.some((c) => c.method === "POST" && c.path.includes("/v2/sessions/hysec_1/bundles/hya%2Fworkflow/command"))).toBe(true)

  // Disabling `hya/workflow` removes the dynamic Workflows pane cleanly.
  await extensionManager.setEnabled("hya/workflow", false)
  await app.waitForGone("Plan Stage")
}, 15_000)

test("plan, agent-channels, and basic-tui-components coexist: tool cards, status badges, and jobs panel", async () => {
  app = await renderApp({
    width: 160,
    height: 44,
    preset: "workspace",
    loadExtensions: true,
    seed: {
      sessions: [
        {
          id: "hysec_1",
          title: "Plan Session",
          agent: "hya-plan",
          workdir: "/workspace",
          projectId: "prj_work",
          kind: "SESSION_KIND_PROJECT",
          extensions: {
            "hya/agent-channels": {
              inboxes: { lead: [0, 1] },
              cursors: { lead: 0 },
              mail: [
                { from: "scout", body: "done 1" },
                { from: "reviewer", body: "done 2" },
              ],
            },
          },
        },
        {
          id: "hysec_busy",
          title: "Background Build",
          agent: "hya-main",
          workdir: "/workspace",
          projectId: "prj_work",
          kind: "SESSION_KIND_PROJECT",
          busy: true,
        },
      ],
      messages: {
        hysec_1: [
          {
            id: "msg_1",
            role: "ROLE_ASSISTANT",
            agent: "hya-plan",
            parts: [
              {
                id: "tc_plan",
                toolCall: {
                  tool: "plan_exit",
                  state: "TOOL_EXECUTION_STATE_OK",
                  inputJson: "{}",
                  outputJson: JSON.stringify({ output: "User approved switching to build agent." }),
                },
              },
              {
                id: "tc_send",
                toolCall: {
                  tool: "send",
                  state: "TOOL_EXECUTION_STATE_OK",
                  inputJson: JSON.stringify({ channel: "#unit-default", body: "Dispatching phase 5" }),
                  outputJson: JSON.stringify({ output: "sent" }),
                },
              },
              {
                id: "tc_report",
                toolCall: {
                  tool: "report",
                  state: "TOOL_EXECUTION_STATE_OK",
                  inputJson: JSON.stringify({ outcome: "done", result: "Bundle UI ready" }),
                  outputJson: JSON.stringify({ output: "recorded" }),
                },
              },
            ],
            finish: "FINISH_REASON_STOP",
          },
        ],
      },
      tuiExtensions: [
        builtinTuiCatalogEntry("basic-tui-components"),
        builtinTuiCatalogEntry("plan"),
        builtinTuiCatalogEntry("agent-channels"),
      ],
    },
  })

  await app.waitFor(
    () =>
      extensionManager.list().filter((e) => e.state === "running").length === 3
      && extensionManager.replacement("jobs")?.key === "hya/basic-tui-components#jobs",
    "basic-tui-components, plan, and agent-channels running",
  )

  // Both plan and agent-channels tool-call renderers render in the same transcript!
  await app.waitForText("Plan Mode Exit")
  await app.waitForText("send → #unit-default")
  await app.waitForText("report · done")

  // Both status items appear in the Context pane!
  await app.waitForText("PLAN (read-only)")
  await app.waitForText("2 unread")

  // Open the jobs pane via /layout wrap root row jobs after -> rendered by hya/basic-tui-components#jobs!
  await app.controller.submit("/layout wrap root row jobs after", "command")
  await app.waitForText("Background Build · busy")

  // Disabling plan and agent-channels cleanly removes their status badges and custom cards.
  await extensionManager.setEnabled("hya/plan", false)
  await app.waitForGone("PLAN (read-only)")
  await extensionManager.setEnabled("hya/agent-channels", false)
  await app.waitForGone("2 unread")
}, 15_000)
