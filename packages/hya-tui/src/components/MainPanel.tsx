import type { ScrollBoxRenderable } from "@opentui/core"
import { createEffect } from "solid-js"
import { useApp } from "../app/context"
import { mainContent, mainTitle } from "../state/format"
import { colors } from "../theme"
import { Panel } from "./Panel"

/** Center panel: the transcript in the chat view, otherwise the current view's text. */
export function MainPanel() {
  const { store } = useApp()
  let scroll: ScrollBoxRenderable | undefined
  const content = () => mainContent(store.state)
  createEffect(() => {
    content()
    if (store.state.view === "chat" && scroll) scroll.scrollTop = scroll.scrollHeight
  })
  return (
    <Panel
      title={mainTitle(store.state.view)}
      text={content()}
      background={colors.bg}
      sticky
      scrollRef={(element) => (scroll = element)}
    />
  )
}
