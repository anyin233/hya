import { expect, test } from "bun:test"
import { customSetup, deepseekSetup } from "../src/provider_setup"

test("DeepSeek preset uses the official Chat Completions origin and saved key id", () => {
  expect(deepseekSetup()).toEqual({
    providerId: "deepseek", kind: "openai-compatible", baseUrl: "https://api.deepseek.com",
    modelIds: ["deepseek-flash", "deepseek-v4-pro"], makeDefault: true,
  })
})

test("custom routes accept one or more model ids and reject credential-bearing URLs", () => {
  expect(customSetup(["local", "http://127.0.0.1:8000/v1/", "alpha", "beta"]).modelIds).toEqual(["alpha", "beta"])
  expect(() => customSetup(["local", "https://user:pass@example.com/v1", "alpha"])).toThrow()
})
