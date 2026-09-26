/**
 * The full-screen MCP view (`/mcp`; docs/tui.md "MCP servers"). The state
 * and keys are state/mcp.ts, the calls app/mcp.ts.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { pickerWindow } from "../state/picker"
import { mcpToolIndex, mcpToolLabel, mcpToolWindow, mcpViewHint, serverHeaderLine, serverLine, serverStateText, shownServers, type McpBusy, type McpNotice, type McpViewState } from "../state/mcp"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: McpNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: McpBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} flexShrink={0} wrapMode="none">
      <span style={{ fg: colors.accent }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
      <span style={{ fg: colors.muted }}>{" · Esc cancels"}</span>
    </text>
  )
}

export function McpView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.mcpView}>
      {(open: () => McpViewState) => {
        const lineWidth = () => Math.max(20, size().width - 6)
        const detail = () => open().screen === "detail"
        const server = () => store.state.mcpServers.find((row) => row.name === open().server)
        const rows = () => shownServers(open(), store.state.mcpServers)
        const shown = () => {
          const all = rows()
          const at = Math.max(0, all.findIndex((row) => row.name === open().server))
          const visible = Math.max(3, size().height - 10)
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const tools = () => server()?.tools ?? []
        const toolWindow = () => mcpToolWindow(tools().length, mcpToolIndex(open()), Math.max(1, size().height - 12))
        const shownTools = () => {
          const window = toolWindow()
          return tools().slice(window.start, window.end).map((tool, offset) => ({ tool, at: window.start + offset }))
        }
        const title = () => detail() ? `MCP › ${open().server ?? ""}` : "MCP servers"
        const empty = () => detail() ? "No tools" : (open().filter ? "No server matches the filter" : "No MCP servers configured")
        return (
          <box
            position="absolute"
            top={0}
            left={0}
            width="100%"
            height="100%"
            zIndex={50}
            border
            borderColor={colors.accent}
            title={title()}
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <Show when={!detail()} fallback={<text height={1} flexShrink={0} wrapMode="none" fg={colors.fg}>{server() ? `${server()!.name} · ${serverStateText(server()!.state)}` : ""}</text>}>
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.fg}>{`${store.state.mcpServers.length} configured server${store.state.mcpServers.length === 1 ? "" : "s"}`}</text>
            </Show>
            <Show when={!detail()}>
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{`  ${serverHeaderLine(lineWidth())}`}</text>
            </Show>
            <box flexGrow={1} flexDirection="column">
              <Show
                when={!detail()}
                fallback={
                  <>
                    <Show when={toolWindow().moreAbove > 0}>
                      <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{`  ↑ ${toolWindow().moreAbove} more`}</text>
                    </Show>
                    <For each={shownTools()}>
                      {(item) => {
                        const on = () => item.at === mcpToolIndex(open())
                        return (
                          <text height={1} wrapMode="none">
                            <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                            <span style={{ fg: on() ? colors.accent : colors.fg }}>{mcpToolLabel(open().server ?? "", item.tool)}</span>
                          </text>
                        )
                      }}
                    </For>
                    <Show when={toolWindow().moreBelow > 0}>
                      <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{`  ↓ ${toolWindow().moreBelow} more`}</text>
                    </Show>
                  </>
                }
              >
                <For each={shown()}>
                  {(row) => {
                    const on = () => row.name === open().server
                    return (
                      <text height={1} wrapMode="none">
                        <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                        <span style={{ fg: on() ? colors.accent : colors.fg }}>{serverLine(row, lineWidth())}</span>
                      </text>
                    )
                  }}
                </For>
              </Show>
              <Show when={detail() ? (server()?.tools?.length ?? 0) === 0 : rows().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{`  ${empty()}`}</text>
              </Show>
            </box>
            <Show when={open().auth}>
              {(auth) => (
                <box flexDirection="column" flexShrink={0}>
                  <text width="100%" wrapMode="word" fg={colors.accent}>{`Open ${auth().url} (copied to the clipboard)`}</text>
                  <text height={1} wrapMode="none">
                    <span style={{ fg: colors.muted }}>Code </span>
                    <span style={{ fg: colors.fg }}>{auth().code}</span>
                    <span style={{ fg: colors.accent }}>▏</span>
                  </text>
                </box>
              )}
            </Show>
            <Show when={!open().auth && open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" flexShrink={0} wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <Show when={open().filtering || open().filter}>
              <text height={1} flexShrink={0} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().filter}</span>
                <span style={{ fg: colors.accent }}>{open().filtering ? "▏" : ""}</span>
              </text>
            </Show>
            <text width="100%" flexShrink={0} wrapMode="word" fg={colors.muted}>{mcpViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
