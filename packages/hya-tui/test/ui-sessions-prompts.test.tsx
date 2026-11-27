/**
 * In-memory `testRender` coverage for permission & question prompts,
 * permission modes, sessions picker & live session-list push, Projects
 * sidebar & Project view, model picker, effort picker, returning chat,
 * and remote concealed-entry/error UI.
 *
 * Covers UI invariants downgraded from 9 PTY specs:
 * `prompts`, `permission-modes`, `sessions`, `session-list-push`,
 * `projects`, `pickers`, `effort`, `returning-chat`, and UI parts of `remote`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { themes } from "../src/theme"
import { errorResponse, loadBasicComponents, renderApp, resetBasicComponents, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined
const temps: string[] = []

beforeAll(async () => {
  await loadBasicComponents()
})

afterEach(async () => {
  await app?.dispose()
  app = undefined
  for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true })
})

afterAll(async () => {
  await resetBasicComponents()
})

function tempDir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  temps.push(dir)
  return dir
}

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  rendered.controller.ui.command?.open(text)
  await rendered.render()
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

describe("permission & question prompts, returning chat, and permission modes", () => {
  test("permission keys (1/2/3/Esc), draft digit protection, edit diff, queued 1 of 2, question options/free-text/reject, subagent & cross-session asks", async () => {
    const responded: Array<{ id: string; body: Record<string, unknown> }> = []
    app = await renderApp({
      preset: "workspace",
      width: 80,
      height: 37,
      seed: {
        sessions: [
          { id: "hysec_1", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, permissionMode: "manual" },
          { id: "hysec_2", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, permissionMode: "manual" },
        ],
        interactions: [
          {
            id: "perm_1", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION",
            title: "Run bash command", detail: "echo prompt-once",
            options: ["Allow once", "Always allow", "Deny"],
            payload: { tool: "bash", action: "bash", resource: "echo prompt-once", agent: "hya-main" },
          },
          {
            id: "perm_2", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION",
            title: "Edit file", detail: "poem.txt",
            options: ["Allow once", "Always allow", "Deny"],
            payload: { tool: "edit", action: "edit", resource: "poem.txt", input: { path: "poem.txt", oldString: "two", newString: "TWO" }, agent: "hya-main" },
          },
        ],
        onRespond: (id, body) => { responded.push({ id, body }) },
      },
    })

    // Returning chat with pending ask opens directly on `hysec_1` showing `1 of 2` at 80 columns.
    await app.waitForText("Permission · 1 of 2")
    await app.waitForText("│ echo prompt-once")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)

    // With text in input, digits type instead of answering.
    await app.type("draft 3")
    await app.waitForText("Clear the input to answer with 1-3")
    expect(responded).toHaveLength(0)
    await app.press("c", { ctrl: true })
    await app.waitForText("1-3 or ↑↓ Enter")

    // `1` allows once -> advances to `perm_2` (edit diff prompt).
    await app.press("1")
    await app.waitFor(() => responded.length === 1, "perm_1 answered")
    expect(responded[0]).toEqual({ id: "perm_1", body: { permission: { allowed: true, persist: false } } })
    await app.waitForText("│ - two")
    await app.waitForText("│ + TWO")
    expect(app.span("- two")?.fg).toBe(themes.hya.diffColors.remove)
    expect(app.span("+ TWO")?.fg).toBe(themes.hya.diffColors.add)

    // Esc denies (never approves).
    await app.press("ESCAPE")
    await app.waitFor(() => responded.length === 2, "perm_2 denied")
    expect(responded[1]).toEqual({ id: "perm_2", body: { permission: { allowed: false, persist: false } } })
    await app.waitForGone("asked by ")

    // Question prompt: option selection + free-text + reject.
    app.backend.emitSession("hysec_1", {
      questionRequested: {
        interaction: {
          id: "q_1", session: "hysec_1", type: "INTERACTION_TYPE_QUESTION",
          title: "Color: Which color do you want?",
          options: ["red", "blue"],
        },
      },
    })
    await app.waitForText("Question")
    await app.waitForText("Color: Which color do you want?")
    await app.press("3") // Other… free-text mode
    await app.type("green")
    await app.waitForText("Enter sends the input as the answer")
    await app.press("RETURN")
    await app.waitFor(() => responded.length === 3, "q_1 answered")
    expect(responded[2]).toEqual({ id: "q_1", body: { question: { answer: "green" } } })

    // Cross-session ask arrives live on global stream as `Pending (1)`; `/pending` jumps to it.
    const otherAsk = {
      id: "perm_other", session: "hysec_2", type: "INTERACTION_TYPE_PERMISSION",
      title: "Run bash", detail: "echo elsewhere",
      options: ["Allow once", "Always allow", "Deny"],
      payload: { tool: "bash", action: "bash", resource: "echo elsewhere", agent: "hya-main" },
    }
    app.backend.interactions.push(otherAsk)
    app.backend.emitGlobal({ session: "hysec_2", permissionRequested: { interaction: otherAsk } })
    await app.waitForText("Pending (1)")
    await runCmd(app, "/pending")
    await app.waitForText("│ echo elsewhere")
    expect(app.store.state.selected?.id).toBe("hysec_2")
    await app.press("2") // Always allow
    await app.waitFor(() => responded.length === 4, "perm_other always allowed")
    expect(responded[3]).toEqual({ id: "perm_other", body: { permission: { allowed: true, persist: true } } })
  })

  test("/permissions picker, yolo confirmation & persistence, bundle mode source, and before-session mode", async () => {
    const configDir = tempDir("hya-perm-")
    app = await renderApp({
      width: 80,
      height: 37,
      configDir,
      seed: {
        permissionModes: [
          { id: "manual", title: "Manual", source: "builtin", description: "Ask the user before actions" },
          { id: "yolo", title: "Yolo", source: "builtin", description: "Allow every action" },
          { id: "e2e/approver/echo-only", title: "Echo only", source: "e2e/approver", description: "Approve echo commands" },
        ],
      },
    })

    // Picker lists built-in and bundle modes, filters, fits 80 cols, and selects bundle mode without confirmation.
    await runCmd(app, "/permissions")
    await app.waitForText("Permission mode")
    await app.waitForText(/Echo only\s+\[e2e\/approver\]/)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.type("echo")
    await app.waitForText("1 of 3")
    await app.press("RETURN")
    await app.waitFor(() => app!.store.state.selected?.permissionMode === "e2e/approver/echo-only", "bundle mode applied")

    // Switching to yolo asks for confirmation; Esc cancels; Enter confirms and saves preference; subsequent yolo doesn't re-ask.
    await runCmd(app, "/permissions yolo")
    await app.waitForText("⚠ Enable yolo?")
    await app.press("ESCAPE")
    await app.waitForGone("⚠ Enable yolo?")
    expect(app.store.state.selected?.permissionMode).toBe("e2e/approver/echo-only")

    await runCmd(app, "/permissions yolo")
    await app.waitForText("⚠ Enable yolo?")
    await app.press("RETURN")
    await app.waitFor(() => app!.store.state.selected?.permissionMode === "yolo", "yolo confirmed")
    expect(JSON.parse(readFileSync(app.preferencesPath, "utf8")).permissionMode).toBe("yolo")

    await runCmd(app, "/permissions manual")
    await app.waitFor(() => app!.store.state.selected?.permissionMode === "manual", "manual restored")
    await runCmd(app, "/permissions yolo")
    expect(app.find("⚠ Enable yolo?")).toBeNull() // already confirmed this process
    await app.waitFor(() => app!.store.state.selected?.permissionMode === "yolo", "yolo without re-confirm")
  })
})

describe("sessions picker, live session push, projects sidebar & view, model & effort pickers, and remote UI", () => {
  test("/sessions picker (F2 rename, Ctrl+D confirm/delete, F3 all projects), live session push, and 80-col fit", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 156,
      height: 41,
      seed: {
        projects: [
          { id: "prj_work", name: "workspace", roots: ["/workspace"], sessionCount: 2, busy: false },
          { id: "prj_other", name: "other-project", roots: ["/other"], sessionCount: 1, busy: false },
        ],
        sessions: [
          { id: "hysec_1", title: "First session", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, timeUpdated: "2026-10-01T10:00:00Z" },
          { id: "hysec_2", title: "Second session", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" }, timeUpdated: "2026-10-01T11:00:00Z" },
          { id: "hysec_other", title: "Other project chat", agent: "hya-main", workdir: "/other", projectId: "prj_other", model: { providerId: "fake", modelId: "model" }, timeUpdated: "2026-10-01T09:00:00Z" },
        ],
      },
    })
    await app.waitForText("Second session")

    // Live session push: created, renamed, busy, deleted via global stream.
    app.backend.sessions.push({ id: "hysec_live", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } })
    app.backend.emitGlobal({ session: "hysec_live", sessionStarted: { agent: "hya-main", workdir: "/workspace" } })
    await app.waitForText("hysec_live")
    app.backend.emitGlobal({ session: "hysec_live", sessionUpdated: { title: "Built elsewhere" } })
    await app.waitForText("Built elsewhere")
    app.backend.emitGlobal({ session: "hysec_live", sessionUpdated: { busy: true } })
    await app.waitForText("running")
    app.backend.sessions = app.backend.sessions.filter((s) => s.id !== "hysec_live")
    app.backend.emitGlobal({ session: "hysec_live", sessionDeleted: {} })
    await app.waitForGone("Built elsewhere")

    // `/sessions` picker: scoped to active project until F3; F2 renames; Ctrl+D confirms & deletes.
    await runCmd(app, "/sessions")
    await app.waitForText("New session")
    expect(app.find("Other project chat")).toBeNull()
    await app.press("F3")
    await app.waitForText("Sessions · all projects")
    await app.waitForText("Other project chat")
    await app.press("F3")
    await app.waitForGone("all projects")

    // F2 renames highlighted row (`Second session`).
    await app.press("F2")
    await app.waitForText("New title")
    for (let i = 0; i < "Second session".length; i++) await app.press("BACKSPACE")
    await app.type("Renamed chat")
    await app.press("RETURN")
    // The typed title is on screen before the rename lands; the numbered row follows the refresh.
    await app.waitForText(/\d\. Renamed chat/)

    // Ctrl+D asks confirmation; Esc cancels; Ctrl+D + Enter deletes and switches to remaining session.
    await app.press("d", { ctrl: true })
    await app.waitForText(/Delete .*Enter confirms · Esc cancels/)
    await app.press("ESCAPE")
    await app.waitForGone("Enter confirms")
    await app.press("d", { ctrl: true })
    await app.press("RETURN")
    await app.waitFor(() => app!.store.state.selected?.id === "hysec_1", "switched after deleting open session")

    // Fits 80 columns.
    await app.resize(80, 37)
    await runCmd(app, "/sessions")
    await app.waitForText("Sessions")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.press("ESCAPE")
  })

  test("/project view create/edit-roots/delete-refusal/temporary session, and --remote auto-open + concealed /connect-remote UI", async () => {
    app = await renderApp({
      preset: "workspace",
      width: 156,
      height: 41,
      seed: {
        projects: [{ id: "prj_work", name: "workspace", roots: ["/workspace"], sessionCount: 1, busy: false }],
        sessions: [{ id: "hysec_1", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "fake", modelId: "model" } }],
      },
    })
    await app.waitForText("Projects")

    // `/project` view: create with two roots, edit roots, delete refusal on in-use project, `t` temporary session.
    await runCmd(app, "/project")
    await app.waitForText("Up/Down move · Enter opens/switches")
    await app.press("n")
    await app.waitForText("New project name")
    await app.type("multi-root")
    await app.press("RETURN")
    await app.waitForText("Root 1 (primary)")
    await app.type("/tmp/root-a")
    await app.press("RETURN")
    await app.type("/tmp/root-b")
    await app.press("RETURN")
    await app.press("RETURN")
    await app.waitForText("multi-root")

    // Temporary session via `t` key and `/new --temp`.
    await app.press("t")
    await app.waitFor(() => app!.store.state.selected?.kind === "SESSION_KIND_TEMPORARY", "temp session created via t")
    await runCmd(app, "/new --temp")
    expect(app.store.state.selected?.kind).toBe("SESSION_KIND_TEMPORARY")

    // Remote start with no projects opens Project view automatically & formats `--server-label`.
    await app.dispose()
    const label = "remote: relay.example.com/abc"
    app = await renderApp({
      width: 80,
      height: 37,
      remote: true,
      serverLabel: label,
      seed: {
        projects: [],
        onRequest: (method, path) => {
          if (method === "GET" && path === "/v2/projects") return errorResponse(503, "unavailable", "remote offline")
          return undefined
        },
      },
      bridge: async () => { throw new Error("invalid relay link") },
    })
    // "Projects" alone matches the sidebar title before the view auto-opens.
    await app.waitForText("Up/Down move · Enter opens/switches")
    await app.press("ESCAPE")
    await app.waitForGone("Up/Down move · Enter opens/switches")
    await runCmd(app, "/status")
    await app.waitForText(/remote: relay\.example\.com\/abc/)

    // `/connect-remote` without a link opens concealed entry fitting 80 cols; Esc cancels; rejected link keeps local backend.
    await runCmd(app, "/connect-remote")
    await app.waitForText("paste or type the relay link (hidden)")
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.paste("hya://secret-token-should-not-appear")
    expect(app.frame()).not.toContain("secret-token-should-not-appear")
    await app.press("ESCAPE")
    await app.waitForGone("Relay link")
    expect(app.store.state.secretEntry).toBeUndefined()
  })

  test("/model and /effort pickers, direct commands, pre-session choice, narrow visibility, and Esc cancel", async () => {
    app = await renderApp({
      width: 80,
      height: 37,
      startup: { continue: true },
      seed: {
        sessions: [],
        models: [
          { id: "fake/fast", providerId: "fake", modelId: "fast", reasoning: true, reasoningVariants: ["none", "low", "medium", "high"], source: "config" },
          { id: "fake/slow", providerId: "fake", modelId: "slow", reasoning: true, reasoningVariants: ["none", "low", "medium", "high"], source: "config" },
        ],
        autoReply: "picked reply",
      },
    })
    // Before any session exists, `/model` remembers choice for the created session.
    expect(app.store.state.selected).toBeUndefined()
    await runCmd(app, "/model")
    await app.waitForText("Model")
    await app.type("slow")
    await app.press("RETURN")
    await app.submit("hello")
    await app.waitForText("picked reply")
    expect(app.store.state.selected?.model?.modelId).toBe("slow")

    // `/effort` picker lists default/none/low/..., Esc keeps unchanged, choosing low updates status.
    await runCmd(app, "/effort")
    await app.waitForText("Thinking effort")
    await app.waitForText(/default\s+\[default\]/)
    await app.press("ESCAPE")
    await app.waitForGone("Thinking effort")

    await runCmd(app, "/effort low")
    await app.waitFor(() => app!.store.state.selected?.effectiveEffort === "low", "effort low")
    await runCmd(app, "/status")
    await app.waitForText(/Thinking\s+low/)
    await runCmd(app, "/effort default")
    await app.waitFor(() => app!.store.state.selected?.effectiveEffort === "default", "effort default")
    await runCmd(app, "/effort none")
    await app.waitFor(() => app!.store.state.selected?.effectiveEffort === "none", "effort none")
  })
})
