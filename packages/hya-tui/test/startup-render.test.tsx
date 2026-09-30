import { expect, test } from "bun:test"
import { testRender } from "@opentui/solid"
import { App } from "../src/app/App"
import { AppContext } from "../src/app/context"
import { createController } from "../src/app/controller"
import { HyaClient } from "../src/client"
import { createAppStore } from "../src/state/store"

test("initial workspace renders before any backend data arrives", async () => {
  const store = createAppStore()
  const client = new HyaClient("http://127.0.0.1:1", "/workspace")
  const controller = createController({ client, store, directory: "/workspace" })
  const setup = await testRender(() => (
    <AppContext.Provider value={{ store, controller, server: client.baseUrl, ui: controller.ui }}>
      <App />
    </AppContext.Provider>
  ), { width: 156, height: 41 })
  try {
    await setup.renderOnce()
    expect(controller.ui.transcript).toBeDefined()
    expect(controller.ui.command?.active()).toBe(false)
  } finally {
    setup.renderer.destroy()
  }
})
