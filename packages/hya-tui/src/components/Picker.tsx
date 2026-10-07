/**
 * The reusable modal picker (state/picker.ts holds its pure state; the
 * controller's `openPicker(spec)` opens it). A bordered box drawn over the
 * main column near the top: the title, a `Filter` row with the typed text,
 * up to `pickerMaxRows` rows (`▸` marks the highlight, `●` the current
 * value; label, `[tag]`, muted detail), and a hint row. While it is open the
 * composer's editor is unfocused and every key but Ctrl+C goes to the picker
 * (components/Composer.tsx); a click on a row chooses it. Choosing or Esc
 * closes it and the input has the focus again.
 *
 * A row action (S9, C13: `/sessions` F2 rename, Ctrl+D delete) switches the
 * box into a one-line `"rename"` (an editable `New title` row) or
 * `"confirm"` (the confirmation text) mode in place of the list; Esc there
 * returns to the list without closing the picker.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { defaultPickerHint, pickerMaxRows, pickerRows, pickerWindow, type ActivePicker } from "../state/picker"
import { colors } from "../theme"
import { keyboardOwner } from "../state/focus"

/** Widest picker box, in columns. */
const pickerMaxWidth = 96

export function Picker() {
  const { store, controller, ui } = useApp()
  const size = useTerminalDimensions()
  const width = () => store.state.picker?.contextMenu ? Math.max(22, Math.min(38, size().width - 2)) : Math.max(20, Math.min(pickerMaxWidth, size().width - 4))
  const left = () => { const point = store.state.picker?.contextMenu; return point ? Math.max(0, Math.min(point.x, size().width - width())) : Math.max(0, Math.floor((size().width - width()) / 2)) }
  const top = () => { const point = store.state.picker?.contextMenu; return point ? Math.max(0, Math.min(point.y, size().height - 3)) : 2 }
  return (
    <Show when={store.state.picker}>
      {(open: () => ActivePicker) => {
        const rows = () => pickerRows(open())
        // Box chrome: 2 border rows, the filter and hint rows, and the 2-row top offset, plus the composer below.
        const visibleRows = () => Math.max(3, Math.min(open().maxRows ?? pickerMaxRows, size().height - (open().detailPane ? 14 : 10) - (open().columns ? 1 : 0)))
        const highlighted = () => rows()[open().index]
        const shown = () => {
          const window = pickerWindow(rows().length, open().index, visibleRows())
          return rows().slice(window.start, window.end).map((row, offset) => ({ row, at: window.start + offset }))
        }
        const labelWidth = () => Math.min(28, Math.max(0, ...rows().map((row) => Bun.stringWidth(row.label))))
        const pad = (text: string) => text + " ".repeat(Math.max(0, labelWidth() - Bun.stringWidth(text)))
        // Tags line up too, so the details start in one column.
        const tagWidth = () => Math.min(14, Math.max(0, ...rows().map((row) => (row.tag ? Bun.stringWidth(row.tag) + 2 : 0))))
        const padTag = (tag: string | undefined) => {
          const text = tag ? `[${tag}]` : ""
          return text + " ".repeat(Math.max(0, tagWidth() - Bun.stringWidth(text)))
        }
        // Column mode is opt-in; keep traditional picker rows unchanged.
        const shortcutWidth = () => Math.min(26, Math.max(8, ...open().rows.map((row) => Bun.stringWidth(row.shortcut ?? ""))))
        const scopeWidth = () => Math.min(14, Math.max(5, ...open().rows.map((row) => Bun.stringWidth(row.tag ?? ""))))
        // Reserve 4 columns for borders/padding, 4 for selection markers and 4 for gaps.
        const actionWidth = () => Math.max(8, width() - 12 - shortcutWidth() - scopeWidth())
        const column = (text: string, width: number) => {
          let clipped = text
          if (Bun.stringWidth(clipped) > width) {
            clipped = ""
            for (const char of text) {
              if (Bun.stringWidth(clipped + char) > width - 1) break
              clipped += char
            }
            clipped += "…"
          }
          return clipped + " ".repeat(Math.max(0, width - Bun.stringWidth(clipped)))
        }
        return (
          <box
            position="absolute"
            top={top()}
            left={left()}
            width={width()}
            zIndex={100}
            border
            borderColor={keyboardOwner(store.state, ui.command?.active() ?? false) === "picker" ? colors.accent : colors.border}
            title={open().contextMenu ? undefined : open().title}
            backgroundColor={colors.panel}
            flexDirection="column"
            paddingX={1}
          >
            <Show
              when={open().mode === "list" || !open().mode}
              fallback={
                <>
                  <Show when={open().mode === "rename"}>
                    <text height={1} wrapMode="none">
                      <span style={{ fg: colors.muted }}>New title </span>
                      <span style={{ fg: colors.fg }}>{open().editValue ?? ""}</span>
                      <span style={{ fg: colors.accent }}>▏</span>
                    </text>
                    <text height={1} wrapMode="none" fg={colors.muted}>Enter renames · Esc cancels</text>
                  </Show>
                  <Show when={open().mode === "confirm"}>
                    <text height={1} wrapMode="none" fg={colors.fg}>{open().confirmText ?? ""}</text>
                  </Show>
                </>
              }
            >
              <Show when={!open().contextMenu}>
              <text height={1} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().query}</span>
                <span style={{ fg: colors.accent }}>▏</span>
                <span style={{ fg: colors.muted }}>{`  ${rows().length} of ${open().rows.length}`}</span>
              </text>
              <Show when={open().columns}>
                <text height={1} wrapMode="none" fg={colors.muted}>
                  {`    ${column(open().columns!.shortcut, shortcutWidth())}  ${column(open().columns!.label, actionWidth())}  ${column(open().columns!.tag, scopeWidth())}`}
                </text>
              </Show>
              </Show>
              <For each={shown()}>
                {(item) => {
                  const highlighted = () => item.at === open().index
                  return (
                    <text height={1} wrapMode="none" onMouseDown={(event) => { if (event.button === 0) controller.choosePickerRow(item.row) }}>
                      <span style={{ fg: highlighted() ? colors.accent : colors.fg }}>{`${highlighted() ? "▸" : " "} `}</span>
                      <span style={{ fg: colors.accent }}>{item.row.current ? "● " : "  "}</span>
                      <Show when={open().columns} fallback={
                        <>
                          <span style={{ fg: highlighted() ? colors.accent : colors.fg }}>{pad(item.row.label)}</span>
                          <span style={{ fg: colors.muted }}>{tagWidth() ? `  ${padTag(item.row.tag)}` : ""}</span>
                          <span style={{ fg: colors.muted }}>{item.row.detail ? `  ${item.row.detail}` : ""}</span>
                        </>
                      }>
                        <span style={{ fg: colors.accent }}>{column(item.row.shortcut ?? "", shortcutWidth())}</span>
                        <span style={{ fg: highlighted() ? colors.accent : colors.fg }}>{`  ${column(item.row.label, actionWidth())}`}</span>
                        <span style={{ fg: colors.muted }}>{`  ${column(item.row.tag ?? "", scopeWidth())}`}</span>
                      </Show>
                    </text>
                  )
                }}
              </For>
              <Show when={rows().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>No match · Backspace widens the filter</text>
              </Show>
              <Show when={open().detailPane && highlighted()?.detail}>
                <text width="100%" wrapMode="word" fg={colors.fg} marginTop={1}>
                  <span style={{ fg: colors.accent }}>{`${highlighted()!.label}  `}</span>
                  {highlighted()!.detail!}
                </text>
              </Show>
              <text height={1} wrapMode="none" fg={colors.muted}>
                {open().hint ?? (open().actions?.length ? `${defaultPickerHint} · ${open().actions!.map((action) => action.label).join(" · ")}` : defaultPickerHint)}
              </text>
            </Show>
          </box>
        )
      }}
    </Show>
  )
}
