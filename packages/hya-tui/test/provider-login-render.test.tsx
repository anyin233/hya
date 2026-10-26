import { expect, test } from "bun:test"
import { TextRenderable, type BaseRenderable } from "@opentui/core"
import { testRender } from "@opentui/solid"
import { AppContext } from "../src/app/context"
import { createController } from "../src/app/controller"
import { HyaClient } from "../src/client"
import { ProviderView } from "../src/components/ProviderView"
import { initialProviderView } from "../src/state/providers"
import { createAppStore } from "../src/state/store"

function texts(node: BaseRenderable): TextRenderable[] {
  return [...(node instanceof TextRenderable ? [node] : []), ...node.getChildren().flatMap(texts)]
}

test("login URL keeps its full hyperlink target when wrapped and copy feedback appears inside the form", async () => {
  const url = `https://auth.openai.com/api/accounts/authorize?state=${"long-state".repeat(12)}&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback`
  const store = createAppStore()
  const client = new HyaClient("http://127.0.0.1:1", "/workspace")
  const controller = createController({ client, store, directory: "/workspace" })
  store.setProviderView({ ...initialProviderView([]), oauth: { provider: "chatgpt", authorizationUrl: url, state: "pending" },
    form: { kind: "chatgpt", title: "Continue with ChatGPT", step: 0, fields: [] },
    notice: { tone: "ok", text: "Login URL copied to clipboard" } })
  const setup = await testRender(() => <AppContext.Provider value={{ store, controller, server: client.baseUrl, ui: controller.ui }}>
    <ProviderView />
  </AppContext.Provider>, { width: 80, height: 40 })
  try {
    await setup.renderOnce()
    const chunks = texts(setup.renderer.root).flatMap((text) => text.textNode.toChunks()).filter((chunk) => chunk.link)
    expect(chunks.length).toBeGreaterThan(0)
    expect(chunks.every((chunk) => chunk.link?.url === url)).toBe(true)
    expect(setup.captureCharFrame()).toContain("F2 copy")
    expect(setup.captureCharFrame().match(/Login URL copied to clipboard/g)).toHaveLength(1)
  } finally { setup.renderer.destroy() }
})
