/**
 * The full-screen Skills view (`/skills`; docs/tui.md "Skills view"). The
 * state and keys are state/skills.ts, the calls app/skills.ts.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, Match, onCleanup, Show, Switch } from "solid-js"
import { useApp } from "../app/context"
import { keyboardOwner } from "../state/focus"
import { wrapLineCount } from "../state/layout"
import { pickerWindow } from "../state/picker"
import { groupRule, skillBodyLines, skillHeaderLine, skillLine, skillLocationText, skillsCountLine, skillSourceText, skillsViewHint, skillsViewLines, type SkillsBusy, type SkillsNotice, type SkillsViewLine, type SkillsViewState } from "../state/skills"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: SkillsNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.success : colors.fg
}

function BusyLine(props: { busy: SkillsBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} flexShrink={0} wrapMode="none">
      <span style={{ fg: colors.activity }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
      <span style={{ fg: colors.muted }}>{" · Esc cancels"}</span>
    </text>
  )
}

export function SkillsView() {
  const { store, ui } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.skillsView}>
      {(open: () => SkillsViewState) => {
        const contentWidth = () => Math.max(20, size().width - 4)
        // The two-column marker prefix (`▸ `).
        const lineWidth = () => Math.max(18, contentWidth() - 2)
        const detail = () => open().screen === "detail"
        const skills = () => store.state.skills
        const skill = () => skills().find((row) => row.name === open().skill)
        const scope = () => store.state.projects.find((row) => row.id === store.state.activeProjectId)?.name ?? store.state.selected?.workdir ?? "global"
        const footerRows = () => {
          let rows = wrapLineCount(skillsViewHint(open()), contentWidth())
          if (open().busy) rows += 1
          const notice = open().notice
          if (notice) rows += wrapLineCount(notice.text, contentWidth())
          if (open().filtering || open().filter) rows += 1
          return rows
        }
        const diagnostics = () => store.state.skillDiagnostics
        const lines = () => skillsViewLines(open(), skills())
        const shown = (): SkillsViewLine[] => {
          const all = lines()
          const at = Math.max(0, all.findIndex((line) => line.kind === "skill" && line.skill.name === open().skill))
          // border (2), the count line, the header, the diagnostics, and the footer.
          const visible = Math.max(3, size().height - 4 - Math.min(diagnostics().length, 3) - footerRows())
          const window = pickerWindow(all.length, at, visible)
          return all.slice(window.start, window.end)
        }
        const body = () => skillBodyLines(skill()?.content ?? "", contentWidth())
        const bodyWindow = () => {
          // border (2), name, file, description, rule, and the footer.
          const description = wrapLineCount(skill()?.description ?? "", contentWidth())
          const visible = Math.max(1, size().height - 5 - description - footerRows())
          const start = Math.max(0, Math.min(open().scroll, Math.max(0, body().length - visible)))
          return { start, end: Math.min(body().length, start + visible), total: body().length }
        }
        const title = () => detail() ? `Skills › ${open().skill ?? ""}` : "Skills"
        const empty = () => open().filter ? "No skill matches the filter" : "No skills · add .hya/skills/<name>/SKILL.md (docs/skills.md)"
        return (
          <box
            position="absolute"
            top={0}
            left={0}
            width="100%"
            height="100%"
            zIndex={50}
            border
            borderColor={keyboardOwner(store.state, ui.command?.active() ?? false) === "skills" ? colors.focus : colors.border}
            titleColor={colors.heading} title={title()}
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <Show
              when={!detail()}
              fallback={
                <Show when={skill()}>
                  {(row) => (
                    <>
                      <text height={1} flexShrink={0} wrapMode="none">
                        <b style={{ fg: colors.heading }}>{row().name}</b>
                        <span style={{ fg: colors.muted }}>{` · ${skillSourceText(row())}${row().disableModelInvocation ? " · only /skill:" + row().name : ""}`}</span>
                      </text>
                      <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{skillLocationText(row())}</text>
                      <text width="100%" flexShrink={0} wrapMode="word" fg={colors.fg}>{row().description ?? ""}</text>
                      <text height={1} flexShrink={0} wrapMode="none" fg={colors.border}>{"─".repeat(contentWidth())}</text>
                      <box flexGrow={1} flexDirection="column">
                        <For each={body().slice(bodyWindow().start, bodyWindow().end)}>
                          {(line) => <text height={1} wrapMode="none" fg={colors.fg}>{line || " "}</text>}
                        </For>
                        <Show when={bodyWindow().end < bodyWindow().total}>
                          <text height={1} wrapMode="none" fg={colors.muted}>{`↓ ${bodyWindow().total - bodyWindow().end} more lines`}</text>
                        </Show>
                      </box>
                    </>
                  )}
                </Show>
              }
            >
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.fg}>{`${skillsCountLine(skills(), diagnostics())} · ${scope()} · /reload-skills applies edits to this session`}</text>
              <For each={diagnostics().slice(0, 3)}>
                {(diagnostic) => <text height={1} flexShrink={0} wrapMode="none" fg={colors.warning}>{`! ${diagnostic.message}`}</text>}
              </For>
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{`  ${skillHeaderLine(lineWidth(), skills())}`}</text>
              <box flexGrow={1} flexDirection="column">
                <For each={shown()}>
                  {(line) => (
                    <Switch>
                      <Match when={line.kind === "divider" && line}>
                        {(divider) => <text height={1} wrapMode="none" fg={colors.muted}>{groupRule(divider().title, contentWidth())}</text>}
                      </Match>
                      <Match when={line.kind === "skill" && line}>
                        {(row) => {
                          const on = () => row().skill.name === open().skill
                          return (
                            <text height={1} wrapMode="none">
                              <span style={{ fg: colors.focus }}>{on() ? "▸ " : "  "}</span>
                              <span style={{ fg: on() ? colors.focus : colors.fg }}>{skillLine(row().skill, lineWidth(), skills())}</span>
                            </text>
                          )
                        }}
                      </Match>
                    </Switch>
                  )}
                </For>
                <Show when={lines().length === 0}>
                  <text height={1} wrapMode="none" fg={colors.muted}>{`  ${empty()}`}</text>
                </Show>
              </box>
            </Show>
            <Show when={open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" flexShrink={0} wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <Show when={open().filtering || open().filter}>
              <text height={1} flexShrink={0} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().filter}</span>
                <span style={{ fg: colors.focus }}>{open().filtering ? "▏" : ""}</span>
              </text>
            </Show>
            <text width="100%" flexShrink={0} wrapMode="word" fg={colors.muted}>{skillsViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
