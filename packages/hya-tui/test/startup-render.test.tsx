import { expect, test } from "bun:test"
import { testRender } from "@opentui/solid"
import { App } from "../src/app/App"
import { AppContext } from "../src/app/context"
import { createController } from "../src/app/controller"
import { HyaClient } from "../src/client"
import { createAppStore } from "../src/state/store"

async function firstFrame(width: number): Promise<string> {
  const store = createAppStore()
  const client = new HyaClient("http://127.0.0.1:1", "/workspace")
  const controller = createController({ client, store, directory: "/workspace" })
  const setup = await testRender(() => (
    <AppContext.Provider value={{ store, controller, server: client.baseUrl, ui: controller.ui }}>
      <App />
    </AppContext.Provider>
  ), { width, height: 41 })
  try {
    await setup.renderOnce()
    expect(controller.ui.transcript).toBeDefined()
    expect(controller.ui.command?.active()).toBe(false)
    return setup.captureCharFrame()
  } finally {
    setup.renderer.destroy()
  }
}

test("initial workspace renders before any backend data arrives", async () => {
  // Wide: the Context pane is framed and loading (hya/basic-tui-components draws it once it runs); no top status line.
  const wide = await firstFrame(156)
  expect(wide).toContain("─Context─")
  expect(wide).toContain("Loading…")
  expect(wide).not.toContain("mode manual")
  // Narrow: no sidebar and no restored metadata heading.
  const narrow = await firstFrame(90)
  expect(narrow).not.toContain("mode manual · connecting…")
  expect(narrow).not.toContain("Context")
})
