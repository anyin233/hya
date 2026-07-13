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

export type SdkSpineState = {
  sync: ReturnType<typeof useSync>["data"]
  data: ReturnType<typeof useData>
}

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
