/**
 * The full-screen Agents view (`/agent`; docs/tui.md "Agents view"). The
 * state and keys are state/agentsView.ts, the calls app/agentsView.ts (`m`
 * and `t` open the shared model and effort pickers over this view).
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, Match, onCleanup, Show, Switch } from "solid-js"
import { useApp } from "../app/context"
import { agentHeaderLine, agentLine, agentsViewHint, agentsViewLines, sectionRule, type AgentsViewBusy, type AgentsViewLine, type AgentsViewNotice, type AgentsViewState } from "../state/agentsView"
import { pickerWindow } from "../state/picker"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: AgentsViewNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: AgentsViewBusy }) {
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

export function AgentsView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.agentsView}>
      {(open: () => AgentsViewState) => {
        // Border, padding, and the three-column marker prefix (`▸● `).
        const lineWidth = () => Math.max(20, size().width - 7)
        const lines = () => agentsViewLines(open(), store.state.agentModelRows)
        const current = () => store.state.selected?.agent ?? store.state.pendingAgent
        const shown = (): AgentsViewLine[] => {
          const all = lines()
          const at = Math.max(0, all.findIndex((line) => line.kind === "agent" && line.row.agentId === open().agent))
          const visible = Math.max(3, size().height - 10)
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const count = () => store.state.agentModelRows.length
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
            title="Agents"
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none" fg={colors.fg}>{`${count()} agent${count() === 1 ? "" : "s"} · ● runs this session`}</text>
            <text height={1} wrapMode="none" fg={colors.muted}>{`   ${agentHeaderLine(lineWidth())}`}</text>
            <box flexGrow={1} flexDirection="column">
              <For each={shown()}>
                {(line) => (
                  <Switch>
                    <Match when={line.kind === "divider" && line}>
                      {(divider) => <text height={1} wrapMode="none" fg={colors.muted}>{sectionRule(divider().title, lineWidth() + 3)}</text>}
                    </Match>
                    <Match when={line.kind === "agent" && line}>
                      {(agent) => {
                        const on = () => agent().row.agentId === open().agent
                        return (
                          <text height={1} wrapMode="none">
                            <span style={{ fg: colors.accent }}>{on() ? "▸" : " "}</span>
                            <span style={{ fg: colors.accent }}>{agent().row.agentId === current() ? "● " : "  "}</span>
                            <span style={{ fg: on() ? colors.accent : colors.fg }}>{agentLine(agent().row, lineWidth())}</span>
                          </text>
                        )
                      }}
                    </Match>
                  </Switch>
                )}
              </For>
              <Show when={lines().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{`   ${empty()}`}</text>
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
            <text width="100%" wrapMode="word" fg={colors.muted}>{agentsViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
