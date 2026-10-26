/** Provider login metadata shared by command completion and setup selection. */
import type { ProviderSummary } from "../client"
import { addProviderForm, offlineProviderId, setKeyForm, validateProviderId, type FormState } from "./providers"

export interface ProviderLoginPreset {
  id: string
  kind: string
  baseUrl: string
  auth: "chatgpt" | "apiKey"
}

/** Presets provide connection defaults; models always come from discovery. */
export const providerLoginPresets: readonly ProviderLoginPreset[] = [
  { id: "chatgpt", kind: "openai-chatgpt", baseUrl: "https://api.openai.com/v1", auth: "chatgpt" },
  { id: "deepseek", kind: "openai", baseUrl: "https://api.deepseek.com", auth: "apiKey" },
  { id: "openai", kind: "openai-response", baseUrl: "https://api.openai.com/v1", auth: "apiKey" },
]

export function providerLoginNames(configured: readonly string[] = []): string[] {
  return [...new Set([...providerLoginPresets.map((preset) => preset.id), ...configured.filter((id) => id !== offlineProviderId)])]
}

export type ProviderLogin = { kind: "oauth"; provider: string } | { kind: "form"; form: FormState }

/** Existing declarations take precedence over presets: login never resets a custom URL or protocol. */
export function providerLogin(provider: string, configured: readonly ProviderSummary[]): ProviderLogin {
  const invalid = validateProviderId(provider, [])
  if (invalid) throw new Error(invalid)
  const existing = configured.find((row) => row.id === provider)
  if (existing && ["openai-codex", "grok-build"].includes(existing.kind ?? "")) {
    throw new Error(`Use hya oauth login --provider ${provider} --type ${existing.kind} for this provider's CLI login flow`)
  }
  if (existing) return existing.kind === "openai-chatgpt"
    ? { kind: "oauth", provider }
    : { kind: "form", form: setKeyForm(provider) }
  const preset = providerLoginPresets.find((row) => row.id === provider)
  if (preset?.auth === "chatgpt") return { kind: "oauth", provider }
  const form = addProviderForm(configured.map((row) => row.id))
  form.title = `Login · ${provider}`
  form.fields[0]!.value = provider
  if (preset) {
    form.fields[1]!.value = preset.kind
    form.fields[2]!.value = preset.baseUrl
    form.fields[3]!.optional = false
    form.fields[3]!.placeholder = "hidden · paste or type"
    form.step = 3
  } else form.step = 1
  return { kind: "form", form }
}
