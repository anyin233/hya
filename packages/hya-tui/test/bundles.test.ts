// The Bundles view's state machine (state/bundles.ts): keys to view updates and commands.
import { describe, expect, test } from "bun:test"
import type { BundleSummary } from "../src/client"
import { helpRows } from "../src/commands/help"
import type { ExtensionInfo } from "../src/extensions/manager"
import type { KeyLike } from "../src/keys/bindings"
import { bundleKeyRows, bundleLine, bundleRows, bundlesViewHint, bundlesViewKey, initialBundlesView, type BundleViewOutcome, type BundleViewState } from "../src/state/bundles"

const key = (name: string, sequence = name.length === 1 ? name : ""): KeyLike => ({ name, sequence, ctrl: false, meta: false, shift: false })
const typed = (text: string): KeyLike[] => [...text].map((character) => key(character))

const bundles: BundleSummary[] = [
  { id: "acme/panel", version: "1.2.0", scope: "user", state: "active", enabled: true, removable: true, components: { agents: ["helper"], skills: ["a", "b"], tui: true, tuiPermissions: ["tui.panel"] } },
  { id: "acme/panel", version: "1.0.0", scope: "project", state: "active", enabled: true, removable: true, components: { tui: true } },
  { id: "hya/basic-tui-components", scope: "first_party", state: "active", enabled: true, components: { tui: true } },
  { id: "acme/off", scope: "user", state: "disabled", removable: true, components: { tools: ["t"] } },
]
const extension = (id: string, jit: boolean): ExtensionInfo => ({ id, version: "1", state: "running", isolated: true, jit, permissions: [], warnings: [], log: [] })
const rows = bundleRows(bundles, [extension("acme/panel", false), extension("hya/basic-tui-components", true)])

function updated(outcome: BundleViewOutcome): BundleViewState {
  if (outcome.type !== "update") throw new Error(`expected an update, got ${outcome.type}`)
  return outcome.view
}

function press(view: BundleViewState, ...keys: KeyLike[]): BundleViewState {
  return keys.reduce((current, pressed) => updated(bundlesViewKey(current, pressed, rows)), view)
}

const at = (selected: string, projectTarget = true): BundleViewState => ({ ...initialBundlesView(rows, projectTarget), selected })

describe("rows", () => {
  test("rows are keyed by scope and id; only an active bundle's extension state is attached", () => {
    expect(rows.map((row) => [row.key, row.extension?.jit])).toEqual([
      ["user:acme/panel", false],
      ["project:acme/panel", false],
      ["first_party:hya/basic-tui-components", true],
      ["user:acme/off", undefined],
    ])
    expect(bundleLine(rows[0]!, 200)).toMatch(/^acme\/panel\s+1\.2\.0\s+user\s+active\s+VM running\s+1 agent · 2 skills · TUI$/)
    expect(bundleLine(rows[2]!, 200)).toContain("first-party")
    expect(bundleLine(rows[2]!, 200)).toContain("JIT running")
    expect(bundleLine(rows[3]!, 200)).toMatch(/disabled\s+—\s+1 tool$/)
  })
})

describe("keys", () => {
  test("Up/Down wrap over rows; Enter opens details; Esc goes back, then closes", () => {
    let view = initialBundlesView(rows, true)
    view = press(view, key("up"))
    expect(view.selected).toBe("user:acme/off")
    view = press(view, key("down"), key("down"), key("return"))
    expect([view.selected, view.screen]).toEqual(["project:acme/panel", "detail"])
    expect(bundlesViewHint(view)).toContain("Esc back")
    view = press(view, key("escape"))
    expect(view.screen).toBe("list")
    expect(bundlesViewKey(view, key("escape"), rows)).toEqual({ type: "close" })
  })

  test("the filter narrows rows and keeps the highlight on a shown one", () => {
    let view = press(initialBundlesView(rows, true), key("/"), ...typed("disabled"))
    expect(view.selected).toBe("user:acme/off")
    view = press(view, key("return"))
    expect(view.filtering).toBe(false)
    view = press(view, key("escape"))
    expect(view.filter).toBe("")
  })

  test("install asks for the path, then user or project scope", () => {
    let view = press(at("user:acme/panel"), key("i"))
    expect(press(view, key("return")).popup).toEqual({ kind: "install", step: "path", path: "", error: "Type the path of a .hyabundle package" })
    view = press(view, ...typed("pkg/x.hyabundle"), key("return"))
    expect(view.popup).toEqual({ kind: "install", step: "target", path: "pkg/x.hyabundle", project: false })
    view = press(view, key("p"))
    expect(bundlesViewKey(view, key("return"), rows)).toEqual({ type: "command", command: { kind: "install", path: "pkg/x.hyabundle", project: true } })
    expect(press(view, key("escape")).popup).toBeUndefined()
  })

  test("without a scope directory, install goes to the user registry at once", () => {
    const view = press(at("user:acme/panel", false), key("i"), ...typed("/a.hyabundle"))
    expect(bundlesViewKey(view, key("return"), rows)).toEqual({ type: "command", command: { kind: "install", path: "/a.hyabundle", project: false } })
  })

  test("uninstall asks first and targets the row's scope; first-party bundles refuse", () => {
    const view = press(at("project:acme/panel"), key("x"))
    expect(bundlesViewHint(view)).toBe("Enter uninstalls · Esc cancels")
    expect(bundlesViewKey(view, key("return"), rows)).toEqual({ type: "command", command: { kind: "uninstall", bundleId: "acme/panel", project: true } })
    expect(press(at("first_party:hya/basic-tui-components"), key("x")).notice?.text).toContain("cannot be uninstalled")
  })

  test("e toggles enabled, telling whether the bundle has a TUI extension", () => {
    expect(bundlesViewKey(at("user:acme/panel"), key("e"), rows)).toEqual({ type: "command", command: { kind: "setEnabled", bundleId: "acme/panel", enabled: false, tui: true } })
    expect(bundlesViewKey(at("user:acme/off"), key("e"), rows)).toEqual({ type: "command", command: { kind: "setEnabled", bundleId: "acme/off", enabled: true, tui: false } })
  })

  test("t toggles the extension's tier; a bundle without one refuses", () => {
    expect(bundlesViewKey(at("user:acme/panel"), key("t"), rows)).toEqual({ type: "command", command: { kind: "setTrusted", bundleId: "acme/panel", trusted: true } })
    expect(bundlesViewKey(at("first_party:hya/basic-tui-components"), key("t"), rows)).toEqual({ type: "command", command: { kind: "setTrusted", bundleId: "hya/basic-tui-components", trusted: false } })
    expect(press(at("user:acme/off"), key("t")).notice?.text).toContain("no TUI extension")
  })

  test("while a call runs only Esc (cancel) is taken", () => {
    const view: BundleViewState = { ...at("user:acme/panel"), busy: { label: "Installing", startedAt: 0 } }
    expect(bundlesViewKey(view, key("e"), rows)).toEqual({ type: "none" })
    expect(bundlesViewKey(view, key("escape"), rows)).toEqual({ type: "cancelBusy" })
  })
})

test("the footer and the help overlay come from the same key table", () => {
  for (const row of bundleKeyRows.filter((candidate) => candidate.hint && candidate.screens.includes("list"))) expect(bundlesViewHint(initialBundlesView(rows, true))).toContain(row.hint!)
  expect(helpRows([]).filter((row) => row.group === "Bundles").map((row) => row.keys)).toEqual(bundleKeyRows.map((row) => row.keys))
})
