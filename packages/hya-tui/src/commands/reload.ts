/**
 * `/reload` and `/reload-<target>` (docs/tui.md "Reload"; app/reload.ts
 * does the work). The outcome goes into the transcript as a notice — every
 * preset shows it — and `/reload` follows it with the startup list, the
 * resources as they are now.
 */
import { startupInput } from "../app/startupInput"
import { reloadAllTargets, reloadSummary, reloadTargetDescriptions, reloadTargets, type ReloadTarget } from "../state/reload"
import { startupLines } from "../state/startup"
import type { CommandContext, CommandSpec } from "./registry"

function report({ store }: CommandContext, summary: string, resources: boolean): void {
  const settings = store.state.structure.startup
  const lines = resources
    ? startupLines({ ...startupInput(store.state), notices: [] }, { resources: settings.resources, quiet: settings.quiet, sections: settings.sections }, Math.max(20, store.state.columns - 4))
    : []
  store.setView("chat")
  store.addTranscriptNotice([summary, ...lines.map((line) => line.text)].join("\n"))
  store.setStatus(summary.split("\n")[0]!)
}

export const reloadCommand: CommandSpec = {
  name: "/reload",
  description: "Reload tui.toml, keybindings, theme, bundles, TUI extensions, MCP, commands, skills, and the session's AGENTS.md context",
  run: async (context) => {
    context.store.setStatus("Reloading…")
    report(context, reloadSummary(await context.actions.reload(reloadAllTargets)), true)
  },
}

/** `/reload-layout` is `/layout reload`, so the workspace-only rule and its messages stay in one place. */
const layoutReload: CommandSpec = {
  name: "/reload-layout",
  description: reloadTargetDescriptions.layout,
  run: async (context) => {
    const layout = context.registry?.get("/layout")
    if (!layout) throw new Error("No /layout command")
    await layout.run(context, context.registry!.parse("/layout reload"))
  },
}

export const reloadTargetCommands: CommandSpec[] = reloadTargets.map((target: ReloadTarget): CommandSpec => target === "layout" ? layoutReload : {
  name: `/reload-${target}`,
  description: reloadTargetDescriptions[target],
  run: async (context) => {
    report(context, reloadSummary(await context.actions.reload([target])), false)
  },
})
