/**
 * The full-screen Diff view (`/diff`; docs/tui.md "Diff view"). The state
 * and keys are state/diff.ts, the calls app/diff.ts. The open file's body
 * scrolls in a native `<scrollbox>` (mouse wheel included); `ui.diff`
 * (app/context.ts) exposes it to the key handler the same way the
 * transcript exposes `ui.transcript`.
 */
import type { ScrollBoxRenderable } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { pageStep } from "../state/scroll"
import { currentDiffFile, diffViewHint, fileLine, type DiffBusy, type DiffNotice, type DiffViewState } from "../state/diff"
import { colors, diffColors } from "../theme"
import type { Tone } from "../state/tools"
import { useSpinner } from "./Spinner"

function noticeColor(notice: DiffNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function toneColor(tone: Tone): string {
  switch (tone) {
    case "add": return diffColors.add
    case "remove": return diffColors.remove
    case "hunk": return diffColors.hunk
    case "error": return colors.error
    default: return diffColors.context
  }
}

function BusyLine(props: { busy: DiffBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} flexShrink={0} wrapMode="none">
      <span style={{ fg: colors.accent }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
      <span style={{ fg: colors.muted }}>{" · Esc cancels"}</span>
    </text>
  )
}

export function DiffView() {
  const { store, ui } = useApp()
  const size = useTerminalDimensions()
  let scroll: ScrollBoxRenderable | undefined
  ui.diff = {
    line(direction) { if (scroll) scroll.scrollBy(direction) },
    page(direction) { if (scroll) scroll.scrollBy(direction * pageStep(scroll.viewport.height)) },
    top() { if (scroll) scroll.scrollTop = 0 },
    bottom() { if (scroll) scroll.scrollTop = scroll.scrollHeight },
  }
  onCleanup(() => { if (ui.diff) ui.diff = undefined })
  return (
    <Show when={store.state.diffView}>
      {(open: () => DiffViewState) => {
        const lineWidth = () => Math.max(20, size().width - 6)
        const fileListWidth = () => Math.min(36, Math.max(16, Math.floor(size().width * 0.28)))
        const file = () => currentDiffFile(open())
        const empty = () => (store.state.gitBranch ? "No changes" : "Not a git repository")
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
            title={file() ? `Diff › ${file()!.path}` : "Diff"}
            backgroundColor={colors.bg}
            flexDirection="row"
          >
            <box width={fileListWidth()} flexShrink={0} flexDirection="column" paddingX={1} border borderColor={colors.border}>
              <text height={1} wrapMode="none" fg={colors.fg}>{`${open().files.length} file${open().files.length === 1 ? "" : "s"} changed`}</text>
              <box flexGrow={1} flexDirection="column">
                <For each={open().files}>
                  {(row) => (
                    <text height={1} wrapMode="none" fg={row.path === open().current ? colors.accent : colors.fg}>
                      {fileLine(row, row.path === open().current, fileListWidth() - 4)}
                    </text>
                  )}
                </For>
                <Show when={open().files.length === 0}>
                  <text height={1} wrapMode="none" fg={colors.muted}>{empty()}</text>
                </Show>
              </box>
            </box>
            <box flexGrow={1} flexBasis={0} flexDirection="column" paddingX={1}>
              <scrollbox
                ref={(element: ScrollBoxRenderable) => (scroll = element)}
                width="100%"
                flexGrow={1}
                verticalScrollbarOptions={{ trackOptions: { backgroundColor: colors.bg, foregroundColor: colors.border } }}
              >
                <Show when={file()} fallback={<text width="100%" wrapMode="word" fg={colors.muted}>{empty()}</text>}>
                  {(shown) => (
                    <For each={shown().lines}>
                      {(line) => <text width={lineWidth()} wrapMode="none" fg={toneColor(line.tone)}>{line.text}</text>}
                    </For>
                  )}
                </Show>
              </scrollbox>
              {/* The hint keeps its rows (flexShrink 0): otherwise Yoga shrinks it to
                  zero height beside the overflowing scrollbox, and it draws over the
                  scrollbox's last row, so End never showed the file's last line. */}
              <Show when={open().busy} fallback={<text width="100%" flexShrink={0} wrapMode="word" fg={open().notice ? noticeColor(open().notice!) : colors.muted}>{diffViewHint(open())}</text>}>
                {(busy) => <BusyLine busy={busy()} />}
              </Show>
            </box>
          </box>
        )
      }}
    </Show>
  )
}
