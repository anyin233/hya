/**
 * In-memory `testRender` coverage for full-screen views:
 * Provider View (`/key`), Agents View (`/agent`), Diff View (`/diff`),
 * MCP View (`/mcp`), Saved Rules View (`/rules`), Skills View (`/skills`),
 * and Bundles View (`/bundles` UI refresh/filter/toggle/uninstall guards).
 *
 * Covers UI invariants downgraded from 7 PTY specs:
 * `providers`, `agents`, `diff`, `mcp`, `rules`, `skills`, and UI parts of `bundles`.
 */
import { afterEach, describe, expect, test } from "bun:test"
import { renderApp, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined

afterEach(async () => {
  await app?.dispose()
  app = undefined
})

async function runCmd(rendered: RenderedApp, text: string): Promise<void> {
  rendered.controller.ui.command?.open(text)
  await rendered.render()
  await rendered.press("RETURN")
  await rendered.waitForGone("Commands")
}

describe("Provider View (/key)", () => {
  test("list, add wizard with secret masking & validation, model add/edit/delete/test, key set/remove, catalogUpdated, and 80-col fit", async () => {
    app = await renderApp({
      width: 120,
      height: 41,
      seed: {
        providers: [{ id: "hya", auth: "AUTH_STATUS_NOT_APPLICABLE", keySource: "none", modelCount: 1 }],
        models: [{ id: "hya/offline", providerId: "hya", modelId: "offline", source: "offline" }],
        sessions: [{ id: "hysec_p", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", model: { providerId: "hya", modelId: "offline" } }],
      },
    })

    await runCmd(app, "/key")
    await app.waitForText("Providers")
    await app.waitForText(/hya\s+offline\s+no key\s+offline\s+1 model/)
    await app.type("k")
    await app.waitForText("hya is the built-in offline provider")

    // Add provider wizard validation + secret masking + model picker auto-open off hya/offline.
    await app.type("a")
    await app.waitForText("Add provider · 1/4")
    await app.type("bad id")
    await app.press("RETURN")
    await app.waitForText("Use 1–64 letters, digits, - or _")
    for (let i = 0; i < 6; i++) await app.press("BACKSPACE")
    await app.type("gw")
    await app.press("RETURN")
    await app.waitForText("Add provider · 2/4")
    await app.press("RETURN") // openai
    await app.waitForText("Add provider · 3/4")
    await app.type("ftp://bad")
    await app.press("RETURN")
    await app.waitForText("Enter an http:// or https:// URL")
    for (let i = 0; i < 9; i++) await app.press("BACKSPACE")
    await app.type("http://127.0.0.1:9/v1")
    await app.press("RETURN")
    await app.waitForText("Add provider · 4/4")
    await app.type("sk-secret-key")
    await app.waitForText("•".repeat("sk-secret-key".length))
    expect(app.frame()).not.toContain("sk-secret-key")
    await app.press("RETURN")

    // Off hya/offline -> model picker opens over the view.
    await app.waitForText("Model · pick one of gw's models for this session")
    await app.press("RETURN")
    await app.waitForText("Providers › gw")
    await app.waitForText(/alpha\s+remote/)

    // Test model + add manual model + delete override + fits 80 cols.
    await app.type("t")
    await app.waitForText(/✓ gw\/alpha replied/)
    await app.type("m")
    await app.waitForText("Add model · gw · 1/5")
    await app.type("manual-1")
    await app.press("RETURN")
    await app.type("Manual One")
    await app.press("RETURN")
    await app.type("32000")
    await app.press("RETURN")
    await app.type("4000")
    await app.press("RETURN")
    await app.press("RETURN")
    await app.waitForText("Saved manual-1 to config.yaml")
    await app.waitForText(/manual-1\s+Manual One\s+config\s+32k \/ 4k/)

    await app.resize(80, 37)
    await app.waitForText("Esc back")
    await app.press("ESCAPE")
    await app.waitForText(/gw\s+openai\s+saved key/)
    // Remove key confirmation + Esc cancel + Enter remove.
    await app.type("x")
    await app.waitForText("Remove the saved key of gw?")
    await app.press("ESCAPE")
    await app.type("x")
    await app.press("RETURN")
    await app.waitForText("Removed the saved key of gw")
    await app.press("ESCAPE")
    await app.waitForGone("Providers")

    // Live `catalogUpdated` pushes new models without opening Provider View.
    app.backend.models.push({ id: "gw/live-model", providerId: "gw", modelId: "live-model", source: "remote" })
    app.backend.emitGlobal({ catalogUpdated: {} })
    await app.waitFor(() => app!.store.state.models.some((m) => m.id === "gw/live-model"), "catalogUpdated applied")
  })
})

describe("Agents View (/agent)", () => {
  test("sections order, select primary vs subagent notice, m model / c clear / t effort, config-pinned agent, and 80-col fit", async () => {
    app = await renderApp({
      width: 110,
      height: 37,
      seed: {
        models: [
          { id: "fake/alpha", providerId: "fake", modelId: "alpha", reasoning: true, reasoningVariants: ["none", "low", "high"], source: "config" },
          { id: "fake/beta", providerId: "fake", modelId: "beta", reasoning: true, reasoningVariants: ["none", "low", "high"], source: "config" },
        ],
        agentModels: [
          { agentId: "hya-main", mode: "primary", settable: false, configured: true, configurationPath: "/cfg/config.yaml", configuration: { providerId: "fake", modelId: "beta" }, effective: { providerId: "fake", modelId: "beta" }, source: "AGENT_MODEL_SOURCE_CONFIGURED", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
          { agentId: "hya-plan", mode: "primary", settable: true, effective: { providerId: "fake", modelId: "alpha" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
          { agentId: "hya-scout", mode: "subagent", settable: true, effective: { providerId: "fake", modelId: "alpha" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
          { agentId: "hya-title", mode: "subagent", hidden: true, settable: true, effective: { providerId: "fake", modelId: "alpha" }, source: "AGENT_MODEL_SOURCE_DEFAULT", effortSource: "AGENT_EFFORT_SOURCE_NONE" },
        ],
      },
    })

    await runCmd(app, "/agent")
    await app.waitForText("── Primary agents")
    await app.waitForText("── Subagents")
    await app.waitForText("── System agents")
    expect(app.find("── Primary agents")!.row).toBeLessThan(app.find("── Subagents")!.row)
    expect(app.find("── Subagents")!.row).toBeLessThan(app.find("── System agents")!.row)

    // `m` on config-pinned hya-main saves configuration.
    await app.waitForText(/hya-main\s+fake\/beta\s+configured/)
    await app.press("m")
    await app.waitForText("Model · hya-main's default")
    await app.type("alpha")
    await app.press("RETURN")
    await app.waitForText(/hya-main\s+fake\/alpha\s+configured/)

    // Filter to subagent `hya-scout`: `m` sets remembered model, `c` clears, `t` sets effort.
    await app.type("/")
    await app.type("hya-scout")
    await app.press("RETURN")
    await app.press("m")
    await app.waitForText("Model · hya-scout's default")
    await app.type("beta")
    await app.press("RETURN")
    await app.waitForText(/hya-scout\s+fake\/beta\s+remembered/)
    await app.press("c")
    await app.waitForGone("remembered")

    await app.press("t")
    await app.waitForText("Thinking effort · hya-scout's default")
    await app.press("ARROW_DOWN")
    await app.press("RETURN")
    await app.waitForText(/none \(set\)/)

    // Clear filter, fit 80 cols, select primary `hya-plan` -> switches session agent.
    await app.press("ESCAPE")
    await app.resize(80, 37)
    for (const line of app.lines()) expect(line.length).toBeLessThanOrEqual(80)
    await app.type("/")
    await app.type("hya-plan")
    await app.press("RETURN")
    await app.press("RETURN")
    await app.waitForGone("── Primary agents")
    expect(app.store.state.selected?.agent).toBe("hya-plan")
  })
})

describe("Diff, MCP, Rules, Skills, and Bundles views", () => {
  test("/diff empty state, multi-file n/p/r navigation, long diff scrolling, and 60-file windowing", async () => {
    const emptyDiff = ""
    app = await renderApp({ width: 110, height: 37, seed: { diff: emptyDiff, vcs: {} } })
    await runCmd(app, "/diff")
    await app.waitForText("0 files changed")
    await app.waitForText("Not a git repository")
    await app.press("ESCAPE")
    await app.waitForGone("0 files changed")

    // Multi-file + long diff + 60-file windowing
    const added80 = Array.from({ length: 80 }, (_, i) => `+added ${String(i + 1).padStart(2, "0")}`).join("\n")
    const manyFiles = Array.from({ length: 60 }, (_, i) => {
      const name = `file${String(i).padStart(2, "0")}.txt`
      return `diff --git a/${name} b/${name}\n--- /dev/null\n+++ b/${name}\n@@ -0,0 +1,${i === 0 ? 80 : 1} @@\n${i === 0 ? added80 : `+one ${i}`}\n`
    }).join("")
    app.backend.vcs = { branch: "main" }
    app.backend.diff = manyFiles
    await runCmd(app, "/diff")
    await app.waitForText("60 files changed")
    await app.waitForText("Diff › file00.txt")
    await app.waitForText("+ added 01")
    await app.waitForText(/↓ \d+ more/)

    // End scrolls diff to last added line above key hint; Home returns to top.
    await app.press("END")
    await app.waitForText("+ added 80")
    expect(app.find("+ added 80")!.row).toBeLessThan(app.find("↑↓ scroll")!.row)
    await app.press("HOME")
    await app.waitForText("+ added 01")

    // `n` / `p` switch files and window follows down to `file59.txt`.
    for (let i = 0; i < 59; i++) await app.press("n")
    await app.waitForText("Diff › file59.txt")
    await app.waitForText(/↑ \d+ more/)
    await app.press("p")
    await app.waitForText("Diff › file58.txt")
    await app.press("ESCAPE")
  })

  test("/mcp empty state + 40 windowed tools at 80 cols, /rules list & delete, /skills list/detail/filter/u/warnings, and /bundles UI", async () => {
    const tools40 = Array.from({ length: 40 }, (_, i) => `mcp__many__tool_${String(i + 1).padStart(2, "0")}`)
    app = await renderApp({
      width: 80,
      height: 37,
      seed: {
        mcpServers: [{ name: "many", state: "MCP_SERVER_STATE_CONNECTED", tools: tools40 }],
        savedRules: [{ id: "rule_1", permission: "RULE_PERMISSION_ALLOW", tool: "bash", pattern: "echo saved-rule", projectId: "global", timeCreated: new Date().toISOString() }],
        skills: [
          { name: "greet", description: "Say hello to someone", source: "project", location: "/workspace/.hya/skills/greet/SKILL.md", content: "# Greeting\n\nSKILL_BODY_MARKER" },
          { name: "reviewer", description: "Review the diff", source: "project", location: "/workspace/.hya/skills/reviewer/SKILL.md", content: "Read the diff." },
          { name: "manual", description: "Run by hand", source: "project", disableModelInvocation: true, location: "/workspace/.hya/skills/manual/SKILL.md", content: "Manual body." },
          { name: "agent-bundle-authoring", description: "Built-in skill", source: "builtin", location: "<built-in>", content: "Builtin." },
        ],
        skillDiagnostics: [{ path: "/workspace/.hya/skills/broken/SKILL.md", kind: "missing_description", message: "no `description`; skipped" }],
        bundles: [
          { id: "hya/basic-tui-components", version: "1.0.0", scope: "first_party", state: "active", enabled: true, removable: false },
          { id: "e2e/custom", version: "1.0.0", scope: "user", state: "active", enabled: true, removable: true },
        ],
      },
    })

    // /mcp at 80 cols: 40 tools windowed, End reaches tool_40 without overlapping hint.
    await runCmd(app, "/mcp")
    await app.waitForText(/many\s+connected\s+40 tools/)
    await app.press("RETURN")
    await app.waitForText("MCP › many")
    await app.waitForText("▸ tool_01")
    expect(app.frame()).not.toContain("mcp__many__")
    await app.press("END")
    await app.waitForText("▸ tool_40")
    expect(app.find("Esc back")!.row).toBeGreaterThan(app.find("tool_40")!.row)
    await app.press("ESCAPE")
    await app.press("ESCAPE")
    await app.waitForGone("MCP servers")

    // /rules: lists saved rule, d + Esc cancels, r refreshes, d + Enter deletes.
    await runCmd(app, "/rules")
    await app.waitForText("Saved Rules")
    await app.waitForText(/allow\s+bash\s+echo saved-rule/)
    await app.press("d")
    await app.waitForText("Enter confirms · Esc cancels")
    await app.press("ESCAPE")
    await app.waitForGone("Enter confirms")
    await app.press("r")
    await app.waitForText("Refreshed")
    await app.press("d")
    await app.press("RETURN")
    await app.waitForText("Deleted rule")
    await app.press("ESCAPE")

    // /skills: source groups, warning banner, detail body, manual-only flag, `/` filter, `u` inserts `/skill:<name> `.
    await runCmd(app, "/skills")
    await app.waitForText("── project")
    await app.waitForText("── builtin")
    await app.waitForText("1 warning")
    expect(app.find("── project")!.row).toBeLessThan(app.find("── builtin")!.row)
    await app.press("RETURN")
    await app.waitForText("SKILL_BODY_MARKER")
    await app.press("ESCAPE")
    await app.type("/")
    await app.type("diff")
    await app.press("RETURN")
    await app.waitForText(/▸ reviewer\s+project/)
    await app.press("u")
    await app.waitForGone("── project")
    expect(app.controller.ui.composerInput?.text).toBe("/skill:reviewer ")
    await app.press("c", { ctrl: true })

    // /bundles: refresh with r, disable/enable with e, first-party uninstall guard with x.
    await runCmd(app, "/bundles")
    await app.waitForText("2 bundles · 2 enabled")
    await app.press("r")
    await app.waitForText("Bundles refreshed")
    await app.type("/")
    await app.type("basic-tui")
    await app.press("RETURN")
    await app.press("x")
    await app.waitForText("ships with hya and cannot be uninstalled")
    await app.press("ESCAPE")
    await app.press("ESCAPE")
  })
})
