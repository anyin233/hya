/**
 * The Provider View (`/key`, docs/tui.md "Provider View"): pure state, keys,
 * pop-up forms, and row text. components/ProviderView.tsx renders it; the
 * controller (app/providers.ts) owns the calls.
 *
 * Screens: `list` (every provider: id, protocol, key source, auth status,
 * model count) and `detail` (one provider's models: id, display name,
 * source, context / output limits, reasoning). Highlights are kept by id, so
 * a catalog refresh never moves them to another row.
 *
 * Pop-ups (`FormState`): the add-provider wizard (name → protocol → base URL
 * → key), the key form, the add / edit model forms, and a one-line confirm.
 * A form asks one field at a time; Enter validates the field and moves on
 * (the last one submits), Esc cancels at any step. A `secret` field never
 * holds its text: its keys come back as `{type: "secret"}` outcomes for the
 * controller's `SecretEntry`, and the form keeps only the mask length.
 *
 * Keys (`providerKeyRows`, also shown in the help overlay): Up/Down move,
 * Enter opens, `a` add, `k` key, `x` remove key, `r` refresh, `t` test,
 * `m` add model, `e` edit model, `d` delete override, `/` filter, Esc back /
 * close. While a call runs (`busy`) Esc cancels it; other actions wait.
 */
import { HttpError, type AgentSummary, type DiscoveryOutcome, type ModelSummary, type ProviderModelPatch, type ProviderSummary, type SessionInfo } from "../client"
import type { KeyLike } from "../keys/bindings"
import { modelReference, truncate } from "./format"

/** The built-in offline provider: no key, no remote list, no config entry. */
export const offlineProviderId = "hya"

/** Protocols offered by the add-provider wizard (`UpsertProvider.kind`). */
export const providerProtocols: readonly ChoiceOption[] = [
  { id: "openai", label: "openai", detail: "OpenAI-compatible Chat Completions" },
  { id: "openai-response", label: "openai-response", detail: "OpenAI Responses API" },
  { id: "anthropic", label: "anthropic", detail: "Anthropic Messages API" },
  { id: "google", label: "google", detail: "Google Gemini API" },
]

const reasoningOptions: readonly ChoiceOption[] = [
  { id: "default", label: "default", detail: "keep what the provider reports" },
  { id: "on", label: "on", detail: "the model takes reasoning effort variants" },
  { id: "off", label: "off", detail: "no reasoning variants" },
]

export interface ChoiceOption {
  id: string
  label: string
  detail?: string
}

export type FieldKind = "text" | "secret" | "choice" | "number"

export interface FormField {
  id: string
  label: string
  kind: FieldKind
  /** Typed text, or the chosen option id; always `""` for a `secret` field. */
  value: string
  /** `secret` fields: characters held by the controller's `SecretEntry` (shown as bullets). */
  masked?: number
  options?: readonly ChoiceOption[]
  /** May be left empty. */
  optional?: boolean
  /** Muted text shown while the field is empty. */
  placeholder?: string
  /** The value the form opened with (model forms): a field still equal to it is unchanged and never sent (`modelPatch`). */
  initial?: string
}

export type FormKind = "addProvider" | "setKey" | "addModel" | "editModel" | "confirm"

export interface FormState {
  kind: FormKind
  title: string
  fields: FormField[]
  /** Index of the field being asked. */
  step: number
  /** Validation or server error for the current step. */
  error?: string
  /** Provider the form is about (every kind but `addProvider`). */
  provider?: string
  /** Provider-local model id (`editModel`, `confirm` of `deleteModel`). */
  model?: string
  /** `confirm`: what Enter does. */
  action?: "removeKey" | "deleteModel"
  /** `confirm`: the question. */
  confirmText?: string
  /** `addProvider`: ids that exist already. */
  existing?: readonly string[]
}

export type ProviderScreen = "list" | "detail"

/** A provider call in flight (Esc cancels it). */
export interface ProviderBusy {
  kind: "add" | "key" | "removeKey" | "refresh" | "test" | "model" | "deleteModel"
  /** `Testing gw/alpha`, `Refreshing gw`, … */
  label: string
  /** `Date.now()` when it started (the elapsed seconds). */
  startedAt: number
  provider: string
}

export interface ProviderNotice {
  text: string
  tone: "info" | "ok" | "error"
}

/** `TestProviderModel`'s answer with the model it was for. */
export interface ProviderTestResult {
  provider: string
  model: string
  ok?: boolean
  text?: string
  finishReason?: string
  errorCode?: string
  errorMessage?: string
  latencyMs?: number | string
}

export interface ProviderViewState {
  screen: ProviderScreen
  /** Highlighted provider id (list) / the open provider (detail). */
  provider: string | undefined
  /** Highlighted model row id (`provider/model`) on the detail screen. */
  model: string | undefined
  filter: string
  /** `/` was pressed: printable keys go to `filter`. */
  filtering: boolean
  form?: FormState
  busy?: ProviderBusy
  notice?: ProviderNotice
  /** The last model test's result (shown under the models). */
  test?: ProviderTestResult
}

export interface ProviderData {
  providers: readonly ProviderSummary[]
  models: readonly ModelSummary[]
}

export type ProviderCommand =
  | { kind: "refresh"; provider: string }
  | { kind: "test"; provider: string; model: string }

export type FormOutcome =
  | { type: "none" }
  | { type: "update"; form: FormState }
  | { type: "cancel" }
  | { type: "secret"; op: "append"; text: string }
  | { type: "secret"; op: "backspace" }
  | { type: "submit"; form: FormState; values: Record<string, string> }

export type ProviderViewOutcome =
  | { type: "none" }
  | { type: "update"; view: ProviderViewState }
  | { type: "close" }
  | { type: "cancelBusy" }
  | { type: "command"; command: ProviderCommand }
  | { type: "secret"; op: "append"; text: string }
  | { type: "secret"; op: "backspace" }
  | { type: "submit"; form: FormState; values: Record<string, string> }

/** One row of the view's key table: the footer hint and the help overlay (commands/help.ts) both read it. */
export interface ProviderKeyRow {
  keys: string
  description: string
  /** Short footer label (`a add`); omitted rows are help-only. */
  hint?: string
  screens: readonly ProviderScreen[]
}

export const providerKeyRows: readonly ProviderKeyRow[] = [
  { keys: "Up / Down", description: "Move the highlight over providers or models", hint: "↑↓ move", screens: ["list", "detail"] },
  { keys: "Enter", description: "Open the highlighted provider's models", hint: "Enter open", screens: ["list"] },
  { keys: "a", description: "Add a provider: name, protocol, base URL, key; its models are fetched right after", hint: "a add", screens: ["list"] },
  { keys: "t", description: "Test the highlighted model: sends hi with 1 output token and shows the reply, finish, and latency", hint: "t test", screens: ["detail"] },
  { keys: "m", description: "Add a model by hand (written to the provider's models: in config.yaml)", hint: "m add model", screens: ["detail"] },
  { keys: "e", description: "Edit the highlighted model's display name, context / output limits, and reasoning (config.yaml)", hint: "e edit", screens: ["detail"] },
  { keys: "d", description: "Delete the highlighted model's config.yaml entry (asks first); a remote model stays listed", hint: "d delete override", screens: ["detail"] },
  { keys: "k", description: "Set or replace the provider's API key (hidden input; applies at once)", hint: "k key", screens: ["list", "detail"] },
  { keys: "x", description: "Remove the provider's saved key (asks first)", hint: "x remove key", screens: ["list", "detail"] },
  { keys: "r", description: "Fetch the provider's latest model list", hint: "r refresh", screens: ["list", "detail"] },
  { keys: "/", description: "Filter the rows by typing; Enter keeps the filter, Esc clears it", hint: "/ filter", screens: ["list", "detail"] },
  { keys: "Esc", description: "Cancel a running call or pop-up, clear the filter, go back to the list, close the view", screens: ["list", "detail"] },
]

// ---- Formatting -----------------------------------------------------------

/** The server's error text without the `METHOD /v1/path:` prefix. */
export function errorText(error: unknown): string {
  if (error instanceof HttpError) return error.detail
  if (error instanceof Error) return error.message
  return String(error)
}

/** `ProviderSummary.keySource` in words. */
export function keySourceText(source: string | undefined): string {
  switch (source) {
    case "saved": return "saved key"
    case "oauth": return "oauth"
    case "config": return "config key"
    default: return "no key"
  }
}

/** `AuthStatus` in words. */
export function authText(auth: string | undefined): string {
  const name = (auth ?? "").replace(/^AUTH_STATUS_/, "")
  switch (name) {
    case "": case "UNSPECIFIED": return "—"
    case "CREDENTIALED": return "ready"
    case "UNAUTHENTICATED": return "no key"
    case "AUTH_REJECTED": return "key rejected"
    case "AUTH_REQUIRED": return "auth required"
    case "NOT_APPLICABLE": return "offline"
    default: return name.toLowerCase().replace(/_/g, " ")
  }
}

/** A token count (`uint64` string) as `64k`, `4.1k`, `1M`; `—` when unknown. */
export function tokenCount(value: string | number | undefined): string {
  const count = Number(value ?? 0)
  if (!Number.isFinite(count) || count <= 0) return "—"
  if (count >= 1_000_000) return `${Number((count / 1_000_000).toFixed(1))}M`
  if (count >= 10_000) return `${Math.round(count / 1000)}k`
  if (count >= 1000) return `${Number((count / 1000).toFixed(1))}k`
  return String(count)
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : "s"}`
}

function cell(text: string, width: number): string {
  const cut = truncate(text, width)
  return cut + " ".repeat(Math.max(0, width - Bun.stringWidth(cut)))
}

function kindText(provider: ProviderSummary): string {
  return provider.id === offlineProviderId && !provider.kind ? "offline" : (provider.kind || "—")
}

/** One provider row: id, protocol, key source, auth status, model count (fits `width`). */
export function providerLine(provider: ProviderSummary, width: number): string {
  const count = Number(provider.modelCount ?? 0)
  const line = `${cell(provider.id, 16)} ${cell(kindText(provider), 16)} ${cell(keySourceText(provider.keySource), 10)} ${cell(authText(provider.auth), 13)} ${plural(count, "model")}`
  return truncate(line.trimEnd(), width)
}

/** The detail screen's header line. */
export function providerDetailHeader(provider: ProviderSummary): string {
  if (provider.id === offlineProviderId && !provider.kind) return `${provider.id} · offline · built in`
  return [
    provider.id,
    kindText(provider),
    provider.baseUrl || undefined,
    keySourceText(provider.keySource),
    authText(provider.auth),
    plural(Number(provider.modelCount ?? 0), "model"),
  ].filter(Boolean).join(" · ")
}

/** Provider-local id of a model row. */
export function localModelId(model: ModelSummary): string {
  if (model.modelId) return model.modelId
  const slash = model.id.indexOf("/")
  return slash < 0 ? model.id : model.id.slice(slash + 1)
}

function providerOf(model: ModelSummary): string {
  return model.providerId || model.id.split("/")[0] || ""
}

/** The model id column: what the other columns leave, 10 to 40 wide. */
function modelIdWidth(width: number): number {
  return Math.min(40, Math.max(10, width - 52))
}

/** One model row: id, display name, source, context / output, reasoning (fits `width`). */
export function modelLine(model: ModelSummary, width: number): string {
  const idWidth = modelIdWidth(width)
  const name = model.displayName && model.displayName !== localModelId(model) ? model.displayName : ""
  const limits = `${tokenCount(model.contextLimit)} / ${tokenCount(model.outputLimit)}`
  const line = `${cell(localModelId(model), idWidth)} ${cell(name, 16)} ${cell(model.source || "—", 8)} ${cell(limits, 13)} ${model.reasoning ? "reasoning" : ""}`
  return truncate(line.trimEnd(), width)
}

/** The discovery outcome of an add / refresh / key save as a notice; `undefined` when nothing was fetched. */
export function discoveryNotice(provider: string, outcome: DiscoveryOutcome | undefined): ProviderNotice | undefined {
  if (!outcome) return undefined
  if (outcome.ok) {
    const count = Number(outcome.modelCount ?? 0)
    return count > 0
      ? { tone: "ok", text: `${provider}: ${plural(count, "model")} fetched` }
      : { tone: "info", text: `${provider}: the remote model list is empty · m adds a model by hand` }
  }
  const reason = outcome.result || "error"
  return { tone: "error", text: `${provider}: model fetch failed (${reason})${outcome.errorMessage ? `: ${outcome.errorMessage}` : ""}` }
}

/** A model test result in one line. */
export function testResultText(result: ProviderTestResult): string {
  const latency = `${Number(result.latencyMs ?? 0)} ms`
  const ref = `${result.provider}/${result.model}`
  if (result.ok) {
    const reply = (result.text ?? "").replace(/\s+/g, " ").trim()
    return [`✓ ${ref} replied`, latency, result.finishReason ? `finish ${result.finishReason}` : undefined, reply ? `"${truncate(reply, 40)}"` : "empty reply"]
      .filter(Boolean).join(" · ")
  }
  const error = [result.errorCode, result.errorMessage].filter(Boolean).join(": ") || "no reply"
  return `✗ ${ref} failed · ${latency} · ${error}`
}

// ---- Rows -----------------------------------------------------------------

function matches(haystack: string, filter: string): boolean {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean)
  const text = haystack.toLowerCase()
  return words.every((word) => text.includes(word))
}

/** Providers in view order (configured ones first, the offline provider last), filtered on the list screen. */
export function shownProviders(view: Pick<ProviderViewState, "screen" | "filter">, providers: readonly ProviderSummary[]): ProviderSummary[] {
  const ordered = [...providers.filter((row) => row.id !== offlineProviderId), ...providers.filter((row) => row.id === offlineProviderId)]
  if (view.screen !== "list" || !view.filter) return ordered
  return ordered.filter((row) => matches(`${row.id}\n${row.kind ?? ""}\n${row.baseUrl ?? ""}\n${row.keySource ?? ""}`, view.filter))
}

/** The models of `provider`, filtered by id, display name, or source. */
export function providerModels(models: readonly ModelSummary[], provider: string, filter: string): ModelSummary[] {
  return models
    .filter((model) => providerOf(model) === provider)
    .filter((model) => !filter || matches(`${model.id}\n${model.displayName ?? ""}\n${model.source ?? ""}`, filter))
}

function shownModels(view: ProviderViewState, models: readonly ModelSummary[]): ModelSummary[] {
  return view.provider ? providerModels(models, view.provider, view.screen === "detail" ? view.filter : "") : []
}

/** Keep the highlight on a shown row (the first when the highlighted one is filtered out or gone). */
function settle(view: ProviderViewState, data: ProviderData): ProviderViewState {
  if (view.screen === "list") {
    const rows = shownProviders(view, data.providers)
    return rows.some((row) => row.id === view.provider) || !rows.length ? view : { ...view, provider: rows[0]!.id }
  }
  const rows = shownModels(view, data.models)
  return rows.some((row) => row.id === view.model) ? view : { ...view, model: rows[0]?.id }
}

/** The view as opened by `/key`: the list, the first configured provider highlighted. */
export function initialProviderView(providers: readonly ProviderSummary[]): ProviderViewState {
  return { screen: "list", provider: shownProviders({ screen: "list", filter: "" }, providers)[0]?.id, model: undefined, filter: "", filtering: false }
}

/** The view after a catalog refresh: highlights stay on their rows where they still exist. */
export function settleProviderView(view: ProviderViewState, data: ProviderData): ProviderViewState {
  return settle(view, data)
}

/** Open `provider`'s detail screen (after adding it, or Enter on its row). */
export function openProviderDetail(view: ProviderViewState, provider: string, data: ProviderData): ProviderViewState {
  return settle({ ...view, screen: "detail", provider, model: undefined, filter: "", filtering: false }, data)
}

// ---- Forms ----------------------------------------------------------------

const providerIdPattern = /^[A-Za-z0-9_-]{1,64}$/

/** Largest context / output limit (`SetProviderModel`'s uint32 fields). */
export const maxTokenLimit = 4_294_967_295

/** The server's provider id rule (1–64 of `A-Z a-z 0-9 - _`, not `hya`), plus "not taken". */
export function validateProviderId(id: string, existing: readonly string[]): string | undefined {
  if (!providerIdPattern.test(id)) return "Use 1–64 letters, digits, - or _"
  if (id === offlineProviderId) return "hya is reserved for the offline provider"
  if (existing.includes(id)) return `Provider ${id} already exists · Esc, then k on its row sets its key`
  return undefined
}

/** An `http(s)://host…` URL without credentials. */
export function validateBaseUrl(url: string): string | undefined {
  if (!/^https?:\/\/[^\s/?#]+/i.test(url)) return "Enter an http:// or https:// URL"
  const authority = url.replace(/^https?:\/\//i, "").split(/[/?#]/, 1)[0] ?? ""
  if (authority.includes("@")) return "The URL must not contain credentials; the key goes in the next step"
  return undefined
}

export function addProviderForm(existing: readonly string[]): FormState {
  return {
    kind: "addProvider",
    title: "Add provider",
    step: 0,
    existing,
    fields: [
      { id: "name", label: "Name", kind: "text", value: "", placeholder: "letters, digits, - or _ (the provider id)" },
      { id: "protocol", label: "Protocol", kind: "choice", value: providerProtocols[0]!.id, options: providerProtocols },
      { id: "baseUrl", label: "Base URL", kind: "text", value: "", placeholder: "https://api.example.com/v1" },
      { id: "key", label: "API key", kind: "secret", value: "", masked: 0, optional: true, placeholder: "hidden · Enter with none skips (local endpoints)" },
    ],
  }
}

export function setKeyForm(provider: string): FormState {
  return {
    kind: "setKey",
    title: `API key · ${provider}`,
    step: 0,
    provider,
    fields: [{ id: "key", label: "API key", kind: "secret", value: "", masked: 0, placeholder: "hidden · paste or type" }],
  }
}

function limitFields(model?: ModelSummary): FormField[] {
  const limit = (value: string | undefined) => (value && value !== "0" ? value : "")
  const fields: FormField[] = [
    { id: "displayName", label: "Display name", kind: "text", value: model?.displayName && model.displayName !== localModelId(model) ? model.displayName : "", optional: true, placeholder: "optional" },
    { id: "contextLimit", label: "Context limit", kind: "number", value: limit(model?.contextLimit), optional: true, placeholder: "tokens · optional" },
    { id: "outputLimit", label: "Output limit", kind: "number", value: limit(model?.outputLimit), optional: true, placeholder: "tokens · optional" },
    { id: "reasoning", label: "Reasoning", kind: "choice", value: model?.reasoning ? "on" : "default", options: reasoningOptions },
  ]
  return fields.map((field) => ({ ...field, initial: field.value }))
}

/**
 * The `SetProviderModel` body of a submitted model form: only the fields the
 * user changed (add model: filled in), so values the server shows for a
 * model without real metadata (a fallback context limit, `reasoning`) are
 * never written into config.yaml. A cleared text or limit field is sent
 * empty / `0`, which removes that field from the entry; `default` reasoning
 * is omitted. `undefined` when an edit changed nothing.
 */
export function modelPatch(form: FormState, values: Record<string, string>): ProviderModelPatch | undefined {
  const modelId = form.kind === "editModel" ? (form.model ?? "") : (values.modelId ?? "")
  const changed = (id: string): boolean => {
    const field = form.fields.find((candidate) => candidate.id === id)
    return field !== undefined && field.value.trim() !== (field.initial ?? "").trim()
  }
  const patch: ProviderModelPatch = { modelId }
  if (changed("displayName")) patch.displayName = values.displayName ?? ""
  if (changed("contextLimit")) patch.contextLimit = Number(values.contextLimit || 0)
  if (changed("outputLimit")) patch.outputLimit = Number(values.outputLimit || 0)
  if (changed("reasoning") && values.reasoning !== "default") patch.reasoning = values.reasoning === "on"
  if (form.kind === "editModel" && !["displayName", "contextLimit", "outputLimit", "reasoning"].some(changed)) return undefined
  return patch
}

export function addModelForm(provider: string): FormState {
  return {
    kind: "addModel",
    title: `Add model · ${provider}`,
    step: 0,
    provider,
    fields: [{ id: "modelId", label: "Model id", kind: "text", value: "", placeholder: "the id the provider serves" }, ...limitFields()],
  }
}

export function editModelForm(provider: string, model: ModelSummary): FormState {
  const id = localModelId(model)
  return { kind: "editModel", title: `Edit model · ${provider}/${id}`, step: 0, provider, model: id, fields: limitFields(model) }
}

function confirmForm(text: string, action: "removeKey" | "deleteModel", provider: string, model?: string): FormState {
  return { kind: "confirm", title: "Confirm", step: 0, fields: [], action, provider, ...(model ? { model } : {}), confirmText: text }
}

function values(form: FormState): Record<string, string> {
  return Object.fromEntries(form.fields.map((field) => [field.id, field.value.trim()]))
}

/** Why the current field cannot be accepted; `undefined` when it can. */
function validateStep(form: FormState): string | undefined {
  const field = form.fields[form.step]
  if (!field) return undefined
  const value = field.value.trim()
  if (field.kind === "secret") return !field.optional && !field.masked ? "The key cannot be empty" : undefined
  if (!value && !field.optional && field.kind !== "choice") return `${field.id === "modelId" ? "A model id" : field.label} is required`
  if (form.kind === "addProvider" && field.id === "name") return validateProviderId(value, form.existing ?? [])
  if (field.id === "baseUrl") return validateBaseUrl(value)
  // `limit.context` / `limit.output` are uint32 on the wire.
  if (field.kind === "number" && value && Number(value) > maxTokenLimit) return `${field.label} is at most ${maxTokenLimit} tokens`
  if (field.id === "outputLimit" && value) {
    const context = Number(values(form).contextLimit || 0)
    if (context > 0 && Number(value) > context) return "The output limit must not exceed the context limit"
  }
  return undefined
}

function setField(form: FormState, value: string): FormState {
  return { ...form, error: undefined, fields: form.fields.map((field, index) => index === form.step ? { ...field, value } : field) }
}

/** The current secret field now holds `length` characters (the controller's `SecretEntry`). */
export function withSecretLength(form: FormState, length: number): FormState {
  return { ...form, error: undefined, fields: form.fields.map((field, index) => index === form.step && field.kind === "secret" ? { ...field, masked: length } : field) }
}

const printable = (key: KeyLike): boolean => !key.ctrl && !key.meta && key.sequence.length === 1 && key.sequence >= " " && key.sequence !== "\x7f"
/** Enter without Alt: a terminal reports Esc quickly followed by Enter as Alt+Enter, which must never submit a form. */
const isEnter = (key: KeyLike): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter")

/** One key in a pop-up form. */
export function formKey(form: FormState, key: KeyLike): FormOutcome {
  if (key.name === "escape") return { type: "cancel" }
  if (form.kind === "confirm") return isEnter(key) ? { type: "submit", form, values: {} } : { type: "none" }
  const field = form.fields[form.step]
  if (!field) return { type: "none" }
  if (isEnter(key)) {
    const error = validateStep(form)
    if (error) return { type: "update", form: { ...form, error } }
    if (form.step + 1 < form.fields.length) return { type: "update", form: { ...form, step: form.step + 1, error: undefined } }
    return { type: "submit", form, values: values(form) }
  }
  if (field.kind === "secret") {
    if (key.name === "backspace") return { type: "secret", op: "backspace" }
    return printable(key) ? { type: "secret", op: "append", text: key.sequence } : { type: "none" }
  }
  if (field.kind === "choice") {
    const options = field.options ?? []
    const at = Math.max(0, options.findIndex((option) => option.id === field.value))
    const step = key.name === "up" || (key.name === "tab" && key.shift) ? -1 : key.name === "down" || key.name === "tab" ? 1 : 0
    if (step && options.length) return { type: "update", form: setField(form, options[(at + step + options.length) % options.length]!.id) }
    const digit = Number(key.sequence)
    if (printable(key) && Number.isInteger(digit) && digit >= 1 && digit <= options.length) return { type: "update", form: setField(form, options[digit - 1]!.id) }
    return { type: "none" }
  }
  if (key.name === "backspace") return { type: "update", form: setField(form, field.value.slice(0, -1)) }
  if (key.ctrl && !key.meta && key.name === "u") return { type: "update", form: setField(form, "") }
  if (!printable(key)) return { type: "none" }
  if (field.kind === "number" && !/[0-9]/.test(key.sequence)) return { type: "none" }
  return { type: "update", form: setField(form, field.value + key.sequence) }
}

/** A bracketed paste into a form: a text field takes it without line breaks; a secret field hands it to the controller. */
export function formPaste(form: FormState, text: string): FormOutcome {
  const field = form.fields[form.step]
  if (!field || field.kind === "choice") return { type: "none" }
  if (field.kind === "secret") return { type: "secret", op: "append", text }
  // eslint-disable-next-line no-control-regex
  let clean = text.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "").replace(/[\r\n\t]+/g, "").replace(/[\x00-\x1f\x7f]/g, "")
  if (field.kind === "number") clean = clean.replace(/[^0-9]/g, "")
  return clean ? { type: "update", form: setField(form, field.value + clean) } : { type: "none" }
}

// ---- Keys -----------------------------------------------------------------

function move(view: ProviderViewState, data: ProviderData, step: number): ProviderViewState {
  if (view.screen === "list") {
    const rows = shownProviders(view, data.providers)
    if (!rows.length) return view
    const at = rows.findIndex((row) => row.id === view.provider)
    return { ...view, provider: rows[(at + step + rows.length) % rows.length]!.id }
  }
  const rows = shownModels(view, data.models)
  if (!rows.length) return view
  const at = rows.findIndex((row) => row.id === view.model)
  return { ...view, model: rows[(at + step + rows.length) % rows.length]!.id }
}

const note = (view: ProviderViewState, text: string, tone: ProviderNotice["tone"] = "info"): ProviderViewOutcome =>
  ({ type: "update", view: { ...view, notice: { text, tone } } })

/** One key while the Provider View is open (the controller runs commands and submissions). */
export function providerViewKey(view: ProviderViewState, key: KeyLike, data: ProviderData): ProviderViewOutcome {
  if (view.form) {
    if (view.busy) return key.name === "escape" ? { type: "cancelBusy" } : { type: "none" }
    const outcome = formKey(view.form, key)
    if (outcome.type === "update") return { type: "update", view: { ...view, form: outcome.form } }
    if (outcome.type === "cancel") return { type: "update", view: { ...view, form: undefined, notice: { text: "Cancelled", tone: "info" } } }
    return outcome
  }
  if (view.busy && key.name === "escape") return { type: "cancelBusy" }
  if (key.name === "up") return { type: "update", view: move(view, data, -1) }
  if (key.name === "down") return { type: "update", view: move(view, data, 1) }
  if (view.filtering) {
    if (key.name === "escape") return { type: "update", view: settle({ ...view, filtering: false, filter: "" }, data) }
    if (isEnter(key)) return { type: "update", view: { ...view, filtering: false } }
    if (key.name === "backspace") return { type: "update", view: settle({ ...view, filter: view.filter.slice(0, -1) }, data) }
    if (printable(key)) return { type: "update", view: settle({ ...view, filter: view.filter + key.sequence }, data) }
    return { type: "none" }
  }
  if (key.name === "escape" || (key.name === "left" && view.screen === "detail")) {
    if (view.filter) return { type: "update", view: settle({ ...view, filter: "" }, data) }
    if (view.screen === "detail") return { type: "update", view: settle({ ...view, screen: "list", model: undefined, filter: "", test: undefined }, data) }
    return key.name === "escape" ? { type: "close" } : { type: "none" }
  }
  if (key.ctrl || key.meta) return { type: "none" }
  if (key.sequence === "/") return { type: "update", view: { ...view, filtering: true } }
  const provider = data.providers.find((row) => row.id === view.provider)
  if ((isEnter(key) || key.name === "right") && view.screen === "list") {
    return provider ? { type: "update", view: openProviderDetail(view, provider.id, data) } : { type: "none" }
  }
  const action = key.sequence
  if (!"akxrtmed".includes(action) || action.length !== 1) return { type: "none" }
  if (view.busy) return note(view, `Busy: ${view.busy.label} · Esc cancels`)
  if (action === "a") return { type: "update", view: { ...view, notice: undefined, form: addProviderForm(data.providers.map((row) => row.id)) } }
  if (!provider) return note(view, "No provider selected · a adds one")
  const offline = provider.id === offlineProviderId
  if (offline && "kxrmed".includes(action)) return note(view, "hya is the built-in offline provider: no key, no model list to fetch or edit")
  switch (action) {
    case "k": return { type: "update", view: { ...view, notice: undefined, form: setKeyForm(provider.id) } }
    case "x":
      if (provider.keySource !== "saved" && provider.keySource !== "oauth") {
        return note(view, `${provider.id} has no saved key${provider.keySource === "config" ? " (its key comes from config.yaml)" : ""}`)
      }
      return { type: "update", view: { ...view, notice: undefined, form: confirmForm(`Remove the saved key of ${provider.id}? · Enter removes · Esc cancels`, "removeKey", provider.id) } }
    case "r": return { type: "command", command: { kind: "refresh", provider: provider.id } }
  }
  if (view.screen !== "detail") return note(view, "Enter opens the provider's models first")
  if (action === "m") return { type: "update", view: { ...view, notice: undefined, form: addModelForm(provider.id) } }
  const model = shownModels(view, data.models).find((row) => row.id === view.model)
  if (!model) return note(view, "No model selected · m adds one")
  const id = localModelId(model)
  if (action === "t") return { type: "command", command: { kind: "test", provider: provider.id, model: id } }
  if (action === "e") return { type: "update", view: { ...view, notice: undefined, form: editModelForm(provider.id, model) } }
  // `d`
  if (model.source !== "config" && model.source !== "override") return note(view, `${id} comes from the ${model.source || "remote"} list: it has no config.yaml entry to delete`)
  return { type: "update", view: { ...view, notice: undefined, form: confirmForm(`Delete the config.yaml entry of ${id}? · Enter deletes · Esc cancels`, "deleteModel", provider.id, id) } }
}

/** The footer hint for the current screen, filter, or pop-up. */
export function providerViewHint(view: ProviderViewState): string {
  const form = view.form
  if (form) {
    if (view.busy) return `${view.busy.label}… · Esc cancels`
    if (form.kind === "confirm") return "Enter confirms · Esc cancels"
    const field = form.fields[form.step]
    const last = form.step + 1 >= form.fields.length
    const next = last ? (form.kind === "setKey" ? "Enter saves" : form.kind === "addProvider" ? "Enter adds" : "Enter saves") : "Enter next"
    if (field?.kind === "choice") return `↑↓ or 1-${field.options?.length ?? 1} choose · ${next} · Esc cancels`
    if (field?.kind === "secret") return `Type or paste the key (hidden) · ${next} · Esc cancels`
    return `${next} · Esc cancels`
  }
  if (view.filtering) return "Type to filter · Enter keeps the filter · Esc clears it"
  const keys = providerKeyRows.filter((row) => row.hint && row.screens.includes(view.screen)).map((row) => row.hint!)
  return [...keys, view.screen === "detail" ? "Esc back" : "Esc close"].join(" · ")
}

// ---- Model selection after adding -----------------------------------------

/** What a turn sent now would run on: the session's model, else what a new session would get (app/controller.ts `newSession`). */
export function defaultModelRef(state: {
  selected: SessionInfo | undefined
  pendingModel: string | undefined
  pendingAgent: string | undefined
  agents: readonly AgentSummary[]
  models: readonly ModelSummary[]
}): string {
  if (state.selected) return modelReference(state.selected)
  const agent = state.pendingAgent ?? state.agents.find((item) => !item.hidden)?.name ?? "build"
  const preferred = state.agents.find((item) => item.name === agent)?.model
  return state.pendingModel ?? (preferred?.providerId && preferred.modelId ? `${preferred.providerId}/${preferred.modelId}` : state.models[0]?.id ?? "")
}

/** Column titles over `providerLine` rows. */
export function providerHeaderLine(width: number): string {
  return truncate(`${cell("PROVIDER", 16)} ${cell("PROTOCOL", 16)} ${cell("KEY", 10)} ${cell("STATUS", 13)} MODELS`, width)
}

/** Column titles over `modelLine` rows. */
export function modelHeaderLine(width: number): string {
  const idWidth = modelIdWidth(width)
  return truncate(`${cell("MODEL", idWidth)} ${cell("NAME", 16)} ${cell("SOURCE", 8)} ${cell("CTX / OUT", 13)} REASONING`, width)
}
