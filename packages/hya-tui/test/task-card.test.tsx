import { expect, mock, test } from "bun:test"
import { type TextRenderable, type Renderable } from "@opentui/core"
import { testRender } from "@opentui/solid"
import { createSignal } from "solid-js"
import { MessageItem } from "../src/components/MessageView"
import { AppContext } from "../src/app/context"
import { createController } from "../src/app/controller"
import { HyaClient } from "../src/client"
import { createAppStore } from "../src/state/store"
import type { MessageView } from "../src/state/messages"

function link(root: Renderable, child: string): TextRenderable | undefined {
  if ("plainText" in root && String(root.plainText).includes(`/open ${child}`)) return root as TextRenderable
  for (const node of root.getChildren()) {
    const found = link(node, child)
    if (found) return found
  }
}

test("live roster and transcript updates preserve clickable task rows", async () => {
  const store = createAppStore()
  const client = new HyaClient("http://127.0.0.1:1", "/workspace")
  const controller = createController({ client, store, directory: "/workspace" })
  const open = mock(async (_id: string) => {})
  controller.openSession = open
  const sessions = [
    { id: "hysec_routes", parent: "hysec_root", agent: "hya-task", workdir: "/workspace", title: "main/routes", busy: true },
    { id: "hysec_tests", parent: "hysec_root", agent: "hya-task", workdir: "/workspace", title: "main/tests", busy: true },
  ]
  store.setSessions(sessions)
  const initial: MessageView = { id: "message", role: "assistant", agent: "hya-main", model: "fake/model", streaming: false, queued: false, blocks: [
    { id: "part", kind: "tool", callId: "call_batch", card: { tool: "task", status: "done", summary: "two inspections", body: [], task: {
      agent: "hya-task", description: "", members: [
        { agent: "hya-task", description: "map routes", child: "hysec_routes" },
        { agent: "hya-task", description: "map tests", child: "hysec_tests" },
      ],
    } } },
  ] }
  const [view, setView] = createSignal(initial)
  const setup = await testRender(() => <AppContext.Provider value={{ store, controller, server: client.baseUrl, ui: controller.ui }}>
    <MessageItem view={view()} first />
  </AppContext.Provider>, { width: 80, height: 24 })
  try {
    for (const [index, child] of ["hysec_routes", "hysec_tests"].entries()) {
      await setup.renderOnce()
      const drawn = link(setup.renderer.root, child)!
      expect(drawn).toBeDefined()
      // Session list updates and projection reads replace link data while the UI is live.
      store.setSessions(sessions.map((row, at) => ({ ...row, busy: at === index ? false : row.busy })))
      setView(structuredClone(initial))
      await setup.renderOnce()
      expect(drawn.isDestroyed).toBe(false)
      expect(link(setup.renderer.root, child)?.num).toBe(drawn.num)
      await setup.mockMouse.click(drawn.x + 7, drawn.y)
      expect(open).toHaveBeenLastCalledWith(child)
      await setup.renderOnce()
      expect(setup.captureCharFrame()).toContain("idle")
    }
  } finally { setup.renderer.destroy() }
})
