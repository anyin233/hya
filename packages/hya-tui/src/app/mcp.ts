/**
 * The MCP view's calls (docs/tui.md "MCP servers"; the pure state is
 * state/mcp.ts, the rendering components/McpView.tsx).
 *
 * `StartMcpAuth`'s URL is copied to the clipboard (OSC 52, the same action
 * `/copy` uses) the moment it comes back, before the code pop-up opens.
 */
import type { HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import { errorText } from "../state/providers"
import { initialMcpView, mcpViewKey, settleMcpView, type McpCommand, type McpNotice, type McpViewState } from "../state/mcp"
import type { AppStore } from "../state/store"

export interface McpControllerOptions {
  store: AppStore
  client: HyaClient
  /** Copy text to the clipboard (OSC 52); `false` when the terminal declined it. */
  copyText(text: string): boolean
}

export function createMcpController({ store, client, copyText }: McpControllerOptions) {
  let abort: AbortController | undefined

  const view = (): McpViewState | undefined => store.state.mcpView
  const patch = (change: (current: McpViewState) => McpViewState): void => {
    const current = view()
    if (current) store.setMcpView(change(current))
  }
  const notify = (notice: McpNotice | undefined): void => patch((current) => ({ ...current, notice }))

  async function reload(): Promise<void> {
    try {
      const servers = await client.getMcpStatus()
      store.setMcpServers(servers)
      patch((current) => settleMcpView(current, servers))
    } catch (error) {
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  function open(): void {
    store.setMcpView(initialMcpView(store.state.mcpServers))
    void reload()
  }

  function close(): void {
    abort?.abort()
    abort = undefined
    store.setMcpView(undefined)
  }

  async function run(kind: "refresh" | "connect" | "disconnect" | "auth" | "authComplete", label: string, server: string, call: (signal: AbortSignal) => Promise<unknown>): Promise<{ ok: true; value: unknown } | { ok: false; cancelled: boolean; error?: unknown }> {
    const controller = new AbortController()
    abort = controller
    patch((current) => ({ ...current, busy: { kind, label, server, startedAt: Date.now() }, notice: undefined }))
    try {
      return { ok: true, value: await call(controller.signal) }
    } catch (error) {
      return controller.signal.aborted ? { ok: false, cancelled: true } : { ok: false, cancelled: false, error }
    } finally {
      if (abort === controller) abort = undefined
      patch((current) => ({ ...current, busy: undefined }))
    }
  }

  async function refresh(): Promise<void> {
    patch((current) => ({ ...current, busy: { kind: "refresh", label: "Refreshing", server: "", startedAt: Date.now() }, notice: undefined }))
    try {
      const servers = await client.getMcpStatus()
      store.setMcpServers(servers)
      patch((current) => ({ ...settleMcpView(current, servers), busy: undefined, notice: { tone: "ok", text: "Refreshed" } }))
    } catch (error) {
      patch((current) => ({ ...current, busy: undefined }))
      notify({ tone: "error", text: `Refresh failed: ${errorText(error)}` })
    }
  }

  async function command(next: McpCommand): Promise<void> {
    if (next.kind === "connect") {
      const outcome = await run("connect", `Connecting ${next.server}`, next.server, (signal) => client.connectMcp(next.server, signal))
      if (!outcome.ok) { notify(outcome.cancelled ? { tone: "info", text: "Cancelled" } : { tone: "error", text: `Connect failed: ${errorText(outcome.error)}` }); return }
      await reload()
      notify({ tone: "ok", text: `Connected ${next.server}` })
      return
    }
    if (next.kind === "disconnect") {
      const outcome = await run("disconnect", `Disconnecting ${next.server}`, next.server, (signal) => client.disconnectMcp(next.server, signal))
      if (!outcome.ok) { notify(outcome.cancelled ? { tone: "info", text: "Cancelled" } : { tone: "error", text: `Disconnect failed: ${errorText(outcome.error)}` }); return }
      await reload()
      notify({ tone: "ok", text: `Disconnected ${next.server}` })
      return
    }
    // auth
    const outcome = await run("auth", `Starting login for ${next.server}`, next.server, (signal) => client.startMcpAuth(next.server, signal))
    if (!outcome.ok) { notify(outcome.cancelled ? { tone: "info", text: "Cancelled" } : { tone: "error", text: `Login failed: ${errorText(outcome.error)}` }); return }
    const url = (outcome.value as { authorizationUrl?: string }).authorizationUrl ?? ""
    if (!url) { notify({ tone: "error", text: "The server did not return an authorization URL" }); return }
    const copied = copyText(url)
    patch((current) => ({ ...current, notice: undefined, auth: { server: next.server, url, code: "" } }))
    if (!copied) notify({ tone: "info", text: "Could not copy the URL: copy it by hand" })
  }

  async function completeAuth(server: string, code: string): Promise<void> {
    const outcome = await run("authComplete", `Completing login for ${server}`, server, (signal) => client.completeMcpAuth(server, code, signal))
    patch((current) => ({ ...current, auth: undefined }))
    if (!outcome.ok) { notify(outcome.cancelled ? { tone: "info", text: "Cancelled" } : { tone: "error", text: `Login failed: ${errorText(outcome.error)}` }); return }
    await reload()
    notify({ tone: "ok", text: `Logged in to ${server}` })
  }

  function key(pressed: KeyLike): void {
    const current = view()
    if (!current) return
    const outcome = mcpViewKey(current, pressed, store.state.mcpServers)
    switch (outcome.type) {
      case "update": store.setMcpView(outcome.view); return
      case "close": close(); return
      case "cancelBusy": abort?.abort(); return
      case "refresh": void refresh(); return
      case "command": void command(outcome.command); return
      case "completeAuth": void completeAuth(outcome.server, outcome.code); return
    }
  }

  return { open, close, key, dispose: () => { abort?.abort() } }
}

export type McpController = ReturnType<typeof createMcpController>
