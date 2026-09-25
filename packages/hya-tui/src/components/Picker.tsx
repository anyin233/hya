/**
 * The reusable modal picker (state/picker.ts holds its pure state; the
 * controller's `openPicker(spec)` opens it). A bordered box drawn over the
 * main column near the top: the title, a `Filter` row with the typed text,
 * up to `pickerMaxRows` rows (`▸` marks the highlight, `●` the current
 * value; label, `[tag]`, muted detail), and a hint row. While it is open the
 * composer's editor is unfocused and every key but Ctrl+C goes to the picker
 * (components/Composer.tsx); a click on a row chooses it. Choosing or Esc
 * closes it and the input has the focus again.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { For, Show } from "solid-js"
import { useApp } from "../app/context"
import { defaultPickerHint, pickerMaxRows, pickerRows, pickerWindow, type ActivePicker } from "../state/picker"
import { colors } from "../theme"

/** Widest picker box, in columns. */
const pickerMaxWidth = 96

export function Picker() {
  const { store, controller } = useApp()
  const size = useTerminalDimensions()
  const width = () => Math.max(20, Math.min(pickerMaxWidth, size().width - 4))
  const left = () => Math.max(0, Math.floor((size().width - width()) / 2))
  return (
    <Show when={store.state.picker}>
      {(open: () => ActivePicker) => {
        const rows = () => pickerRows(open())
        const shown = () => {
          const window = pickerWindow(rows().length, open().index, pickerMaxRows)
          return rows().slice(window.start, window.end).map((row, offset) => ({ row, at: window.start + offset }))
        }
        const labelWidth = () => Math.min(28, Math.max(0, ...rows().map((row) => Bun.stringWidth(row.label))))
        const pad = (text: string) => text + " ".repeat(Math.max(0, labelWidth() - Bun.stringWidth(text)))
        return (
          <box
            position="absolute"
            top={2}
            left={left()}
            width={width()}
            zIndex={100}
            border
            borderColor={colors.accent}
            title={open().title}
            backgroundColor={colors.panel}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none">
              <span style={{ fg: colors.muted }}>Filter </span>
              <span style={{ fg: colors.fg }}>{open().query}</span>
              <span style={{ fg: colors.accent }}>▏</span>
              <span style={{ fg: colors.muted }}>{`  ${rows().length} of ${open().rows.length}`}</span>
            </text>
            <For each={shown()}>
              {(item) => {
                const highlighted = () => item.at === open().index
                return (
                  <text height={1} wrapMode="none" onMouseDown={() => controller.choosePickerRow(item.row)}>
                    <span style={{ fg: highlighted() ? colors.accent : colors.fg }}>{`${highlighted() ? "▸" : " "} `}</span>
                    <span style={{ fg: colors.accent }}>{item.row.current ? "● " : "  "}</span>
                    <span style={{ fg: highlighted() ? colors.accent : colors.fg }}>{pad(item.row.label)}</span>
                    <span style={{ fg: colors.muted }}>{item.row.tag ? `  [${item.row.tag}]` : ""}</span>
                    <span style={{ fg: colors.muted }}>{item.row.detail ? `  ${item.row.detail}` : ""}</span>
                  </text>
                )
              }}
            </For>
            <Show when={rows().length === 0}>
              <text height={1} wrapMode="none" fg={colors.muted}>No match · Backspace widens the filter</text>
            </Show>
            <text height={1} wrapMode="none" fg={colors.muted}>{open().hint ?? defaultPickerHint}</text>
          </box>
        )
      }}
    </Show>
  )
}
