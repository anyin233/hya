/**
 * The full-screen Provider View (`/key`; docs/tui.md "Provider View"). The
 * state and keys are state/providers.ts, the calls app/providers.ts; keys
 * reach it through the composer's handler (components/Composer.tsx).
 *
 * Drawn over the whole screen (the chat layout stays mounted beneath it):
 * a bordered box with the screen's header line, column titles, the rows
 * (a window that follows the highlight), then the last model test, the
 * running call (spinner, elapsed seconds, `Esc cancels`), a notice, the
 * filter, and the key line. A pop-up form (the add-provider wizard, the key
 * form, the model forms, a confirm) is a smaller box over it; the modal
 * picker (the `/model` prompt after adding a provider) goes over both.
 */
import { useTerminalDimensions } from "@opentui/solid"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { pickerWindow } from "../state/picker"
import {
  modelHeaderLine,
  modelLine,
  providerDetailHeader,
  providerHeaderLine,
  providerLine,
  providerModels,
  providerViewHint,
  shownProviders,
  testResultText,
  type FormField,
  type FormState,
  type ProviderBusy,
  type ProviderNotice,
  type ProviderViewState,
} from "../state/providers"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

/** Widest pop-up form, in columns. */
const formMaxWidth = 76

function noticeColor(notice: ProviderNotice): string {
  return notice.tone === "error" ? colors.error : notice.tone === "ok" ? colors.accent : colors.fg
}

/** The running call: spinner, label, elapsed seconds, and how to cancel it. */
function BusyLine(props: { busy: ProviderBusy }) {
  const frame = useSpinner()
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), 1000)
  onCleanup(() => clearInterval(timer))
  const seconds = () => Math.max(0, Math.floor((now() - props.busy.startedAt) / 1000))
  return (
    <text height={1} wrapMode="none">
      <span style={{ fg: colors.accent }}>{frame()}</span>
      <span style={{ fg: colors.fg }}>{` ${props.busy.label}… ${seconds()}s`}</span>
      <span style={{ fg: colors.muted }}>{" · Esc cancels"}</span>
    </text>
  )
}

/** One field row of a form: done fields show their value, the current one a cursor (or its options), later ones only their label. */
function FieldRow(props: { field: FormField; index: number; form: FormState }) {
  const state = () => (props.index < props.form.step ? "done" : props.index === props.form.step ? "current" : "later")
  const label = () => props.field.label.padEnd(14)
  const shown = () => {
    const field = props.field
    if (field.kind === "secret") return "•".repeat(field.masked ?? 0)
    if (field.kind === "choice") return field.options?.find((option) => option.id === field.value)?.label ?? field.value
    return field.value
  }
  return (
    <>
      <text height={1} wrapMode="none">
        <span style={{ fg: state() === "current" ? colors.accent : colors.muted }}>{`${state() === "current" ? "▸" : " "} ${label()}`}</span>
        <Show when={state() !== "later"}>
          <span style={{ fg: colors.fg }}>{shown() || (props.field.kind === "secret" && state() === "done" ? "(none)" : "")}</span>
        </Show>
        <Show when={state() === "current" && props.field.kind !== "choice"}>
          <span style={{ fg: colors.accent }}>▏</span>
          <span style={{ fg: colors.muted }}>{shown() ? "" : ` ${props.field.placeholder ?? ""}`}</span>
        </Show>
      </text>
      <Show when={state() === "current" && props.field.kind === "choice"}>
        <For each={props.field.options ?? []}>
          {(option, at) => {
            const chosen = () => option.id === props.field.value
            return (
              <text height={1} wrapMode="none">
                <span style={{ fg: chosen() ? colors.accent : colors.fg }}>{`    ${chosen() ? "●" : "○"} ${at() + 1}. ${option.label.padEnd(16)}`}</span>
                <span style={{ fg: colors.muted }}>{option.detail ?? ""}</span>
              </text>
            )
          }}
        </For>
      </Show>
    </>
  )
}

/** The pop-up form over the view. */
function FormBox(props: { view: ProviderViewState; form: FormState }) {
  const size = useTerminalDimensions()
  const width = () => Math.max(30, Math.min(formMaxWidth, size().width - 4))
  const left = () => Math.max(0, Math.floor((size().width - width()) / 2))
  const title = () => props.form.kind === "addProvider" || props.form.kind === "addModel"
    ? `${props.form.title} · ${props.form.step + 1}/${props.form.fields.length}`
    : props.form.title
  return (
    <box
      position="absolute"
      top={3}
      left={left()}
      width={width()}
      zIndex={60}
      border
      borderColor={colors.accent}
      title={title()}
      backgroundColor={colors.panel}
      flexDirection="column"
      paddingX={1}
    >
      <Show
        when={props.form.kind !== "confirm"}
        fallback={<text width="100%" wrapMode="word" fg={colors.fg}>{props.form.confirmText ?? ""}</text>}
      >
        <For each={props.form.fields}>
          {(field, index) => <FieldRow field={field} index={index()} form={props.form} />}
        </For>
      </Show>
      <Show when={props.form.error}>
        <text width="100%" wrapMode="word" fg={colors.error}>{`✗ ${props.form.error}`}</text>
      </Show>
      <Show when={props.view.busy} fallback={<text width="100%" wrapMode="word" fg={colors.muted}>{providerViewHint(props.view)}</text>}>
        {(busy) => <BusyLine busy={busy()} />}
      </Show>
    </box>
  )
}

export function ProviderView() {
  const { store } = useApp()
  const size = useTerminalDimensions()
  return (
    <Show when={store.state.providerView}>
      {(open: () => ProviderViewState) => {
        // Inside the border and padding, after the 2-column row marker.
        const lineWidth = () => Math.max(20, size().width - 6)
        const provider = () => store.state.providers.find((row) => row.id === open().provider)
        const detail = () => open().screen === "detail"
        const providers = () => shownProviders(open(), store.state.providers)
        const models = () => (open().provider ? providerModels(store.state.models, open().provider!, open().filter) : [])
        const rows = () => detail()
          ? models().map((model) => ({ id: model.id, text: modelLine(model, lineWidth()), muted: false }))
          : providers().map((row) => ({ id: row.id, text: providerLine(row, lineWidth()), muted: row.id === "hya" && !row.kind }))
        const highlighted = () => (detail() ? open().model : open().provider)
        // Chrome: border 2, header 2 (header + titles), bottom lines ~5.
        const visible = () => Math.max(3, size().height - 11)
        const shown = () => {
          const all = rows()
          const at = Math.max(0, all.findIndex((row) => row.id === highlighted()))
          const window = pickerWindow(all.length, at, visible())
          return all.slice(window.start, window.end)
        }
        const title = () => (detail() ? `Providers › ${open().provider ?? ""}` : "Providers")
        const header = () => detail()
          ? (provider() ? providerDetailHeader(provider()!) : open().provider ?? "")
          : `${store.state.providers.filter((row) => row.id !== "hya").length} configured · changes apply at once, no restart`
        const empty = () => detail()
          ? (open().filter ? "No model matches the filter" : "No models · r fetches the list · m adds one by hand")
          : (open().filter ? "No provider matches the filter" : "No providers yet · a adds one")
        return (
          <>
          <box
            position="absolute"
            top={0}
            left={0}
            width="100%"
            height="100%"
            zIndex={50}
            border
            borderColor={colors.accent}
            title={title()}
            backgroundColor={colors.bg}
            flexDirection="column"
            paddingX={1}
          >
            <text height={1} wrapMode="none" fg={colors.fg}>{header()}</text>
            <text height={1} wrapMode="none" fg={colors.muted}>{`  ${detail() ? modelHeaderLine(lineWidth()) : providerHeaderLine(lineWidth())}`}</text>
            <box flexGrow={1} flexDirection="column">
              <For each={shown()}>
                {(row) => {
                  const on = () => row.id === highlighted()
                  return (
                    <text height={1} wrapMode="none">
                      <span style={{ fg: colors.accent }}>{on() ? "▸ " : "  "}</span>
                      <span style={{ fg: on() ? colors.accent : row.muted ? colors.muted : colors.fg }}>{row.text}</span>
                    </text>
                  )
                }}
              </For>
              <Show when={rows().length === 0}>
                <text height={1} wrapMode="none" fg={colors.muted}>{`  ${empty()}`}</text>
              </Show>
            </box>
            <Show when={detail() && open().test}>
              {(test) => <text height={1} wrapMode="none" fg={test().ok ? colors.accent : colors.error}>{testResultText(test())}</text>}
            </Show>
            <Show when={!open().form && open().busy}>
              {(busy) => <BusyLine busy={busy()} />}
            </Show>
            <Show when={open().notice}>
              {(notice) => <text width="100%" wrapMode="word" fg={noticeColor(notice())}>{notice().text}</text>}
            </Show>
            <Show when={open().filtering || open().filter}>
              <text height={1} wrapMode="none">
                <span style={{ fg: colors.muted }}>Filter </span>
                <span style={{ fg: colors.fg }}>{open().filter}</span>
                <span style={{ fg: colors.accent }}>{open().filtering ? "▏" : ""}</span>
              </text>
            </Show>
            <text width="100%" wrapMode="word" fg={colors.muted}>{providerViewHint(open().form ? { ...open(), form: undefined } : open())}</text>
          </box>
          <Show when={open().form}>
            {(form) => <FormBox view={open()} form={form()} />}
          </Show>
          </>
        )
      }}
    </Show>
  )
}
