/** Solid context giving every component the store, the controller, and the server URL. */
import { createContext, useContext } from "solid-js"
import type { AppStore } from "../state/store"
import type { Controller } from "./controller"

export interface AppContextValue {
  store: AppStore
  controller: Controller
  server: string
}

export const AppContext = createContext<AppContextValue>()

export function useApp(): AppContextValue {
  const value = useContext(AppContext)
  if (!value) throw new Error("useApp needs <AppContext.Provider>")
  return value
}
