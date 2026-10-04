/**
 * The Bundles view's calls (`/bundles`; state/bundles.ts holds its state and
 * keys): `ListBundles`, `InstallBundle`, `UninstallBundle`,
 * `SetBundleEnabled`, and the TUI-side preferences of a bundle's extension
 * (`extensionEnabled`, `extensionTrusted`). After every change the list and
 * the TUI extension catalog reload, so panels appear and disappear at once.
 */
import { isAbsolute, resolve } from "node:path"
import type { HyaClient } from "../client"
import { setExtensionTrust } from "../commands/native"
import { extensionManager } from "../extensions/manager"
import type { KeyLike } from "../keys/bindings"
import { loadPreferences, preferencesPath, type TuiPreferences } from "../prefs"
import { bundleRows, bundlesViewKey, initialBundlesView, settleBundlesView, type BundleCommand, type BundleViewState } from "../state/bundles"
import { errorText } from "../state/providers"
import type { AppStore } from "../state/store"

export interface BundlesControllerOptions {
  store: AppStore
  client: HyaClient
  /** The TUI's working directory: a relative package path is resolved against it. */
  directory: string
  /** Persist TUI preferences (no-op without a preferences file). */
  savePreferences: (patch: Partial<TuiPreferences>) => void
  /** Reload the scope's TUI extension catalog (app/run.tsx). */
  refreshExtensions: () => Promise<void>
}

export function createBundlesController({ store, client, directory, savePreferences, refreshExtensions }: BundlesControllerOptions) {
  let abort: AbortController | undefined
  const rows = () => bundleRows(store.state.bundles, extensionManager.list())
  const view = (): BundleViewState | undefined => store.state.bundlesView
  const patch = (change: (current: BundleViewState) => BundleViewState): void => {
    const current = view()
    if (current) store.setBundlesView(change(current))
  }

  async function reload(): Promise<void> {
    try {
      store.setBundles(await client.listBundles())
      patch((current) => settleBundlesView(current, rows()))
    } catch (error) {
      patch((current) => ({ ...current, notice: { tone: "error", text: `Reload failed: ${errorText(error)}` } }))
    }
  }

  function open(): void {
    store.setBundlesView(initialBundlesView(rows(), client.directory !== ""))
    void reload()
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setBundlesView(undefined)
  }

  /** Run one change, then reload the list and the extension catalog; the outcome is the view's notice. */
  async function run(label: string, done: string, change: (signal: AbortSignal) => Promise<void>): Promise<void> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, popup: undefined, notice: undefined, busy: { label, startedAt: Date.now() } }))
    try {
      await change(controller.signal)
      await refreshExtensions()
      await reload()
      patch((current) => ({ ...current, busy: undefined, notice: { tone: "ok", text: done } }))
    } catch (error) {
      const cancelled = controller.signal.aborted
      patch((current) => ({ ...current, busy: undefined, notice: cancelled ? { tone: "info", text: `${label} cancelled` } : { tone: "error", text: errorText(error) } }))
    } finally {
      if (abort === controller) abort = undefined
    }
  }

  function command(command: BundleCommand): Promise<void> {
    switch (command.kind) {
      case "install": {
        const path = isAbsolute(command.path) ? command.path : resolve(directory, command.path)
        return run("Installing", `Installed ${path}${command.project ? " into the project" : ""}`, (signal) => client.installBundle(path, command.project, signal))
      }
      case "uninstall":
        return run(`Uninstalling ${command.bundleId}`, `Uninstalled ${command.bundleId}`, (signal) => client.uninstallBundle(command.bundleId, command.project, signal))
      case "setEnabled":
        return run(`${command.enabled ? "Enabling" : "Disabling"} ${command.bundleId}`, `${command.bundleId} ${command.enabled ? "enabled" : "disabled"}`, async (signal) => {
          await client.setBundleEnabled(command.bundleId, command.enabled, signal)
          // The TUI's own switch follows, so a remote backend's extension is not left blocked.
          if (!command.tui) return
          const enabled = loadPreferences(preferencesPath(process.env)).preferences.extensionEnabled ?? {}
          savePreferences({ extensionEnabled: { ...enabled, [command.bundleId]: command.enabled } })
        })
      case "setTrusted":
        return run(
          `${command.trusted ? "Trusting" : "Untrusting"} ${command.bundleId}`,
          command.trusted ? `${command.bundleId} trusted: its TUI extension runs on the JIT (no VM memory cap)` : `${command.bundleId} untrusted: its TUI extension runs in the VM`,
          () => setExtensionTrust(command.bundleId, command.trusted, savePreferences),
        )
    }
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = bundlesViewKey(current, pressed, rows())
    switch (outcome.type) {
      case "update": store.setBundlesView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void reload(); return
      case "command": void command(outcome.command); return
      case "none": return
    }
  }

  return { open, close, key, dispose: () => abort?.abort() }
}
