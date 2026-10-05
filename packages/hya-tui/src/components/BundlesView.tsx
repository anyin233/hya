/**
 * The full-screen Bundles view (`/bundles`; docs/tui.md "Bundles"). The state
 * and keys are state/bundles.ts, the calls app/bundles.ts.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { extensionManager } from "../extensions/manager"
import { bundleDetailLines, bundleHeaderLine, bundleLine, bundleRows, bundlesViewHint, shownBundles, type BundleBusy, type BundleNotice, type BundlePopup, type BundleViewState } from "../state/bundles"
import { keyboardOwner } from "../state/focus"
import { truncate } from "../state/format"
import { wrapLineCount } from "../state/layout"
import { pickerWindow } from "../state/picker"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

function noticeColor(notice: BundleNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

function BusyLine(props: { busy: BundleBusy }) {
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

/** The install or uninstall pop-up's lines. */
function popupLines(popup: BundlePopup): string[] {
  if (popup.kind === "uninstall") return [`Uninstall ${popup.bundleId}${popup.project ? " from the project" : ""}? Enter confirms`]
  if (popup.step === "path") return [`Package (.hyabundle) path: ${popup.path}▏`, ...(popup.error ? [popup.error] : [])]
  return [`Install ${popup.path}`, `Scope: ${popup.project ? "user   [project: this directory's .hya/bundles]" : "[user]   project"}`]
}

export function BundlesView() {
  const { store, ui } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.bundlesView}>
      {(open: () => BundleViewState) => {
        const contentWidth = () => Math.max(1, size().width - 4)
        const lineWidth = () => Math.max(20, size().width - 6)
        const detail = () => open().screen === "detail"
        const rows = () => bundleRows(store.state.bundles, extensionManager.list())
        const shownRows = () => shownBundles(open(), rows())
        const selected = () => rows().find((row) => row.key === open().selected)
        // Rows under the list: pop-up or busy line, notice, filter line, hint (the wrapping ones measured).
        const footerRows = () => {
          let extra = 0
          const popup = open().popup
          if (popup) extra += popupLines(popup).reduce((sum, line) => sum + wrapLineCount(line, contentWidth()), 0)
          else if (open().busy) extra += 1
          const notice = open().notice
          if (notice) extra += wrapLineCount(notice.text, contentWidth())
          if (open().filtering || open().filter) extra += 1
          return extra + wrapLineCount(bundlesViewHint(open()), contentWidth())
        }
        const listWindow = () => {
          const all = shownRows()
          const at = Math.max(0, all.findIndex((row) => row.key === open().selected))
          // border (2) + count line (1) + header (1) + footerRows()
          const window = pickerWindow(all.length, at, Math.max(3, size().height - 4 - footerRows()))
          return all.slice(window.start, window.end)
        }
        const detailLines = () => {
          const row = selected()
          return row ? bundleDetailLines(row).map((line) => truncate(line, lineWidth())) : []
        }
        const title = () => detail() ? `Bundles › ${selected()?.bundle.id ?? ""}` : "Bundles"
        const count = () => {
          const all = rows()
          const enabled = all.filter((row) => row.bundle.enabled).length
          return `${all.length} bundle${all.length === 1 ? "" : "s"} · ${enabled} enabled · ${all.filter((row) => row.bundle.components?.tui).length} with a TUI extension`
        }
        return (
          <box
            position="absolute"
            top={0}
            left={0}
            width="100%"
            height="100%"
            zIndex={50}
            border
            borderColor={keyboardOwner(store.state, ui.command?.active() ?? false) === "bundles" ? colors.accent : colors.border}
            title={title()}
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <Show
              when={!detail()}
              fallback={
                <box flexGrow={1} flexDirection="column">
                  <For each={detailLines()}>{(line) => <text height={1} wrapMode="none" fg={colors.fg}>{line}</text>}</For>
                </box>
              }
            >
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.fg}>{count()}</text>
              <text height={1} flexShrink={0} wrapMode="none" fg={colors.muted}>{`  ${bundleHeaderLine(lineWidth())}`}</text>
              <box flexGrow={1} flexDirection="column">
                <For each={listWindow()}>
                  {(row) => {
                    const on = () => row.key === open().selected
                    const muted = () => row.bundle.state !== "active"
                    return (
                      <text height={1} wrapMode="none">
                        <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                        <span style={{ fg: on() ? colors.accent : muted() ? colors.muted : colors.fg }}>{bundleLine(row, lineWidth())}</span>
                      </text>
                    )
                  }}
                </For>
                <Show when={shownRows().length === 0}>
                  <text height={1} wrapMode="none" fg={colors.muted}>{open().filter ? "  No bundle matches the filter" : "  No bundles"}</text>
                </Show>
              </box>
            </Show>
            <Show when={open().popup}>
              {(popup) => (
                <box flexDirection="column" flexShrink={0}>
                  <For each={popupLines(popup())}>{(line) => <text width="100%" wrapMode="word" fg={colors.accent}>{line}</text>}</For>
                </box>
              )}
            </Show>
            <Show when={!open().popup && open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" flexShrink={0} wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <Show when={open().filtering || open().filter}>
              <text height={1} flexShrink={0} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().filter}</span>
                <span style={{ fg: colors.accent }}>{open().filtering ? "▏" : ""}</span>
              </text>
            </Show>
            <text width="100%" flexShrink={0} wrapMode="word" fg={colors.muted}>{bundlesViewHint(open())}</text>
          </box>
        )
      }}
    </Show>
  )
}
