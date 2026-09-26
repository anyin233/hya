/**
 * The full-screen Saved Rules view (`/rules`; docs/tui.md "Saved Rules").
 * The state and keys are state/rules.ts, the calls app/rules.ts; keys reach
 * it through the composer's handler the same way the Provider View does.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { pickerWindow } from "../state/picker"
import { ruleHeaderLine, ruleLine, rulesViewHint, shownRules, type RulesBusy, type RulesNotice, type RulesViewState } from "../state/rules"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: RulesNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: RulesBusy }) {
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

export function RulesView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.rulesView}>
      {(open: () => RulesViewState) => {
        const lineWidth = () => Math.max(20, size().width - 6)
        const rows = () => shownRules(open(), store.state.savedRules)
        const shown = () => {
          const all = rows()
          const at = Math.max(0, all.findIndex((row) => row.id === open().rule))
          const visible = Math.max(3, size().height - 10)
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const empty = () => open().filter ? "No rule matches the filter" : "No saved permission rules"
        const confirmed = () => rows().find((row) => row.id === open().confirm)
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
            title="Saved Rules"
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none" fg={colors.fg}>{`${rows().length} saved rule${rows().length === 1 ? "" : "s"}`}</text>
            <text height={1} wrapMode="none" fg={colors.muted}>{`  ${ruleHeaderLine(lineWidth())}`}</text>
            <box flexGrow={1} flexDirection="column">
              <For each={shown()}>
                {(row) => {
                  const on = () => row.id === open().rule
                  return (
                    <text height={1} wrapMode="none">
                      <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                      <span style={{ fg: on() ? colors.accent : colors.fg }}>{ruleLine(row, lineWidth(), store.state.projects)}</span>
                    </text>
                  )
                }}
              </For>
              <Show when={rows().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{`  ${empty()}`}</text>
              </Show>
            </box>
            <Show when={confirmed()}>
              {(rule) => <text width="100%" wrapMode="word" fg={colors.warning}>{`Delete ${rule().tool || "*"} ${rule().pattern || "*"}? Enter confirms · Esc cancels`}</text>}
            </Show>
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
            <text width="100%" wrapMode="word" fg={colors.muted}>{rulesViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
