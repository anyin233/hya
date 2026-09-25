import type { ConfigureProviderRequest } from "./client"

/** Official DeepSeek Chat Completions route; keys remain in backend auth storage. */
export function deepseekSetup(): ConfigureProviderRequest {
  return {
    providerId: "deepseek",
    kind: "openai-compatible",
    baseUrl: "https://api.deepseek.com",
    modelIds: ["deepseek-flash", "deepseek-v4-pro"],
    makeDefault: true,
  }
}

export function customSetup(args: string[]): ConfigureProviderRequest {
  if (args.length < 3) throw new Error("Usage: /connect custom <provider> <base-url> <model-id> [more-model-ids]")
  const [providerId = "", baseUrl = "", ...modelIds] = args
  if (!/^[A-Za-z0-9_-]+$/.test(providerId)) throw new Error("Invalid provider ID")
  const url = new URL(baseUrl)
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("Base URL must be HTTP(S) without credentials, query, or fragment")
  }
  if (modelIds.some((id) => !id || /\s/.test(id))) throw new Error("Invalid model ID")
  return { providerId, kind: "openai-compatible", baseUrl: baseUrl.replace(/\/+$/, ""), modelIds, makeDefault: true }
}
