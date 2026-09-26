/**
 * The Provider View's calls (docs/tui.md "Provider View"; the pure state is
 * state/providers.ts, the rendering components/ProviderView.tsx).
 *
 * Every provider write applies live on the server (it rebuilds the route
 * and catalog and emits `catalog.updated`, which only reaches in-process
 * subscribers), so after each call the TUI re-reads its catalog: the
 * view's rows and the `/model` picker's models are current at once.
 *
 * One call runs at a time (`busy`); Esc aborts the request (the server may
 * still finish a write, so the catalog is re-read after a cancel too). A
 * key typed into a `secret` field goes to `SecretEntry` here and never into
 * the store; it is cleared when the form closes.
 *
 * After adding a provider whose models were fetched, when the next turn
 * would run on the offline `hya` provider, the `/model` picker opens over
 * the view with the new provider's first model highlighted.
 */
import type { HyaClient } from "../client"
import { SecretEntry } from "../completion"
import type { KeyLike } from "../keys/bindings"
import type { AppStore } from "../state/store"
import {
  defaultModelRef,
  discoveryNotice,
  errorText,
  formPaste,
  initialProviderView,
  modelPatch,
  offlineProviderId,
  openProviderDetail,
  providerViewKey,
  settleProviderView,
  withSecretLength,
  type FormState,
  type ProviderBusy,
  type ProviderCommand,
  type ProviderNotice,
  type ProviderViewState,
} from "../state/providers"

export interface ProviderControllerOptions {
  store: AppStore
  client: HyaClient
  /** The full catalog refresh (app/controller.ts `refresh`). */
  refresh(): Promise<void>
  /** Open the `/model` picker with `provider`'s first model highlighted; `onChosen` runs with the chosen `provider/model`. */
  pickModel(provider: string, onChosen: (model: string) => void): void
}

type Outcome<T> = { ok: true; value: T } | { ok: false; cancelled: boolean; error?: unknown }

export function createProviderController({ store, client, refresh, pickModel }: ProviderControllerOptions) {
  const secret = new SecretEntry()
  let abort: AbortController | undefined

  const view = (): ProviderViewState | undefined => store.state.providerView
  const data = () => ({ providers: store.state.providers, models: store.state.models })
  const patch = (change: (current: ProviderViewState) => ProviderViewState): void => {
    const current = view()
    if (current) store.setProviderView(change(current))
  }
  const notify = (notice: ProviderNotice | undefined): void => patch((current) => ({ ...current, notice }))

  /** Re-read the catalog and keep the highlights on their rows. */
  async function reload(): Promise<void> {
    try {
      await refresh()
      patch((current) => settleProviderView(current, data()))
    } catch (error) {
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  function open(): void {
    secret.clear()
    store.setProviderView(initialProviderView(store.state.providers))
    void reload().then(() => patch((current) => current.provider ? current : initialProviderView(store.state.providers)))
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    secret.clear()
    store.setProviderView(undefined)
  }

  /** Run one call with the busy line shown; Esc (`cancelBusy`) aborts it. */
  async function run<T>(busy: Omit<ProviderBusy, "startedAt">, call: (signal: AbortSignal) => Promise<T>): Promise<Outcome<T>> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { ...busy, startedAt: Date.now() }, notice: undefined }))
    try {
      return { ok: true, value: await call(controller.signal) }
    } catch (error) {
      return controller.signal.aborted ? { ok: false, cancelled: true } : { ok: false, cancelled: false, error }
    } finally {
      if (abort === controller) abort = undefined
      patch((current) => ({ ...current, busy: undefined }))
    }
  }

  /** A form submission failed: keep the form (and the typed key) with the server's reason on the step it is about. */
  function formFailed(form: FormState, outcome: Outcome<unknown>): void {
    if (outcome.ok) return
    if (outcome.cancelled) {
      notify({ tone: "info", text: "Cancelled · the server may still apply it" })
      void reload()
      return
    }
    const message = errorText(outcome.error)
    const target = form.kind === "addProvider"
      ? /url/i.test(message) ? 2 : /kind|protocol/i.test(message) ? 1 : /\bid\b|name/i.test(message) ? 0 : form.step
      : form.step
    patch((current) => ({ ...current, form: current.form && { ...current.form, step: target, error: message } }))
  }

  function succeeded(notice: ProviderNotice, change: (current: ProviderViewState) => ProviderViewState = (current) => current): Promise<void> {
    secret.clear()
    patch((current) => change({ ...current, form: undefined, notice }))
    return reload()
  }

  /** Strip a discovery notice's `id: ` prefix so it reads after `Added id · `. */
  const detail = (provider: string, notice: ProviderNotice): string => notice.text.replace(`${provider}: `, "")

  async function addProvider(form: FormState, values: Record<string, string>): Promise<void> {
    const name = values.name ?? ""
    const apiKey = secret.peek()
    const outcome = await run({ kind: "add", label: `Adding ${name} · fetching its models`, provider: name }, (signal) =>
      client.upsertProvider(name, { kind: values.protocol ?? "openai", baseUrl: values.baseUrl ?? "", ...(apiKey ? { apiKey } : {}) }, signal))
    if (!outcome.ok) return formFailed(form, outcome)
    const discovered = discoveryNotice(name, outcome.value.discovery)
    const notice: ProviderNotice = discovered
      ? { tone: discovered.tone === "error" ? "error" : "ok", text: `Added ${name} · ${detail(name, discovered)}` }
      : { tone: "ok", text: `Added ${name}` }
    // The detail's model highlight settles on the first model once the catalog is re-read.
    await succeeded(notice, (current) => openProviderDetail(current, name, data()))
    const fetched = Number(outcome.value.discovery?.modelCount ?? 0) > 0 || store.state.models.some((model) => model.providerId === name)
    if (fetched && defaultModelRef(store.state).startsWith(`${offlineProviderId}/`)) {
      notify({ tone: notice.tone, text: `${notice.text} · the session uses ${offlineProviderId}/offline: pick a model` })
      pickModel(name, (model) => notify({ tone: "ok", text: `Model → ${model}` }))
    }
  }

  async function setKey(form: FormState): Promise<void> {
    const provider = form.provider ?? ""
    const outcome = await run({ kind: "key", label: `Saving the key of ${provider}`, provider }, (signal) => client.setProviderKey(provider, secret.peek(), signal))
    if (!outcome.ok) return formFailed(form, outcome)
    const discovered = discoveryNotice(provider, outcome.value.discovery)
    await succeeded({ tone: discovered?.tone === "error" ? "error" : "ok", text: `Saved the key of ${provider} · applies now${discovered ? ` · ${detail(provider, discovered)}` : ""}` })
  }

  async function confirm(form: FormState): Promise<void> {
    const provider = form.provider ?? ""
    if (form.action === "removeKey") {
      const outcome = await run({ kind: "removeKey", label: `Removing the key of ${provider}`, provider }, (signal) => client.removeProviderKey(provider, signal))
      if (!outcome.ok) return formFailed(form, outcome)
      await succeeded({ tone: "ok", text: `Removed the saved key of ${provider}` })
      return
    }
    const model = form.model ?? ""
    const outcome = await run({ kind: "deleteModel", label: `Deleting the config entry of ${model}`, provider }, (signal) => client.removeProviderModel(provider, model, signal))
    if (!outcome.ok) return formFailed(form, outcome)
    await succeeded({ tone: "ok", text: `Deleted the config.yaml entry of ${model}` })
  }

  async function saveModel(form: FormState, values: Record<string, string>): Promise<void> {
    const provider = form.provider ?? ""
    const body = modelPatch(form, values)
    if (!body) {
      patch((current) => ({ ...current, form: undefined, notice: { tone: "info", text: `No changes to ${form.model ?? ""}` } }))
      return
    }
    const modelId = body.modelId
    const outcome = await run({ kind: "model", label: `Saving ${modelId}`, provider }, (signal) => client.setProviderModel(provider, body, signal))
    if (!outcome.ok) return formFailed(form, outcome)
    await succeeded({ tone: "ok", text: `Saved ${modelId} to config.yaml` }, (current) => ({ ...current, model: `${provider}/${modelId}` }))
  }

  async function command(next: ProviderCommand): Promise<void> {
    if (next.kind === "refresh") {
      const outcome = await run({ kind: "refresh", label: `Fetching the models of ${next.provider}`, provider: next.provider }, (signal) => client.refreshProvider(next.provider, signal))
      if (!outcome.ok) {
        notify(outcome.cancelled ? { tone: "info", text: "Refresh cancelled" } : { tone: "error", text: `${next.provider}: ${errorText(outcome.error)}` })
        return
      }
      notify(discoveryNotice(next.provider, outcome.value.discovery) ?? { tone: "ok", text: `Refreshed ${next.provider}` })
      await reload()
      return
    }
    const outcome = await run({ kind: "test", label: `Testing ${next.provider}/${next.model}`, provider: next.provider }, (signal) => client.testProviderModel(next.provider, next.model, signal))
    if (!outcome.ok) {
      notify(outcome.cancelled ? { tone: "info", text: "Test cancelled" } : { tone: "error", text: `Test of ${next.provider}/${next.model} failed: ${errorText(outcome.error)}` })
      return
    }
    patch((current) => ({ ...current, test: { ...outcome.value, provider: next.provider, model: next.model } }))
  }

  function submit(form: FormState, values: Record<string, string>): void {
    const done = form.kind === "addProvider" ? addProvider(form, values)
      : form.kind === "setKey" ? setKey(form)
      : form.kind === "confirm" ? confirm(form)
      : saveModel(form, values)
    void done.catch((error: unknown) => notify({ tone: "error", text: errorText(error) }))
  }

  /** One key while the view is open. */
  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = providerViewKey(current, pressed, data())
    switch (outcome.type) {
      case "update":
        if (!outcome.view.form) secret.clear()
        store.setProviderView(outcome.view)
        return
      case "close": return close()
      case "cancelBusy":
        abort?.abort()
        return
      case "secret":
        if (outcome.op === "append") secret.append(outcome.text)
        else secret.backspace()
        patch((view) => ({ ...view, form: view.form && withSecretLength(view.form, secret.length) }))
        return
      case "submit": return submit(outcome.form, outcome.values)
      case "command": {
        void command(outcome.command).catch((error: unknown) => notify({ tone: "error", text: errorText(error) }))
        return
      }
    }
  }

  /** A bracketed paste while the view is open: into the open form's field, else into the filter. */
  function paste(text: string): void {
    const current = view()
    if (!current || current.busy) return
    if (!current.form) {
      if (current.filtering) patch((view) => settleProviderView({ ...view, filter: view.filter + text.replace(/[\r\n]+/g, "") }, data()))
      return
    }
    const outcome = formPaste(current.form, text)
    if (outcome.type === "update") patch((view) => ({ ...view, form: outcome.form }))
    else if (outcome.type === "secret" && outcome.op === "append") {
      secret.append(outcome.text)
      patch((view) => ({ ...view, form: view.form && withSecretLength(view.form, secret.length) }))
    }
  }

  return { open, close, key, paste, dispose: () => { abort?.abort(); secret.clear() } }
}

export type ProviderController = ReturnType<typeof createProviderController>
