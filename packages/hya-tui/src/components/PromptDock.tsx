/**
 * The permission / question prompt docked above the status line
 * (state/prompts.ts, docs/tui.md "Permission and question prompts"): the
 * oldest pending ask of the open session's tree, one at a time (`1 of N`).
 * A permission shows its title, who asks, and the waiting call's details
 * (like its tool card), then `1 Allow once`, `2 Always allow` (with what it
 * covers), `3 Deny`. A question shows its header and text, its options,
 * `Other…` (typed into the input), and `Reject`. Keys are handled by the
 * composer (components/Composer.tsx); a click on an option chooses it.
 */
import { Show, For } from "solid-js"
import { useApp } from "../app/context"
import { currentPrompt, type PromptView } from "../state/prompts"
import { colors } from "../theme"
import { toneColor } from "./MessageView"

function hint(view: PromptView, draft: boolean): string {
  const count = view.options.length
  if (draft) {
    return view.kind === "question"
      ? `Enter sends the input as the answer · ${view.id}`
      : `Clear the input to answer with 1-${count} · or /approve ${view.id}`
  }
  return view.kind === "question"
    ? `1-${count} or ↑↓ Enter · type an answer + Enter · Esc rejects · ${view.id}`
    : `1-${count} or ↑↓ Enter · Esc denies · ${view.id}`
}

export function PromptDock() {
  const { store, controller } = useApp()
  const prompt = () => currentPrompt(store.state)
  return (
    <Show when={prompt()}>
      {(shown) => {
        const view = () => shown().view
        const index = () => store.promptIndex(view().id)
        const heading = () => `${view().kind === "question" ? "Question" : "Permission"}${view().total > 1 ? ` · ${view().position + 1} of ${view().total}` : ""}`
        return (
          <box
            width="100%"
            flexShrink={0}
            border
            borderColor={colors.warning}
            title={heading()}
            backgroundColor={colors.panel}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none">
              <b style={{ fg: colors.fg }}>{view().headline}</b>
            </text>
            <text height={1} wrapMode="none" fg={colors.muted}>{`asked by ${view().asker}`}</text>
            <Show when={view().body.length > 0}>
              <box width="100%" flexDirection="column" border={["left"]} borderColor={colors.border} paddingLeft={1}>
                <For each={view().body}>
                  {(line) => <text width="100%" height={1} wrapMode="none" fg={toneColor(line.tone)}>{line.text || " "}</text>}
                </For>
              </box>
            </Show>
            <For each={view().options}>
              {(option, row) => (
                <text height={1} wrapMode="none" onMouseDown={() => controller.answer(shown().interaction, option.choice)}>
                  <span style={{ fg: row() === index() ? colors.accent : colors.fg }}>{`${row() === index() ? "▸" : " "} ${row() + 1}  ${option.label}`}</span>
                  <span style={{ fg: colors.muted }}>{option.detail ? `  ${option.detail}` : ""}</span>
                </text>
              )}
            </For>
            <text height={1} wrapMode="none" fg={colors.muted}>{hint(view(), store.state.draft)}</text>
          </box>
        )
      }}
    </Show>
  )
}
