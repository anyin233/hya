/**
 * The start of the transcript (ADR-0036 `startup` component,
 * state/startup.ts): `[Config]` / `[Layout]` notices, then what was loaded —
 * `[Context]`, `[Skills]`, `[Commands]`, `[MCP]`, `[Bundles]`,
 * `[Extensions]` — one line each (`resources = "compact"`) or grouped by
 * source (`"full"`). Problems are in the warning color and stay with
 * `quiet = true`. It scrolls away with the history and outlives session
 * switches; a section appears once its data arrived.
 */
import type { BoxRenderable } from "@opentui/core"
import { createMemo, createSignal, For, Show } from "solid-js"
import { useApp } from "../app/context"
import { startupInput } from "../app/startupInput"
import { startupLines, type LineTone } from "../state/startup"
import { colors } from "../theme"

const toneColor = (tone: LineTone): string => tone === "warning" ? colors.warning : tone === "heading" ? colors.heading : colors.muted

export function StartupBlock() {
  const { store } = useApp()
  const [width, setWidth] = createSignal(80)
  let box: BoxRenderable | undefined
  const lines = createMemo(() => {
    const settings = store.state.structure.startup
    const notices = store.state.startupNotices
    // Off: only the notices about the configuration itself.
    if (!settings.enabled) return startupLines({ notices }, { ...settings, sections: [] }, width())
    return startupLines(startupInput(store.state), settings, width())
  })
  return (
    <Show when={lines().length > 0}>
      <box ref={(element: BoxRenderable) => (box = element)} width="100%" flexDirection="column" flexShrink={0} paddingBottom={1}
        onSizeChange={() => { if (box && box.width > 0) setWidth(box.width) }}>
        <For each={lines()}>
          {(line) => (
            <text width="100%" wrapMode="word" fg={toneColor(line.tone)}>
              <Show when={line.label && line.text.startsWith(line.label)} fallback={line.text}>
                <span style={{ fg: colors.heading }}>{line.label}</span>{line.text.slice(line.label!.length)}
              </Show>
            </text>
          )}
        </For>
      </box>
    </Show>
  )
}
