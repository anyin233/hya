import { expect, test } from "bun:test"
import { providerLogin, providerLoginNames } from "../src/state/providerLogin"

test("login metadata chooses ChatGPT OAuth, prefilled API keys, or a named custom setup", () => {
  expect(providerLogin("chatgpt", [])).toEqual({ kind: "oauth", provider: "chatgpt" })
  const deepseek = providerLogin("deepseek", [])
  expect(deepseek.kind).toBe("form")
  if (deepseek.kind !== "form") throw new Error("expected API form")
  expect(deepseek.form.step).toBe(3)
  expect(deepseek.form.fields.map((field) => field.value)).toEqual(["deepseek", "openai", "https://api.deepseek.com", ""])
  expect(deepseek.form.fields[3]).toMatchObject({ kind: "secret", optional: false })
  const custom = providerLogin("my-server", [])
  expect(custom.kind).toBe("form")
  if (custom.kind !== "form") throw new Error("expected setup form")
  expect(custom.form.step).toBe(1)
  expect(custom.form.fields[0]?.value).toBe("my-server")
})

test("configured declarations take priority over presets, including named OAuth profiles", () => {
  const login = providerLogin("deepseek", [{ id: "deepseek", kind: "openai-response", baseUrl: "https://my-gateway.invalid" }])
  expect(login).toMatchObject({ kind: "form", form: { kind: "setKey", provider: "deepseek" } })
  expect(providerLogin("work", [{ id: "work", kind: "openai-chatgpt" }])).toEqual({ kind: "oauth", provider: "work" })
  expect(providerLoginNames(["chatgpt", "work", "hya"])).toEqual(["chatgpt", "deepseek", "openai", "work"])
  expect(() => providerLogin("hya", [])).toThrow("reserved")
  expect(() => providerLogin("../bad", [])).toThrow("letters")
  expect(() => providerLogin("codex", [{ id: "codex", kind: "openai-codex" }])).toThrow("hya oauth login")
})
