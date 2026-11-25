import { extensionManager } from "../../extensions/manager"
import { sandboxPolicies, type SandboxPolicy } from "../../extensions/sandbox"
import { loadPreferences, preferencesPath } from "../../prefs"
import { matchValues, type CommandSpec } from "../registry"
import { skillCommandPrefix } from "../menu"

/**
 * `/extensions trust|untrust` and the Bundles view's `t`: remember the choice
 * (`extensionTrusted`) and move the extension to its tier now when it is loaded.
 */
export async function setExtensionTrust(id: string, trusted: boolean, save: (patch: { extensionTrusted: Record<string, boolean> }) => void): Promise<void> {
  const current = loadPreferences(preferencesPath(process.env)).preferences.extensionTrusted ?? {}
  save({ extensionTrusted: { ...current, [id]: trusted } })
  if (extensionManager.list().some((extension) => extension.id === id)) await extensionManager.setTrusted(id, trusted)
}

export const extensionsCommand: CommandSpec = {
  name: "/extensions",
  description: "Bundle TUI extensions: list, enable, disable, reload, trust (JIT), or set the sandbox policy",
  argumentHint: "[enable|disable|reload|trust|untrust <bundle id>|sandbox <required|best-effort|disabled>]",
  complete: ({ words, current, head }) => {
    if (words.length === 1) return matchValues(head, current, ["enable", "disable", "reload", "trust", "untrust", "sandbox"])
    if (words.length === 2 && words[0] === "sandbox") return matchValues(head, current, [...sandboxPolicies])
    if (words.length === 2) return matchValues(head, current, extensionManager.list().map((extension) => extension.id))
    return []
  },
  run: async ({ store, actions }, { args }) => {
    const [command, target] = args
    const enabledPreferences = () => loadPreferences(preferencesPath(process.env)).preferences.extensionEnabled ?? {}
    if (command === "enable" || command === "disable") {
      if (!target) throw new Error(`Usage: /extensions ${command} <bundle id>`)
      actions.savePreferences({ extensionEnabled: { ...enabledPreferences(), [target]: command === "enable" } })
      await extensionManager.setEnabled(target, command === "enable")
      store.setStatus(`Extension ${target} ${command}d`)
      return
    }
    if (command === "reload") {
      if (!target) throw new Error("Usage: /extensions reload <bundle id>")
      await extensionManager.reload(target)
      store.setStatus(`Extension ${target} reloaded`)
      return
    }
    if (command === "trust" || command === "untrust") {
      if (!target) throw new Error(`Usage: /extensions ${command} <bundle id>`)
      await setExtensionTrust(target, command === "trust", actions.savePreferences)
      store.setStatus(command === "trust" ? `Extension ${target} trusted: runs on the JIT (no VM memory cap)` : `Extension ${target} untrusted: runs in the VM`)
      return
    }
    if (command === "sandbox") {
      if (!sandboxPolicies.includes(target as SandboxPolicy)) throw new Error(`Usage: /extensions sandbox <${sandboxPolicies.join("|")}>`)
      actions.savePreferences({ extensionSandbox: target as SandboxPolicy })
      extensionManager.configure({ sandbox: target as SandboxPolicy })
      for (const extension of extensionManager.list()) if (extension.state === "running" || extension.state === "failed") await extensionManager.reload(extension.id)
      store.setStatus(`Extension sandbox policy: ${target}`)
      return
    }
    if (command !== undefined) throw new Error("Usage: /extensions [enable|disable|reload|trust|untrust <bundle id>|sandbox <policy>]")
    const lines = [`Bundle TUI extensions · SDK ${extensionManager.sdkVersion() ?? "not installed"} · OS sandbox ${extensionManager.sandboxPolicy()}`, ""]
    const extensions = extensionManager.list()
    if (extensions.length === 0) lines.push("None in this scope. A bundle declares one with a `tui:` manifest section (docs/tui-extensions.md).")
    for (const extension of extensions) {
      // Untrusted extensions run in the SDK's VM, trusted ones on the JIT; "OS sandbox" says whether the host is also confined by the OS.
      const tier = extension.jit ? "JIT (trusted)" : "VM"
      lines.push(`${extension.id} ${extension.version} · ${extension.state}${extension.reason ? ` · ${extension.reason}` : ""}${extension.state === "running" ? (extension.isolated ? ` · ${tier} + OS sandbox` : ` · ${tier} only (no OS sandbox)`) : ""}`)
      lines.push(`  permissions  ${extension.permissions.join(", ") || "none"}`)
      const contributions = extension.contributions
      if (contributions) {
        if (contributions.panels.length) lines.push(`  panels       ${contributions.panels.map((panel) => `${extension.id}#${panel.id} (${panel.replaces ? `replaces ${panel.replaces}` : panel.placement})`).join(", ")}`)
        if (contributions.statusItems.length) lines.push(`  status       ${contributions.statusItems.map((item) => item.label).join(", ")}`)
        if (contributions.renderers.length) lines.push(`  renderers    ${contributions.renderers.map((renderer) => `${renderer.id} → ${renderer.target} ${renderer.mode} p${renderer.priority}`).join(", ")}`)
        if (contributions.formatters.length) lines.push(`  formatters   ${contributions.formatters.map((formatter) => `${formatter.id} p${formatter.priority}`).join(", ")}`)
        if (contributions.interceptors.length) lines.push(`  intercepts   ${contributions.interceptors.map((interceptor) => `${interceptor.id} → ${interceptor.target} p${interceptor.priority}`).join(", ")}`)
      }
      for (const warning of extension.warnings) lines.push(`  ! ${warning}`)
      for (const line of extension.log.slice(-3)) lines.push(`  log: ${line}`)
    }
    lines.push("", "/extensions enable|disable|reload|trust|untrust <id> · /extensions sandbox <required|best-effort|disabled> · /layout split vertical extension <bundle>#<panel>")
    store.setStatusText(lines.join("\n"))
    store.setView("status")
  },
}

export const catalogCommands: CommandSpec[] = [
  {
    name: "/diff",
    description: "Open the Diff view: the working tree diff, split per file",
    run: ({ actions }) => { actions.openDiff() },
  },
  {
    name: "/mcp",
    description: "Open the MCP view: server status, tools, connect/disconnect, login",
    run: ({ actions }) => { actions.openMcp() },
  },
  {
    name: "/bundles",
    description: "Manage installed bundles: install, uninstall, enable, disable, trust",
    run: ({ actions }) => { actions.openBundles() },
  },
  {
    name: "/skills",
    description: "Open the Skills view: every skill of the scope by source, its file and body; u puts /skill:<name> into the input",
    argumentHint: "[filter]",
    complete: ({ words, current, head }, context) => words.length === 1 ? matchValues(head, current, context.backendCommands.filter((name) => name.startsWith(skillCommandPrefix)).map((name) => name.slice(skillCommandPrefix.length))) : [],
    run: ({ actions }, { argumentsText }) => { actions.openSkills(argumentsText.trim()) },
  },
  {
    name: "/rules",
    description: "Open the Saved Rules view: saved permission decisions, delete",
    run: ({ actions }) => { actions.openRules() },
  },
]
