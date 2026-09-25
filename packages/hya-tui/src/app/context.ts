/** Solid context giving every component the store, the controller, and the server URL. */
import { createContext, useContext } from "solid-js"
import type { AppStore } from "../state/store"
import type { Controller } from "./controller"

/** Imperative handles registered by mounted components (the transcript's scroll actions). */
export interface UiHandles {
  transcript?: TranscriptScroller
}

export interface TranscriptScroller {
  /** Scroll by one page; `-1` up, `1` down. */
  page(direction: -1 | 1): void
  top(): void
  /** Jump to the newest line and follow it again. */
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
