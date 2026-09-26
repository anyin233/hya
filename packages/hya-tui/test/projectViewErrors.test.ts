import { expect, test } from "bun:test"
import { HttpError, type HyaClient } from "../src/client"
import { createProjectViewController } from "../src/app/projectView"
import { errorLine } from "../src/state/projectView"
import { createAppStore } from "../src/state/store"

const offline = new HttpError(503, "GET", "/v1/projects", "unavailable: remote backend is offline, or the relay link was rotated or is wrong")
const key = (name: string) => ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "" })
const settle = () => new Promise((resolve) => setTimeout(resolve, 0))

test("errorLine is `<code>: <message>` on one line, never a stack or the request line", () => {
  expect(errorLine(offline)).toBe("unavailable: remote backend is offline, or the relay link was rotated or is wrong")
  const multi = new Error("first line\n    at somewhere (file.ts:1:2)")
  expect(errorLine(multi)).toBe("first line")
  expect(errorLine(new TypeError("Unable to connect. Is the computer able to access the url?"))).toBe("unavailable: Unable to connect. Is the computer able to access the url?")
  expect(errorLine("\x1b[31mred\x1b[0m")).toBe("red")
})

function harness(failures: { reload?: boolean; switchProject?: boolean; temporary?: boolean }) {
  const store = createAppStore()
  const unhandled: unknown[] = []
  const onUnhandled = (reason: unknown) => { unhandled.push(reason) }
  process.on("unhandledRejection", onUnhandled)
  const view = createProjectViewController({
    store,
    client: {} as unknown as HyaClient,
    refreshProjects: async () => { if (failures.reload) throw offline },
    switchProject: async () => { if (failures.switchProject) throw offline },
    newTemporarySession: async () => { if (failures.temporary) throw offline },
  })
  return { store, view, unhandled, done: () => process.off("unhandledRejection", onUnhandled) }
}

test("a failing reload on open shows the error in the view", async () => {
  const h = harness({ reload: true })
  h.view.open()
  await settle()
  expect(h.store.state.projectView?.notice).toEqual({ tone: "error", text: `Refresh failed: ${errorLine(offline)}` })
  expect(h.unhandled).toEqual([])
  h.done()
})

test("a failing temporary session keeps the view open with the error", async () => {
  const h = harness({ temporary: true })
  h.view.open()
  h.view.key(key("t"))
  await settle()
  expect(h.store.state.projectView?.notice).toEqual({ tone: "error", text: `Temporary session failed: ${errorLine(offline)}` })
  expect(h.unhandled).toEqual([])
  h.done()
})

test("a failing switch keeps the view open with the error; a working one closes it", async () => {
  const h = harness({ switchProject: true })
  h.store.setProjects([{ id: "a", name: "alpha", roots: ["/a"] }])
  h.view.open()
  h.view.key(key("return"))
  await settle()
  expect(h.store.state.projectView?.notice).toEqual({ tone: "error", text: `Switch failed: ${errorLine(offline)}` })
  expect(h.unhandled).toEqual([])
  h.done()
  const ok = harness({})
  ok.store.setProjects([{ id: "a", name: "alpha", roots: ["/a"] }])
  ok.view.open()
  ok.view.key(key("return"))
  await settle()
  expect(ok.store.state.projectView).toBeUndefined()
  ok.done()
})
