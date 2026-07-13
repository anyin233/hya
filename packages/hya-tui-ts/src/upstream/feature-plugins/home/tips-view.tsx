import type { TuiPluginApi } from "@opencode-ai/plugin/tui"
import { createMemo, For } from "solid-js"

import { useTheme } from "../../context/theme"

type TipPart = { text: string; highlight: boolean }

function parse(tip: string): TipPart[] {
  const parts: TipPart[] = []
  const regex = /\{highlight\}(.*?)\{\/highlight\}/g
  let index = 0
  for (const match of tip.matchAll(regex)) {
    const start = match.index ?? 0
    if (start > index) parts.push({ text: tip.slice(index, start), highlight: false })
    parts.push({ text: match[1], highlight: true })
    index = start + match[0].length
  }
  if (index < tip.length) parts.push({ text: tip.slice(index), highlight: false })
  return parts
}

const TIPS = [
  "Type {highlight}@{/highlight} followed by a filename to attach files",
  "Start a message with {highlight}!{/highlight} to run a shell command",
  "Use {highlight}/undo{/highlight} and {highlight}/redo{/highlight} to revert or restore changes",
  "Use {highlight}/models{/highlight} to switch models",
  "Use {highlight}/sessions{/highlight} to resume a session",
  "Use {highlight}/compact{/highlight} to summarize a long session",
  "Use {highlight}/help{/highlight} to show available commands",
]
const NO_MODELS_TIP = "Configure a model to start coding"

export function Tips(_props: { api: TuiPluginApi; connected?: boolean }) {
  const theme = useTheme().theme
  const parts = createMemo(() => parse(TIPS[Math.floor(Math.random() * TIPS.length)] ?? NO_MODELS_TIP))

  return (
    <box flexDirection="row" maxWidth="100%">
      <text flexShrink={0} style={{ fg: theme.warning }}>
        ● Tip{" "}
      </text>
      <text flexShrink={1} wrapMode="word">
        <For each={parts()}>
          {(part) => <span style={{ fg: part.highlight ? theme.text : theme.textMuted }}>{part.text}</span>}
        </For>
      </text>
    </box>
  )
}
