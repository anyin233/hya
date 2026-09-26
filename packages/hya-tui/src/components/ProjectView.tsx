/**
 * The full-screen Project view (`/project`, `/projects`; docs/tui.md
 * "Projects"), the RulesView pattern. The state and keys are
 * state/projectView.ts, the calls app/projectView.ts; keys reach it through
 * the composer's handler the same way the Saved Rules view does.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { pickerWindow } from "../state/picker"
import { projectViewHint, type ProjectViewBusy, type ProjectViewNotice, type ProjectViewState } from "../state/projectView"
import { truncate } from "../state/format"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: ProjectViewNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: ProjectViewBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} wrapMode="none">
      <span style={{ fg: colors.accent }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
    </text>
  )
}

export function ProjectView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.projectView}>
      {(open: () => ProjectViewState) => {
        const width = () => Math.max(20, size().width - 6)
        const projects = () => store.state.projects
        const shown = () => {
          const all = projects()
          const at = Math.max(0, all.findIndex((row) => row.id === open().highlighted))
          const visible = Math.max(3, size().height - 12)
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const confirmed = () => projects().find((row) => row.id === open().confirm)
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
            title="Projects"
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none" fg={colors.fg}>{`${projects().length} project${projects().length === 1 ? "" : "s"}`}</text>
            <box flexGrow={1} flexDirection="column">
              <For each={shown()}>
                {(project) => {
                  const on = () => project.id === open().highlighted
                  const active = () => project.id === store.state.activeProjectId
                  return (
                    <text height={1} wrapMode="none">
                      <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                      <span style={{ fg: project.busy ? colors.warning : colors.fg }}>{project.busy ? "● " : "  "}</span>
                      <span style={{ fg: active() ? colors.accent : colors.fg }}>{truncate(project.name, Math.max(1, Math.floor(width() * 0.4)))}</span>
                      <span style={{ fg: colors.muted }}>{`  ${(project.sessionCount ?? 0)} session${(project.sessionCount ?? 0) === 1 ? "" : "s"} · ${truncate(project.roots[0] ?? "", Math.max(1, Math.floor(width() * 0.4)))}`}</span>
                    </text>
                  )
                }}
              </For>
              <Show when={projects().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{"  No projects yet · n creates one"}</text>
              </Show>
            </box>
            <Show when={open().create}>
              {(flow) => (
                <text width="100%" wrapMode="word">
                  <span style={{ fg: colors.muted }}>{flow().step === "name" ? "New project name " : `Root ${flow().roots.length + 1} (${flow().roots.length ? "Enter empty to finish" : "primary"}) `}</span>
                  <span style={{ fg: colors.fg }}>{flow().input}</span>
                  <span style={{ fg: colors.accent }}>▏</span>
                  <Show when={flow().roots.length > 0}>
                    <span style={{ fg: colors.muted }}>{`  · roots so far: ${flow().roots.join(", ")}`}</span>
                  </Show>
                </text>
              )}
            </Show>
            <Show when={open().rename}>
              {(flow) => (
                <text width="100%" wrapMode="word">
                  <span style={{ fg: colors.muted }}>Rename to </span>
                  <span style={{ fg: colors.fg }}>{flow().input}</span>
                  <span style={{ fg: colors.accent }}>▏</span>
                </text>
              )}
            </Show>
            <Show when={open().editRoots}>
              {(flow) => (
                <box width="100%" flexDirection="column">
                  <text width="100%" wrapMode="none" fg={colors.muted}>{"Roots (first = primary):"}</text>
                  <For each={flow().roots}>
                    {(root, index) => (
                      <text height={1} wrapMode="none">
                        <span style={{ fg: colors.accent }}>{index() === flow().selected ? "▸ " : "  "}</span>
                        <span style={{ fg: index() === 0 ? colors.accent : colors.fg }}>{index() === 0 ? `${root} (primary)` : root}</span>
                      </text>
                    )}
                  </For>
                  <Show when={flow().adding}>
                    <text width="100%" wrapMode="word">
                      <span style={{ fg: colors.muted }}>Add root </span>
                      <span style={{ fg: colors.fg }}>{flow().input}</span>
                      <span style={{ fg: colors.accent }}>▏</span>
                    </text>
                  </Show>
                </box>
              )}
            </Show>
            <Show when={confirmed()}>
              {(project) => <text width="100%" wrapMode="word" fg={colors.warning}>{`Delete ${project().name}? Enter confirms · Esc cancels`}</text>}
            </Show>
            <Show when={open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <text width="100%" wrapMode="word" fg={colors.muted}>{projectViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
