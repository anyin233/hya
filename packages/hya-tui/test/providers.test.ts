import { expect, test } from "bun:test"
import { HttpError, type ModelSummary, type ProviderSummary } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  addModelForm,
  addProviderForm,
  authText,
  defaultModelRef,
  discoveryNotice,
  editModelForm,
  errorText,
  formKey,
  formPaste,
  initialProviderView,
  keySourceText,
  modelPatch,
  modelLine,
  providerDetailHeader,
  providerKeyRows,
  providerLine,
  providerModels,
  providerViewHint,
  providerViewKey,
  shownProviders,
  testResultText,
  tokenCount,
  validateBaseUrl,
  validateProviderId,
  withSecretLength,
  type FormState,
  type ProviderViewState,
} from "../src/state/providers"

const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra,
})
const type = (text: string): KeyLike[] => [...text].map((char) => key(char, { sequence: char }))

const providers: ProviderSummary[] = [
  { id: "hya", auth: "AUTH_STATUS_NOT_APPLICABLE", keySource: "none", modelCount: 1 },
  { id: "gw", kind: "openai", baseUrl: "https://gw.example/v1", auth: "AUTH_STATUS_CREDENTIALED", keySource: "saved", modelCount: 2 },
  { id: "local", kind: "anthropic", baseUrl: "http://127.0.0.1:9/v1", auth: "AUTH_STATUS_UNAUTHENTICATED", keySource: "none", modelCount: 0 },
]
const models: ModelSummary[] = [
  { id: "hya/offline", providerId: "hya", modelId: "offline", source: "offline" },
  { id: "gw/alpha", providerId: "gw", modelId: "alpha", displayName: "Alpha", contextLimit: "64000", outputLimit: "4096", reasoning: true, source: "override" },
  { id: "gw/beta", providerId: "gw", modelId: "beta", source: "remote" },
]
const data = { providers, models }

/** Feed keys through the view machine; returns the final view and every non-update outcome. */
function press(view: ProviderViewState, keys: KeyLike[]) {
  const outcomes: ReturnType<typeof providerViewKey>[] = []
  let current = view
  for (const pressed of keys) {
    const outcome = providerViewKey(current, pressed, data)
    if (outcome.type === "update") current = outcome.view
    else outcomes.push(outcome)
  }
  return { view: current, outcomes }
}

/** Feed keys through a form; returns the final form and the last non-update outcome. */
function fill(form: FormState, keys: KeyLike[]) {
  let current = form
  let last: ReturnType<typeof formKey> | undefined
  for (const pressed of keys) {
    const outcome = formKey(current, pressed)
    if (outcome.type === "update") current = outcome.form
    else last = outcome
  }
  return { form: current, last }
}

test("the view opens on the provider list, highlighting the first configured provider", () => {
  const view = initialProviderView(providers)
  expect(view.screen).toBe("list")
  expect(view.provider).toBe("gw")
  expect(initialProviderView([]).provider).toBeUndefined()
})

test("Up/Down move over providers, Enter opens the detail, Esc goes back, then closes", () => {
  let { view } = press(initialProviderView(providers), [key("down")])
  expect(view.provider).toBe("local")
  view = press(view, [key("down")]).view
  expect(view.provider).toBe("hya")
  view = press(view, [key("up"), key("up"), key("return")]).view
  expect(view).toMatchObject({ screen: "detail", provider: "gw", model: "gw/alpha" })
  view = press(view, [key("down")]).view
  expect(view.model).toBe("gw/beta")
  const back = press(view, [key("escape")])
  expect(back.view.screen).toBe("list")
  expect(press(back.view, [key("escape")]).outcomes).toEqual([{ type: "close" }])
})

test("/ filters the list; Esc clears the filter before it closes the view", () => {
  const { view } = press(initialProviderView(providers), [key("/"), ...type("loc")])
  expect(view.filtering).toBe(true)
  expect(shownProviders(view, providers).map((row) => row.id)).toEqual(["local"])
  expect(view.provider).toBe("local")
  const kept = press(view, [key("return")]).view
  expect(kept).toMatchObject({ filtering: false, filter: "loc" })
  const cleared = press(kept, [key("escape")])
  expect(cleared.outcomes).toEqual([])
  expect(cleared.view.filter).toBe("")
})

test("a opens the add-provider wizard; k the key form; x asks before removing a saved key", () => {
  const start = initialProviderView(providers)
  expect(press(start, [key("a")]).view.form?.kind).toBe("addProvider")
  const keyForm = press(start, [key("k")]).view.form
  expect(keyForm).toMatchObject({ kind: "setKey", provider: "gw" })
  const confirm = press(start, [key("x")]).view.form
  expect(confirm).toMatchObject({ kind: "confirm", action: "removeKey", provider: "gw" })
  expect(confirm?.confirmText).toContain("gw")
  // `local` has no saved key: nothing to remove.
  const none = press(start, [key("down"), key("x")]).view
  expect(none.form).toBeUndefined()
  expect(none.notice?.text).toContain("no saved key")
  // The offline provider takes no key.
  const offline = press(start, [key("up"), key("k")]).view
  expect(offline.form).toBeUndefined()
  expect(offline.notice?.text).toContain("offline")
})

test("r refreshes, t tests the highlighted model, m/e open model forms, d asks before deleting an override", () => {
  const start = initialProviderView(providers)
  expect(press(start, [key("r")]).outcomes).toEqual([{ type: "command", command: { kind: "refresh", provider: "gw" } }])
  const detail = press(start, [key("return")]).view
  expect(press(detail, [key("t")]).outcomes).toEqual([{ type: "command", command: { kind: "test", provider: "gw", model: "alpha" } }])
  expect(press(detail, [key("m")]).view.form).toMatchObject({ kind: "addModel", provider: "gw" })
  const edit = press(detail, [key("e")]).view.form!
  expect(edit).toMatchObject({ kind: "editModel", provider: "gw", model: "alpha" })
  expect(edit.fields.map((field) => [field.id, field.value])).toEqual([
    ["displayName", "Alpha"], ["contextLimit", "64000"], ["outputLimit", "4096"], ["reasoning", "on"],
  ])
  expect(press(detail, [key("d")]).view.form).toMatchObject({ kind: "confirm", action: "deleteModel", model: "alpha" })
  // beta comes only from the remote list: no config entry to delete.
  const remote = press(detail, [key("down"), key("d")]).view
  expect(remote.form).toBeUndefined()
  expect(remote.notice?.text).toContain("remote")
})

test("while a call runs, Esc cancels it and other actions wait", () => {
  const busy: ProviderViewState = { ...initialProviderView(providers), busy: { kind: "test", label: "Testing gw/alpha", startedAt: 0, provider: "gw" } }
  expect(press(busy, [key("escape")]).outcomes).toEqual([{ type: "cancelBusy" }])
  const blocked = press(busy, [key("r")])
  expect(blocked.outcomes).toEqual([])
  expect(blocked.view.notice?.text).toContain("Esc cancels")
  // Moving still works.
  expect(press(busy, [key("down")]).view.provider).toBe("local")
})

test("the add-provider wizard asks name, protocol, base URL, then key, validating each step", () => {
  let form = addProviderForm(["gw", "hya"])
  expect(form.fields.map((field) => field.id)).toEqual(["name", "protocol", "baseUrl", "key"])
  // Invalid ids stay on the step with an error.
  let step = fill(form, [...type("bad id"), key("return")])
  expect(step.form.step).toBe(0)
  expect(step.form.error).toContain("letters, digits")
  step = fill(form, [...type("gw"), key("return")])
  expect(step.form.error).toContain("already exists")
  step = fill(form, [...type("hya"), key("return")])
  expect(step.form.error).toContain("reserved")
  form = fill(form, [...type("newgw"), key("return")]).form
  expect(form.step).toBe(1)
  expect(form.error).toBeUndefined()
  // Protocol: a choice; Down twice → anthropic.
  form = fill(form, [key("down"), key("down"), key("return")]).form
  expect(form.fields[1]?.value).toBe("anthropic")
  expect(form.step).toBe(2)
  step = fill(form, [...type("ftp://x"), key("return")])
  expect(step.form.error).toContain("http")
  form = fill(step.form, [...Array(7).fill(key("backspace")), ...type("http://127.0.0.1:9/v1"), key("return")]).form
  expect(form.step).toBe(3)
  // Key step: characters go to the controller's secret entry, never into the form.
  const secret = formKey(form, key("s", { sequence: "s" }))
  expect(secret).toEqual({ type: "secret", op: "append", text: "s" })
  expect(formKey(form, key("backspace"))).toEqual({ type: "secret", op: "backspace" })
  form = withSecretLength(form, 3)
  expect(form.fields[3]).toMatchObject({ value: "", masked: 3 })
  const done = formKey(form, key("return"))
  expect(done).toEqual({ type: "submit", form, values: { name: "newgw", protocol: "anthropic", baseUrl: "http://127.0.0.1:9/v1", key: "" } })
})

test("Alt+Enter (a fast Esc then Enter) never submits or advances a form", () => {
  const form = fill(addProviderForm([]), [...type("gw")]).form
  expect(formKey(form, key("return", { meta: true }))).toEqual({ type: "none" })
})

test("Esc cancels the wizard at any step", () => {
  let form = addProviderForm([])
  for (const keys of [[], [...type("x"), key("return")], [key("return")], [...type("http://h"), key("return")]]) {
    form = fill(form, keys).form
    expect(formKey(form, key("escape"))).toEqual({ type: "cancel" })
  }
  expect(form.step).toBe(3)
})

test("pasting fills a text step (line breaks dropped) and goes to the secret entry on a key step", () => {
  const form = addProviderForm([])
  expect(formPaste(form, "my-gw\n")).toEqual({ type: "update", form: { ...form, fields: form.fields.map((field, index) => index === 0 ? { ...field, value: "my-gw" } : field) } })
  const keyStep = { ...form, step: 3 }
  expect(formPaste(keyStep, "sk-1\r\n")).toEqual({ type: "secret", op: "append", text: "sk-1\r\n" })
})

test("the model form takes an id, display name, limits (digits only), and a reasoning switch", () => {
  let form = addModelForm("gw")
  expect(form.fields.map((field) => field.id)).toEqual(["modelId", "displayName", "contextLimit", "outputLimit", "reasoning"])
  expect(fill(form, [key("return")]).form.error).toContain("required")
  form = fill(form, [...type("vendor/m:1"), key("return"), ...type("M one"), key("return"), ...type("12a8000"), key("return")]).form
  expect(form.fields[2]?.value).toBe("128000")
  form = fill(form, [...type("200000"), key("return")]).form
  expect(form.error).toContain("output limit")
  form = fill(form, [...Array(6).fill(key("backspace")), ...type("8000"), key("return")]).form
  const done = fill(form, [key("down"), key("return")])
  expect(done.last).toMatchObject({ type: "submit", values: { modelId: "vendor/m:1", displayName: "M one", contextLimit: "128000", outputLimit: "8000", reasoning: "on" } })
  // Limits are uint32 on the wire.
  const huge = fill(addModelForm("gw"), [...type("m"), key("return"), key("return"), ...type("5000000000"), key("return")])
  expect(huge.form.step).toBe(2)
  expect(huge.form.error).toBe("Context limit is at most 4294967295 tokens")
  const edit = editModelForm("gw", models[2]!)
  expect(edit.fields.map((field) => field.id)).toEqual(["displayName", "contextLimit", "outputLimit", "reasoning"])
  expect(edit.fields[3]?.value).toBe("default")
})

test("id and base URL validation match the server's rules", () => {
  expect(validateProviderId("gw_1-x", [])).toBeUndefined()
  expect(validateProviderId("", [])).toContain("letters, digits")
  expect(validateProviderId("a".repeat(65), [])).toContain("64")
  expect(validateProviderId("hya", [])).toContain("reserved")
  expect(validateBaseUrl("https://api.example.com/v1")).toBeUndefined()
  expect(validateBaseUrl("http://127.0.0.1:8080")).toBeUndefined()
  expect(validateBaseUrl("api.example.com")).toContain("http")
  expect(validateBaseUrl("https://user:pw@example.com")).toContain("credentials")
})

test("formats provider and model rows for about 80 columns", () => {
  expect(keySourceText("saved")).toBe("saved key")
  expect(keySourceText("oauth")).toBe("oauth")
  expect(keySourceText("config")).toBe("config key")
  expect(keySourceText(undefined)).toBe("no key")
  expect(authText("AUTH_STATUS_CREDENTIALED")).toBe("ready")
  expect(authText("AUTH_STATUS_AUTH_REJECTED")).toBe("key rejected")
  expect(authText("AUTH_STATUS_NOT_APPLICABLE")).toBe("offline")
  expect(tokenCount("64000")).toBe("64k")
  expect(tokenCount("4096")).toBe("4.1k")
  expect(tokenCount("1000000")).toBe("1M")
  expect(tokenCount("0")).toBe("—")
  expect(tokenCount(undefined)).toBe("—")
  const line = providerLine(providers[1]!, 76)
  expect(line).toContain("gw")
  expect(line).toContain("openai")
  expect(line).toContain("saved key")
  expect(line).toContain("ready")
  expect(line).toContain("2 models")
  expect(Bun.stringWidth(line)).toBeLessThanOrEqual(76)
  const model = modelLine(models[1]!, 76)
  expect(model).toContain("alpha")
  expect(model).toContain("Alpha")
  expect(model).toContain("override")
  expect(model).toContain("64k / 4.1k")
  expect(model).toContain("reasoning")
  expect(Bun.stringWidth(model)).toBeLessThanOrEqual(76)
  expect(Bun.stringWidth(modelLine({ id: "gw/" + "x".repeat(90), providerId: "gw", modelId: "x".repeat(90), source: "remote" }, 76))).toBeLessThanOrEqual(76)
  expect(providerDetailHeader(providers[1]!)).toBe("gw · openai · https://gw.example/v1 · saved key · ready · 2 models")
  expect(providerModels(models, "gw", "").map((row) => row.id)).toEqual(["gw/alpha", "gw/beta"])
  expect(providerModels(models, "gw", "bet").map((row) => row.id)).toEqual(["gw/beta"])
})

test("discovery and test results read as one line each", () => {
  expect(discoveryNotice("gw", { ok: true, result: "models", modelCount: 2 })).toEqual({ tone: "ok", text: "gw: 2 models fetched" })
  expect(discoveryNotice("gw", { ok: true, result: "empty" })).toEqual({ tone: "info", text: "gw: the remote model list is empty · m adds a model by hand" })
  expect(discoveryNotice("gw", { result: "unavailable", errorMessage: "connection refused" })).toEqual({ tone: "error", text: "gw: model fetch failed (unavailable): connection refused" })
  expect(discoveryNotice("gw", undefined)).toBeUndefined()
  expect(testResultText({ provider: "gw", model: "alpha", ok: true, text: "Hi", finishReason: "length", latencyMs: 412 }))
    .toBe('✓ gw/alpha replied · 412 ms · finish length · "Hi"')
  expect(testResultText({ provider: "gw", model: "alpha", errorCode: "http_401", errorMessage: "bad key", latencyMs: "90" }))
    .toBe("✗ gw/alpha failed · 90 ms · http_401: bad key")
})

test("the footer hint and the help rows come from one key table", () => {
  const list = providerViewHint(initialProviderView(providers))
  for (const text of ["Enter", "a add", "k key", "r refresh", "Esc"]) expect(list).toContain(text)
  const detail = providerViewHint({ ...initialProviderView(providers), screen: "detail" })
  for (const text of ["t test", "m add model", "e edit", "d delete", "Esc back"]) expect(detail).toContain(text)
  const keys = providerKeyRows.map((row) => row.keys)
  for (const label of ["a", "k", "x", "r", "t", "m", "e", "d", "/", "Esc"]) expect(keys).toContain(label)
})

test("the default model for the next turn: the session's, else the pending choice, the agent's, the first model", () => {
  const base = { selected: undefined, pendingModel: undefined, pendingAgent: undefined, agents: [{ name: "build", model: { providerId: "hya", modelId: "offline" } }], models }
  expect(defaultModelRef(base)).toBe("hya/offline")
  expect(defaultModelRef({ ...base, pendingModel: "gw/alpha" })).toBe("gw/alpha")
  expect(defaultModelRef({ ...base, agents: [] })).toBe("hya/offline")
  expect(defaultModelRef({ ...base, selected: { id: "s", agent: "build", workdir: "/w", model: { providerId: "gw", modelId: "beta" } } })).toBe("gw/beta")
})

test("server errors show without the method and path", () => {
  expect(errorText(new HttpError(400, "PUT", "/v1/providers/gw", "invalid_argument: base URL must be http(s)"))).toBe("invalid_argument: base URL must be http(s)")
  expect(errorText(new Error("boom"))).toBe("boom")
  expect(errorText("plain")).toBe("plain")
})

test("a model form sends only what the user changed (add: filled in); clearing a field removes it", () => {
  // Add: only filled fields.
  const added = fill(addModelForm("gw"), [...type("m1"), key("return"), key("return"), ...type("8000"), key("return"), key("return")])
  const submitted = formKey(added.form, key("return"))
  expect(submitted.type).toBe("submit")
  if (submitted.type !== "submit") return
  expect(modelPatch(submitted.form, submitted.values)).toEqual({ modelId: "m1", contextLimit: 8000 })
  // Edit a remote model shown with server defaults (200k context, reasoning on): only the new name is sent.
  const remote: ModelSummary = { id: "gw/r", providerId: "gw", modelId: "r", contextLimit: "200000", reasoning: true, source: "remote" }
  const renamed = fill(editModelForm("gw", remote), [...type("R"), key("return"), key("return"), key("return")])
  const done = formKey(renamed.form, key("return"))
  if (done.type !== "submit") throw new Error("not submitted")
  expect(modelPatch(done.form, done.values)).toEqual({ modelId: "r", displayName: "R" })
  // Clearing a field sends it empty / 0 (the server removes it); reasoning back to default is omitted.
  const cleared = fill(editModelForm("gw", models[1]!), [key("u", { ctrl: true }), key("return"), key("u", { ctrl: true }), key("return"), key("return"), key("1", { sequence: "1" }), key("return")])
  if (cleared.last?.type !== "submit") throw new Error("not submitted")
  expect(modelPatch(cleared.last.form, cleared.last.values)).toEqual({ modelId: "alpha", displayName: "", contextLimit: 0 })
  // Nothing changed: no call.
  const same = fill(editModelForm("gw", models[1]!), [key("return"), key("return"), key("return"), key("return")])
  if (same.last?.type !== "submit") throw new Error("not submitted")
  expect(modelPatch(same.last.form, same.last.values)).toBeUndefined()
})
