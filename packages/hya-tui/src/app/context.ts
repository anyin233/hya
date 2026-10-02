/** Solid context giving every component the store, the controller, and the server URL. */
import { createContext, useContext } from "solid-js"
import type { KeyEvent, PasteEvent } from "@opentui/core"
import type { AppStore } from "../state/store"
import type { InputHistory } from "../composer/history"
import type { Controller } from "./controller"

/** Imperative handles registered by mounted components (the transcript's and Diff view's scroll actions). */
export interface UiHandles {
  /** One workspace listener calls this router before native input dispatch. */
  workspaceInput?: { onKey(event: KeyEvent): void; onPaste(event: PasteEvent): void }
  /** Mounted selectable panes expose the same input contract, keyed by instance id. */
  paneInputs?: Map<string, PaneInputHandle>
  transcript?: TranscriptScroller
  /** Scroll actions of read-only workspace panes, keyed by stable pane id. */
  panes?: Map<string, DiffScroller>
  diff?: DiffScroller
  command?: CommandPaneHandle
  composerHistory?: InputHistory
  commandHistory?: InputHistory
  composerRows?: () => number
  composerInput?: { text: string; cursor: number }
  commandInput?: { text: string; active: boolean; originSidebar: boolean }
}

/** A focused pane consumes unsupported input; it never falls through to another pane. */
export interface PaneInputHandle {
  onKey(event: KeyEvent): void
  onPaste?(event: PasteEvent): void
}

/** One command input, independent of the message composer. */
export interface CommandPaneHandle {
  active(): boolean
  open(): void
  /** Returns true when the key was handled and must not reach the editor. */
  key(event: KeyEvent): boolean
  paste(text: string): void
}

export interface TranscriptScroller {
  /** Scroll by one page; `-1` up, `1` down. */
  page(direction: -1 | 1): void
  top(): void
  /** Jump to the newest line and follow it again. */
  bottom(): void
}

/** The Diff view's open file body (components/DiffView.tsx); mirrors `TranscriptScroller` plus a single-line step. */
export interface DiffScroller {
  /** Scroll by one line; `-1` up, `1` down. */
  line(direction: -1 | 1): void
  /** Scroll by one page; `-1` up, `1` down. */
  page(direction: -1 | 1): void
  top(): void
  bottom(): void
}

export interface AppContextValue {
  store: AppStore
  controller: Controller
  server: string
  ui: UiHandles
}

export const AppContext = createContext<AppContextValue>()

export function useApp(): AppContextValue {
  const value = useContext(AppContext)
  if (!value) throw new Error("useApp needs <AppContext.Provider>")
  return value
}
