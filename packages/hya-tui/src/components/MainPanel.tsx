import type { ScrollBoxRenderable } from "@opentui/core"
import { createEffect, createMemo } from "solid-js"
import { useApp } from "../app/context"
import { mainContent, mainTitle, queuedText } from "../state/format"
import { colors } from "../theme"
import { Panel } from "./Panel"

/**
 * Center panel: the transcript (projection + streaming overlay) and the dimmed
 * queued prompts in the chat view, otherwise the current view's text.
 */
export function MainPanel() {
  const { store } = useApp()
  let scroll: ScrollBoxRenderable | undefined
  // Memoized: the effect and the panel read the same text; compute it once per change.
  const content = createMemo(() => mainContent(store.state))
  const queued = createMemo(() => queuedText(store.state))
  createEffect(() => {
    content()
    queued()
    if (store.state.view === "chat" && scroll) scroll.scrollTop = scroll.scrollHeight
  })
  return (
    <Panel
      title={mainTitle(store.state.view)}
      text={content()}
      trailer={queued()}
      background={colors.bg}
      sticky
      scrollRef={(element) => (scroll = element)}
    />
  )
}
