import { createEffect, createRoot } from "solid-js"

import type { TuiInput } from "../upstream"
import { ArgsProvider } from "../upstream/context/args"
import { DataProvider, useData } from "../upstream/context/data"
import { ExitProvider } from "../upstream/context/exit"
import { KVProvider } from "../upstream/context/kv"
import { ProjectProvider } from "../upstream/context/project"
import { TuiPathsProvider, TuiStartupProvider } from "../upstream/context/runtime"
import { SDKProvider } from "../upstream/context/sdk"
import { SyncProvider, useSync } from "../upstream/context/sync"
import { HyaPaths } from "./platform"

/**
 * Snapshot of SDK sync + data provider state observed by {@link observeSdkSpine}.
 */
export type SdkSpineState = {
  /** Live sync store data from {@link useSync}. */
  sync: ReturnType<typeof useSync>["data"]
  /** Live data provider API from {@link useData}. */
  data: ReturnType<typeof useData>
}

/**
 * Mount a minimal Solid provider tree and wait until `ready` returns true.
 *
 * Used by tests to prove `launch`/SDK bootstrap reaches a usable sync/data
 * state without rendering the full TUI. Times out after 5 seconds.
 *
 * @param input - Same launch input shape as the full TUI (`url`, `directory`, …)
 * @param ready - Predicate over {@link SdkSpineState}; resolve when it returns true
 * @returns Promise that resolves when ready, or rejects on timeout / exit error
 */
export function observeSdkSpine(input: TuiInput, ready: (state: SdkSpineState) => boolean): Promise<void> {
  return new Promise((resolve, reject) => {
    let dispose = () => {}
    let settled = false
    const finish = (error?: unknown) => {
      if (settled) return
      settled = true
      clearTimeout(timeout)
      dispose()
      if (error === undefined) resolve()
      else reject(error)
    }
    const timeout = setTimeout(() => finish(new Error("SDK spine timed out")), 5000)

    createRoot((rootDispose) => {
      dispose = rootDispose
      const Probe = () => {
        const state = { sync: useSync().data, data: useData() }
        createEffect(() => {
          if (!ready(state)) return
          queueMicrotask(finish)
        })
        return null
      }

      return (
        <TuiPathsProvider value={{ cwd: process.cwd(), home: HyaPaths.home, state: HyaPaths.state, worktree: HyaPaths.data + "/worktree" }}>
          <TuiStartupProvider value={{ skipInitialLoading: false }}>
            <ExitProvider exit={finish}>
              <ArgsProvider {...input.args}>
                <KVProvider>
                  <SDKProvider url={input.url} directory={input.directory} fetch={input.fetch} headers={input.headers} events={input.events}>
                    <ProjectProvider>
                      <SyncProvider>
                        <DataProvider>
                          <Probe />
                        </DataProvider>
                      </SyncProvider>
                    </ProjectProvider>
                  </SDKProvider>
                </KVProvider>
              </ArgsProvider>
            </ExitProvider>
          </TuiStartupProvider>
        </TuiPathsProvider>
      )
    })
  })
}
