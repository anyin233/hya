/**
 * The chat transcript: one `MessageItem` per message (keyed by message id, so
 * streaming updates only the message it touches), in a scrollbox that sticks
 * to the bottom while the view is at the bottom.
 *
 * Scrolling: PgUp/PgDn, Ctrl+Home/Ctrl+End (Home/End with an empty
 * composer), and the mouse wheel. The key handler reaches the scroll actions
 * through `ui.transcript` (app/context.ts). When new content arrives below a
 * scrolled-up view, a `↓ New messages below` hint shows until the bottom is
 * reached again (state/scroll.ts).
 */
import type { ScrollBoxRenderable } from "@opentui/core"
import { useRenderer } from "@opentui/solid"
import { createEffect, createMemo, createSignal, on, onCleanup, Show } from "solid-js"
import { useApp, type TranscriptScroller } from "../app/context"
import { mainContent } from "../state/format"
import { transcriptViews } from "../state/messages"
import { pageStep, ScrollFollow } from "../state/scroll"
import { colors } from "../theme"
import { KeyedFor, MessageItem } from "./MessageView"

export function Transcript() {
  const { store, ui } = useApp()
  const renderer = useRenderer()
  const views = createMemo(() => transcriptViews(store.state))
  const empty = createMemo(() => mainContent(store.state))
  const [unseen, setUnseen] = createSignal(false)
  const follow = new ScrollFollow()
  let scroll: ScrollBoxRenderable | undefined

  const scroller: TranscriptScroller = {
    page(direction) {
      if (scroll) scroll.scrollBy(direction * pageStep(scroll.viewport.height))
    },
    top() {
      if (scroll) scroll.scrollTop = 0
    },
    bottom() {
      // At the maximum offset the scrollbox re-arms its sticky bottom.
      if (scroll) scroll.scrollTop = scroll.scrollHeight
    },
  }
  ui.transcript = scroller

  // Once per rendered frame: raise or clear the "new messages below" hint.
  const onFrame = async (): Promise<void> => {
    if (!scroll || scroll.isDestroyed) return
    const state = follow.observe({ scrollTop: scroll.scrollTop, scrollHeight: scroll.scrollHeight, viewportHeight: scroll.viewport.height })
    if (state.unseen !== unseen()) setUnseen(state.unseen)
  }
  renderer.setFrameCallback(onFrame)
  onCleanup(() => {
    renderer.removeFrameCallback(onFrame)
    if (ui.transcript === scroller) ui.transcript = undefined
  })

  // A submitted prompt jumps to the newest line; another session starts at its bottom.
  createEffect(on(() => store.state.followTick, () => scroller.bottom(), { defer: true }))
  createEffect(on(() => store.state.selected?.id, () => {
    follow.reset()
    setUnseen(false)
    scroller.bottom()
  }, { defer: true }))

  return (
    <box width="100%" flexGrow={1} flexBasis={0} flexDirection="column" backgroundColor={colors.bg}>
      <scrollbox
        ref={(element: ScrollBoxRenderable) => (scroll = element)}
        width="100%"
        flexGrow={1}
        stickyScroll
        stickyStart="bottom"
        paddingLeft={1}
        paddingRight={1}
        paddingTop={1}
        verticalScrollbarOptions={{ trackOptions: { backgroundColor: colors.bg, foregroundColor: colors.border } }}
      >
        <Show when={empty()}>
          <text width="100%" fg={colors.muted}>{empty()}</text>
        </Show>
        <KeyedFor each={views()}>
          {(view, index) => <MessageItem view={view()} first={index() === 0} />}
        </KeyedFor>
      </scrollbox>
      <Show when={unseen()}>
        <box position="absolute" bottom={0} right={2} height={1} backgroundColor={colors.panel} paddingX={1}>
          <text fg={colors.accent} wrapMode="none">↓ New messages below · End jumps</text>
        </box>
      </Show>
    </box>
  )
}
