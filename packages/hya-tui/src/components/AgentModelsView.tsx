/**
 * The full-screen Agent Models view (`/agent-models`; docs/tui.md "Agent
 * Models"). The state and keys are state/agentModels.ts, the calls
 * app/agentModels.ts (Enter opens the shared model picker over this view).
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { agentModelHeaderLine, agentModelLine, agentModelsViewHint, shownAgentModels, type AgentModelsBusy, type AgentModelsNotice, type AgentModelsViewState } from "../state/agentModels"
import { pickerWindow } from "../state/picker"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: AgentModelsNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: AgentModelsBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} wrapMode="none">
      <span style={{ fg: colors.accent }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
      <span style={{ fg: colors.muted }}>{" · Esc cancels"}</span>
    </text>
  )
}

export function AgentModelsView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.agentModelsView}>
      {(open: () => AgentModelsViewState) => {
        const lineWidth = () => Math.max(20, size().width - 6)
        const rows = () => shownAgentModels(open(), store.state.agentModelRows)
        const shown = () => {
          const all = rows()
          const at = Math.max(0, all.findIndex((row) => row.agentId === open().agent))
          const visible = Math.max(3, size().height - 10)
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const empty = () => open().filter ? "No agent matches the filter" : "No agents"
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
            title="Agent Models"
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none" fg={colors.fg}>{`${store.state.agentModelRows.length} agent${store.state.agentModelRows.length === 1 ? "" : "s"}`}</text>
            <text height={1} wrapMode="none" fg={colors.muted}>{`  ${agentModelHeaderLine(lineWidth())}`}</text>
            <box flexGrow={1} flexDirection="column">
              <For each={shown()}>
                {(row) => {
                  const on = () => row.agentId === open().agent
                  return (
                    <text height={1} wrapMode="none">
                      <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                      <span style={{ fg: on() ? colors.accent : colors.fg }}>{agentModelLine(row, lineWidth())}</span>
                    </text>
                  )
                }}
              </For>
              <Show when={rows().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{`  ${empty()}`}</text>
              </Show>
            </box>
            <Show when={open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <Show when={open().filtering || open().filter}>
              <text height={1} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().filter}</span>
                <span style={{ fg: colors.accent }}>{open().filtering ? "▏" : ""}</span>
              </text>
            </Show>
            <text width="100%" wrapMode="word" fg={colors.muted}>{agentModelsViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
