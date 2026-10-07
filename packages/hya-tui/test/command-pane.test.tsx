import { expect, test } from "bun:test"
import { testRender } from "@opentui/solid"
import { App } from "../src/app/App"
import { AppContext } from "../src/app/context"
import { createController } from "../src/app/controller"
import { HyaClient } from "../src/client"
import { createAppStore } from "../src/state/store"

test("Backspace deleting the final command character closes the command pane", async () => {
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
    await setup.mockInput.typeText("/h")
    await setup.renderOnce()
    expect(controller.ui.command?.active()).toBe(true)
    setup.mockInput.pressBackspace()
    await setup.renderOnce()
    expect(controller.ui.command?.active()).toBe(true)
    expect(controller.ui.commandInput?.text).toBe("/")
    setup.mockInput.pressKey("HOME")
    setup.mockInput.pressBackspace()
    await setup.renderOnce()
    expect(controller.ui.command?.active()).toBe(true)
    expect(controller.ui.commandInput?.text).toBe("/")
    setup.mockInput.pressKey("END")
    setup.mockInput.pressBackspace()
    await setup.renderOnce()
    expect(controller.ui.command?.active()).toBe(false)
    expect(controller.ui.commandInput?.text).toBe("")
    await setup.mockInput.typeText("message")
    await setup.renderOnce()
    expect(controller.ui.composerInput?.text).toBe("message")
    expect(controller.ui.commandInput?.text).toBe("")
    store.setProjectsSidebar("open")
    store.setProjectsFocus(true)
    controller.ui.command?.open()
    await setup.renderOnce()
    expect(controller.ui.commandInput?.text).toBe("/")
    expect(store.state.projectsFocus).toBe(true) // Overlay preserves the underlying workspace focus.
    setup.mockInput.pressBackspace()
    await setup.renderOnce()
    expect(controller.ui.command?.active()).toBe(false)
    expect(store.state.projectsFocus).toBe(true)
    expect(controller.ui.composerInput?.text).toBe("message")
  } finally {
    setup.renderer.destroy()
  }
})
