import { afterEach, expect, test } from "bun:test"
import { extensionManager } from "../src/extensions/manager"
import { builtinTuiCatalogEntry, renderApp, type FakeBackendSeed, type RenderedApp } from "./support/appHarness"

let app: RenderedApp | undefined
afterEach(async () => { await app?.dispose(); app = undefined })

function seed(bundle = false): FakeBackendSeed {
  return {
    sessions: [
      { id: "hysec_root", agent: "hya-main", workdir: "/workspace", projectId: "prj_work", kind: "SESSION_KIND_PROJECT" },
      { id: "hysec_routes", parent: "hysec_root", agent: "hya-task", workdir: "/workspace", busy: true },
      { id: "hysec_tests", parent: "hysec_root", agent: "hya-task", workdir: "/workspace", busy: true },
    ],
    messages: {
      hysec_root: [{
        id: "m1", role: "ROLE_ASSISTANT", parts: [{
          id: "p1", toolCall: {
            tool: "task", state: "TOOL_EXECUTION_STATE_OK",
            inputJson: JSON.stringify({ tasks: [{ description: "map routes" }, { description: "map tests" }] }),
            outputJson: JSON.stringify({ output: "two launches", metadata: { members: [
              { sessionId: "hysec_routes", handle: "main/routes" },
              { sessionId: "hysec_tests", handle: "main/tests" },
            ] } }),
          },
        }],
      }],
    },
    tuiExtensions: bundle ? [builtinTuiCatalogEntry("multi-agent")] : [],
  }
}

test("a bare transcript shows the unknown tool name, generic arguments, and expandable result", async () => {
  app = await renderApp({ width: 100, height: 30, loadExtensions: true, seed: seed() })
  await app.waitForText("task")
  await app.waitForText('"description":"map routes"')
  expect(app.find("two launches")).toBeNull()
  await app.controller.submit("/tools on", "command")
  await app.waitForText('"description": "map routes"')
  await app.waitForText("two launches")
  expect(app.find("click to view")).toBeNull()
})

test("a bundle renderer overrides fallback and each task row opens its own child", async () => {
  app = await renderApp({ width: 100, height: 36, loadExtensions: true, seed: seed(true) })
  for (const [child, label] of [["hysec_routes", "main/routes · map routes"], ["hysec_tests", "main/tests · map tests"]]) {
    await app.waitForText("hya-task · map routes · 2 members")
    await app.waitForText(label!)
    await app.waitForText(`click to view · /open ${child}`)
    await app.clickText(`click to view · /open ${child}`)
    await app.waitForText("Viewing child session")
    expect(app.store.state.selected?.id).toBe(child!)
    await app.press("ESCAPE")
    await app.waitForGone("Viewing child session")
  }
  await extensionManager.setEnabled("hya/multi-agent", false)
  await app.waitForGone("click to view")
  await app.waitForText('"description":"map routes"')
})
