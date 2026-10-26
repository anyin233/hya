import { expect, test } from "bun:test"
import { createProviderController } from "../src/app/providers"
import { HttpError, type HyaClient, type ModelSummary, type ProviderSummary } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import { createAppStore } from "../src/state/store"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})
const type = (text: string): KeyLike[] => [...text].map((char) => key(char, { sequence: char }))
const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

function harness(client: Partial<HyaClient>, catalog: { providers: ProviderSummary[]; models: ModelSummary[] }) {
  const store = createAppStore()
  store.applyBootstrap({ agents: [{ name: "hya-main", model: { providerId: "hya", modelId: "offline" } }], models: catalog.models })
  const picks: string[] = []
  let current = catalog
  const providers = createProviderController({
    store,
    client: client as HyaClient,
    refresh: async () => { store.setProviderCatalog(current.providers, current.models) },
    pickModel: (provider) => { picks.push(provider) },
  })
  return {
    store, picks, providers,
    setCatalog: (next: typeof catalog) => { current = next },
    press: async (keys: KeyLike[]) => { for (const pressed of keys) providers.key(pressed); await settle(); await settle() },
  }
}

const offline: ProviderSummary = { id: "hya", auth: "AUTH_STATUS_NOT_APPLICABLE", keySource: "none", modelCount: 1 }
const offlineModel: ModelSummary = { id: "hya/offline", providerId: "hya", modelId: "offline", source: "offline" }

test("the add-provider wizard sends the key only to the server, opens the new provider, and asks for a model on hya/offline", async () => {
  const sent: unknown[] = []
  const h = harness({
    upsertProvider: async (id: string, body: unknown) => {
      sent.push({ id, body })
      h.setCatalog({
        providers: [{ id: "gw", kind: "openai", baseUrl: "http://h/v1", keySource: "saved", auth: "AUTH_STATUS_CREDENTIALED", modelCount: 2 }, offline],
        models: [offlineModel, { id: "gw/alpha", providerId: "gw", modelId: "alpha", source: "remote" }, { id: "gw/beta", providerId: "gw", modelId: "beta", source: "remote" }],
      })
      return { discovery: { ok: true, result: "models", modelCount: 2 } }
    },
  }, { providers: [offline], models: [offlineModel] })
  h.providers.open()
  await settle()
  await h.press([key("a"), ...type("gw"), key("return"), key("return"), ...type("http://h/v1"), key("return"), ...type("sk-1")])
  h.providers.paste("23\n")
  expect(h.store.state.providerView?.form?.fields[3]?.masked).toBe(6)
  // The key is never in the store.
  expect(JSON.stringify(h.store.state.providerView)).not.toContain("sk-1")
  await h.press([key("return")])
  expect(sent).toEqual([{ id: "gw", body: { kind: "openai", baseUrl: "http://h/v1", apiKey: "sk-123" } }])
  const view = h.store.state.providerView!
  expect(view.form).toBeUndefined()
  expect(view).toMatchObject({ screen: "detail", provider: "gw", model: "gw/alpha" })
  expect(view.notice?.text).toContain("Added gw · 2 models fetched")
  expect(view.notice?.text).toContain("pick a model")
  expect(h.picks).toEqual(["gw"])
})

test("a server error keeps the wizard open on the step it names", async () => {
  const h = harness({
    upsertProvider: async () => { throw new HttpError(400, "PUT", "/v1/providers/gw", "invalid_argument: base URL must be http(s)://host") },
  }, { providers: [offline], models: [offlineModel] })
  h.providers.open()
  await settle()
  await h.press([key("a"), ...type("gw"), key("return"), key("return"), ...type("http://h"), key("return"), key("return")])
  const form = h.store.state.providerView?.form
  expect(form?.step).toBe(2)
  expect(form?.error).toBe("invalid_argument: base URL must be http(s)://host")
  expect(h.store.state.providerView?.busy).toBeUndefined()
  expect(h.picks).toEqual([])
})

test("a running test shows as busy and Esc cancels it; a finished test is kept for the detail screen", async () => {
  const gw: ProviderSummary = { id: "gw", kind: "openai", keySource: "saved", modelCount: 1 }
  const alpha: ModelSummary = { id: "gw/alpha", providerId: "gw", modelId: "alpha", source: "remote" }
  let calls = 0
  const h = harness({
    testProviderModel: (_provider: string, _model: string, signal?: AbortSignal) => {
      calls++
      if (calls === 2) return Promise.resolve({ ok: true, text: "Hi", finishReason: "length", latencyMs: 5 })
      return new Promise((_resolve, reject) => signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))))
    },
  }, { providers: [gw, offline], models: [offlineModel, alpha] })
  h.providers.open()
  await settle()
  await h.press([key("return"), key("t")])
  expect(h.store.state.providerView?.busy?.label).toBe("Testing gw/alpha")
  await h.press([key("escape")])
  expect(h.store.state.providerView?.busy).toBeUndefined()
  expect(h.store.state.providerView?.notice?.text).toBe("Test cancelled")
  expect(h.store.state.providerView?.screen).toBe("detail")
  await h.press([key("t")])
  expect(h.store.state.providerView?.test).toMatchObject({ provider: "gw", model: "alpha", ok: true, text: "Hi" })
})

test("editing a model sends only the fields changed; the reasoning switch maps to a boolean", async () => {
  const bodies: unknown[] = []
  const gw: ProviderSummary = { id: "gw", kind: "openai", keySource: "saved", modelCount: 1 }
  const alpha: ModelSummary = { id: "gw/alpha", providerId: "gw", modelId: "alpha", source: "remote", contextLimit: "64000" }
  const h = harness({
    setProviderModel: async (_provider: string, body: unknown) => { bodies.push(body); return {} },
  }, { providers: [gw, offline], models: [offlineModel, alpha] })
  h.providers.open()
  await settle()
  await h.press([key("return"), key("e"), ...type("Alpha"), key("return"), key("return"), ...type("4096"), key("return"), key("down"), key("down"), key("return")])
  // The context limit the form opened with (64000, shown by the server) was not touched: not sent.
  expect(bodies).toEqual([{ modelId: "alpha", displayName: "Alpha", outputLimit: 4096, reasoning: false }])
  expect(h.store.state.providerView?.notice?.text).toBe("Saved alpha to config.yaml")
  // An edit that changes nothing sends nothing.
  await h.press([key("e"), key("return"), key("return"), key("return"), key("return")])
  expect(bodies).toHaveLength(1)
  expect(h.store.state.providerView?.notice?.text).toBe("No changes to alpha")
})

test("closing the view drops it and aborts a running call", async () => {
  let aborted = false
  const gw: ProviderSummary = { id: "gw", kind: "openai", keySource: "saved", modelCount: 0 }
  const h = harness({
    refreshProvider: (_provider: string, signal?: AbortSignal) => new Promise((_resolve, reject) => signal?.addEventListener("abort", () => { aborted = true; reject(new Error("aborted")) })),
  }, { providers: [gw, offline], models: [offlineModel] })
  h.providers.open()
  await settle()
  await h.press([key("r")])
  expect(h.store.state.providerView?.busy?.kind).toBe("refresh")
  h.providers.close()
  await settle()
  expect(aborted).toBe(true)
  expect(h.store.state.providerView).toBeUndefined()
})

test("Continue with ChatGPT completes the existing OAuth API without putting callback credentials in view state", async () => {
  const calls: { path: string; body: unknown }[] = []
  const h = harness({ request: async (_method: string, path: string, body: unknown) => {
    calls.push({ path, body })
    if (path.endsWith("/start")) return { authorizationUrl: "https://auth.openai.com/api/accounts/authorize?state=pending", state: "pending" } as never
    h.setCatalog({ providers: [offline, { id: "chatgpt", kind: "openai-chatgpt", keySource: "oauth", modelCount: 1 }],
      models: [offlineModel, { id: "chatgpt/account-model", providerId: "chatgpt", modelId: "account-model", source: "remote" }] })
    return { status: "AUTH_STATUS_CREDENTIALED" } as never
  } }, { providers: [offline], models: [offlineModel] })
  h.providers.open("chatgpt")
  await settle()
  await settle()
  expect(h.store.state.providerView?.form?.kind).toBe("chatgpt")
  expect(h.store.state.providerView?.oauth?.authorizationUrl).toContain("auth.openai.com")
  const callback = "http://127.0.0.1:1455/auth/callback?code=private-code&state=pending&client_id=issued"
  h.providers.paste(callback)
  expect(JSON.stringify(h.store.state.providerView)).not.toContain("private-code")
  await h.press([key("return")])
  expect(calls[1]).toEqual({ path: "/v1/auth/chatgpt/oauth/callback", body: { state: "pending", code: callback } })
  expect(h.store.state.providerView?.form).toBeUndefined()
  expect(h.store.state.providerView?.notice?.text).toContain("Connected chatgpt")
  expect(h.store.state.providerView?.screen).toBe("detail")
  expect(h.picks).toEqual(["chatgpt"])
  expect(h.store.state.providers.find((p) => p.id === "chatgpt")?.keySource).toBe("oauth")
})

test("/key login deepseek asks only for its key and writes discovered connection defaults", async () => {
  const sent: unknown[] = []
  const h = harness({ upsertProvider: async (id, body) => { sent.push({ id, body }); return {} } }, { providers: [offline], models: [offlineModel] })
  h.providers.open("deepseek")
  await settle()
  await settle()
  expect(h.store.state.providerView?.form).toMatchObject({ kind: "addProvider", step: 3 })
  h.providers.paste("test-private-key")
  expect(JSON.stringify(h.store.state.providerView)).not.toContain("test-private-key")
  await h.press([key("return")])
  expect(sent).toEqual([{ id: "deepseek", body: { kind: "openai", baseUrl: "https://api.deepseek.com", apiKey: "test-private-key" } }])
})

test("login to a configured provider uses key replacement and never upserts its custom route", async () => {
  const sent: unknown[] = []
  const h = harness({ setProviderKey: async (provider, key) => { sent.push({ provider, key }); return {} } },
    { providers: [offline, { id: "deepseek", kind: "openai-response", baseUrl: "http://custom.invalid/v1" }], models: [offlineModel] })
  h.providers.open("deepseek")
  await settle()
  await settle()
  expect(h.store.state.providerView?.form?.kind).toBe("setKey")
  h.providers.paste("replacement")
  await h.press([key("return")])
  expect(sent).toEqual([{ provider: "deepseek", key: "replacement" }])
})

test("closing provider login while its catalog is loading does not reopen a stale login", async () => {
  const store = createAppStore()
  let release!: () => void
  let requests = 0
  const controller = createProviderController({ store, client: { request: async () => { requests++; return {} } } as unknown as HyaClient,
    refresh: () => new Promise<void>((resolve) => { release = resolve }), pickModel: () => undefined })
  controller.open("chatgpt")
  controller.close()
  release()
  await settle()
  expect(store.state.providerView).toBeUndefined()
  expect(requests).toBe(0)
})
