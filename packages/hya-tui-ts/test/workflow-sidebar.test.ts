import { expect, test } from "bun:test"

import { BUILTIN_IDS } from "../src/upstream/feature-plugins/builtins"
import WorkflowPlugin from "../src/upstream/feature-plugins/sidebar/workflow"
import { createStaticPluginHost } from "../src/hya/static-host"

const theme = {
  text: "text",
  textMuted: "muted",
  info: "info",
  success: "success",
  warning: "warning",
  error: "error",
}

function session(workflow: unknown) {
  return {
    id: "session-1",
    title: "Session",
    slug: "session",
    projectID: "project",
    directory: "/tmp/project",
    version: "test",
    time: { created: 1, updated: 2 },
    workflow,
  }
}

test("Workflow sidebar registers first with synchronized Session state access", async () => {
  expect(BUILTIN_IDS[0]).toBe("internal:sidebar-workflow")

  let registration:
    | {
        order: number
        slots: { sidebar_content: (_ctx: unknown, props: { session_id: string }) => unknown }
      }
    | undefined
  const api = {
    theme: { current: theme },
    state: {
      session: {
        get: (id: string) =>
          id === "session-1"
            ? session({
                selection: { source: "project:release", name: "release", revision: "ab".repeat(32) },
                availability: "available",
              })
            : undefined,
      },
    },
    slots: {
      register(value: typeof registration) {
        registration = value
        return "workflow-slot"
      },
    },
  }

  await WorkflowPlugin.tui(api as never, undefined, {} as never)
  expect(registration?.order).toBe(50)
  expect(registration?.slots.sidebar_content).toBeFunction()
})

test("static host unregisters the Workflow sidebar during disposal", async () => {
  const registered: string[] = []
  let unregistered = 0
  let slotsDisposed = 0
  let runtimeCleared = 0
  let inputDisposed = 0
  let statuses: Array<{ id: string }> = []
  const runtime = {
    setupSlots() {
      return {
        register(plugin: { id: string }) {
          registered.push(plugin.id)
          return () => {
            unregistered += 1
          }
        },
        dispose() {
          slotsDisposed += 1
        },
      }
    },
    update(input: { status: Array<{ id: string }> }) {
      statuses = input.status
    },
    clear() {
      runtimeCleared += 1
    },
  }
  const api = {
    kv: {
      get(_key: string, fallback: unknown) {
        return fallback
      },
      set() {},
    },
    route: {
      register() {
        return () => {}
      },
      navigate() {},
      current: { name: "home" },
    },
    event: {
      on() {
        return () => {}
      },
    },
    keymap: {
      registerLayer() {
        return () => {}
      },
    },
  }
  const host = createStaticPluginHost()

  await host.start({
    api,
    config: {},
    runtime,
    dispose() {
      inputDisposed += 1
    },
  } as never)
  expect(registered[0]).toStartWith("internal:sidebar-workflow:")
  expect(statuses[0]?.id).toBe("internal:sidebar-workflow")

  await host.dispose()
  expect(unregistered).toBe(registered.length)
  expect(slotsDisposed).toBe(1)
  expect(runtimeCleared).toBe(1)
  expect(inputDisposed).toBe(1)
})
