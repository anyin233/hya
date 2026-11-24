/**
 * The startup list (ADR-0036 `startup` component, docs/tui-config.md
 * "Startup list"): what this TUI and its backend loaded — config warnings,
 * context files, skills, commands, MCP servers, bundles, and TUI extensions —
 * at the start of the transcript. Problems (config warnings, MCP failures or
 * logins, unreadable bundles, extension errors) always show, `quiet` too.
 * Pure: the component (components/StartupBlock.tsx) feeds it the store and
 * the extension manager and draws the lines.
 */
import type { BundleSummary, CatalogDiagnostic, CommandSummary, McpServerStatus, SkillSummary } from "../client"
import type { ExtensionInfo } from "../extensions/manager"
import { formatBytes } from "../composer/attachments"

export const startupSectionIds = ["context", "skills", "commands", "mcp", "bundles", "extensions"] as const
export type StartupSectionId = typeof startupSectionIds[number]

/** A context file the backend loaded into the system prompt (`Bootstrap.contextFiles`). */
export interface ContextFileSummary {
  path: string
  /** `project`, `parent`, or `user`. */
  scope?: string
  /** A uint64: a decimal string in protojson. */
  bytes?: string | number
}

export interface StartupInput {
  /** `[Config]` / `[Layout]` lines from app/structure.ts (always shown). */
  notices: readonly string[]
  contextFiles?: readonly ContextFileSummary[]
  skills?: readonly SkillSummary[]
  /** Skill files the catalog skipped or warns about (`[Skills]` problems). */
  skillDiagnostics?: readonly CatalogDiagnostic[]
  commands?: readonly CommandSummary[]
  /** Command files the catalog skipped (`[Slash commands]` problems). */
  commandDiagnostics?: readonly CatalogDiagnostic[]
  /** Backend commands a local command of the same name hides (`[Slash commands]` problems). */
  shadowedCommands?: readonly CommandSummary[]
  mcp?: readonly McpServerStatus[]
  bundles?: readonly BundleSummary[]
  extensions?: readonly ExtensionInfo[]
  /** The extension catalog could not be read. */
  extensionsError?: string
}

export interface StartupOptions {
  resources: "compact" | "full"
  sections: readonly StartupSectionId[]
  quiet: boolean
}

export type LineTone = "heading" | "muted" | "warning"
export interface StartupLine {
  text: string
  tone: LineTone
  /** The `[Section]` prefix of `text`, drawn in the heading color. */
  label?: string
}

interface Item {
  name: string
  /** Grouping for the full list (`user ~/.codex/skills`, `first_party`, …). */
  group: string
  /** Shown after the name in the full list. */
  detail?: string
  /** A note in the compact list too: `(disabled)`, `(connecting)`. */
  note?: string
}

interface Section {
  title: string
  items: Item[]
  problems: string[]
}

const titles: Record<StartupSectionId, string> = {
  context: "[Context]", skills: "[Skills]", commands: "[Slash commands]", mcp: "[MCP]", bundles: "[Bundles]", extensions: "[Extensions]",
}

const byName = (left: Item, right: Item) => left.name.localeCompare(right.name)
const dirOf = (path: string) => path.replace(/\/[^/]*$/, "") || "/"
const home = (path: string, env: Record<string, string | undefined>) => {
  const dir = env.HOME?.replace(/\/+$/, "")
  return dir && (path === dir || path.startsWith(`${dir}/`)) ? `~${path.slice(dir.length)}` : path
}

/**
 * Where a skill came from. `SkillSummary.source` says `builtin` for user skills
 * on older backends, so a real file location wins: under `$HOME` → `user`,
 * else `project`; `<built-in>` → `builtin`.
 */
export function skillGroup(skill: SkillSummary, env: Record<string, string | undefined> = process.env): string {
  // A bundle or plugin skill has no file: group by its owner.
  if (skill.bundleId) return `bundle ${skill.bundleId}`
  const location = skill.location ?? ""
  if (!location || location.startsWith("<")) return skill.source || "builtin"
  const source = skill.source && skill.source !== "builtin" ? skill.source : (env.HOME && location.startsWith(`${env.HOME}/`) ? "user" : "project")
  // The skill's own directory holds SKILL.md: group by the directory of skills.
  return `${source} ${home(dirOf(dirOf(location)), env)}`
}

function sections(input: StartupInput, env: Record<string, string | undefined>): Record<StartupSectionId, Section | undefined> {
  const mcp = input.mcp ?? []
  const state = (server: McpServerStatus) => (server.state ?? "").replace(/^MCP_SERVER_STATE_/, "").toLowerCase()
  return {
    context: input.contextFiles ? {
      title: titles.context,
      items: input.contextFiles.map((file) => ({ name: home(file.path, env), group: file.scope || "context", ...(Number(file.bytes) > 0 ? { detail: formatBytes(Number(file.bytes)) } : {}) })),
      problems: [],
    } : undefined,
    skills: input.skills ? {
      title: titles.skills,
      items: input.skills.map((skill) => ({ name: skill.name, group: skillGroup(skill, env) })),
      problems: (input.skillDiagnostics ?? []).map((diagnostic) => `${home(diagnostic.message, env)} · /skills`),
    } : undefined,
    commands: input.commands ? {
      title: titles.commands,
      items: input.commands.filter((command) => (command.source ?? "command") === "command").map((command) => ({ name: `/${command.name.replace(/^\//, "")}`, group: command.scope && command.scope !== "builtin" ? `${command.scope} commands` : "commands", detail: command.description })),
      problems: [
        ...(input.commandDiagnostics ?? []).map((diagnostic) => home(diagnostic.message, env)),
        ...(input.shadowedCommands ?? []).map((command) => `/${command.name}: the TUI's own /${command.name} hides this ${command.scope ? `${command.scope} ` : ""}command; rename its file to run it`),
      ],
    } : undefined,
    mcp: input.mcp ? {
      title: titles.mcp,
      items: mcp.map((server) => ({
        name: server.name,
        group: "servers",
        detail: [state(server), server.tools?.length ? `${server.tools.length} tools` : ""].filter(Boolean).join(" · "),
        ...(state(server) === "desired" ? { note: "(connecting)" } : state(server) === "disconnected" ? { note: "(disconnected)" } : {}),
      })),
      problems: mcp.flatMap((server) => server.authRequired
        ? [`${server.name}: needs login · /mcp to connect`]
        : state(server) === "failed" ? [`${server.name}: failed${server.error ? `: ${server.error}` : ""} · /mcp to fix`] : []),
    } : undefined,
    bundles: input.bundles ? {
      title: titles.bundles,
      items: input.bundles.filter((bundle) => bundle.state !== "unreadable").map((bundle) => ({
        name: bundle.id,
        group: bundle.scope ?? "bundles",
        detail: [bundle.version, bundle.state].filter(Boolean).join(" · "),
        ...(bundle.state === "disabled" || bundle.enabled === false ? { note: "(disabled)" } : bundle.state === "shadowed" ? { note: "(shadowed)" } : {}),
      })),
      problems: input.bundles.filter((bundle) => bundle.state === "unreadable").map((bundle) => `${bundle.id}: unreadable${bundle.error ? `: ${bundle.error}` : ""} · /bundles`),
    } : undefined,
    extensions: input.extensions || input.extensionsError ? {
      title: titles.extensions,
      items: (input.extensions ?? []).filter((extension) => extension.state !== "failed" && extension.state !== "blocked").map((extension) => ({
        name: extension.id,
        group: "extensions",
        detail: [extension.version, extension.state].filter(Boolean).join(" · "),
        ...(extension.state === "disabled" ? { note: "(disabled)" } : extension.state === "starting" ? { note: "(starting)" } : {}),
      })),
      problems: [
        ...(input.extensionsError ? [input.extensionsError] : []),
        ...(input.extensions ?? []).flatMap((extension) => extension.state === "failed" || extension.state === "blocked"
          ? [`${extension.id}: ${extension.state}${extension.reason ? `: ${extension.reason}` : ""} · /extensions`]
          : extension.warnings.map((warning) => `${extension.id}: ${warning}`)),
      ],
    } : undefined,
  }
}

/** `[Skills] a, b, c, +11` cut to `width` columns (at least the title and a count). */
export function compactLine(title: string, names: readonly string[], width: number): string {
  if (!names.length) return `${title} none`
  let line = title
  for (const [index, name] of names.entries()) {
    const rest = names.length - index - 1
    const piece = `${index ? "," : ""} ${name}`
    const tail = rest ? `, +${rest}` : ""
    if (Bun.stringWidth(line + piece + tail) > width && index > 0) return `${line}, +${names.length - index}`
    if (Bun.stringWidth(line + piece + tail) > width) return `${title} +${names.length}`
    line += piece
  }
  return line
}

/** Every line of the startup list for `width` columns. */
export function startupLines(input: StartupInput, options: StartupOptions, width: number, env: Record<string, string | undefined> = process.env): StartupLine[] {
  // The notices of app/structure.ts (`[Config]`, `[Layout]`) are problems by nature.
  const lines: StartupLine[] = input.notices.map((text) => ({ text, tone: "warning" }))
  const all = sections(input, env)
  for (const id of options.sections) {
    const section = all[id]
    if (!section) continue
    const items = [...section.items].sort(byName)
    if (!options.quiet) {
      if (options.resources === "full") {
        lines.push({ text: section.title, tone: "heading" })
        const groups = new Map<string, Item[]>()
        for (const item of items) groups.set(item.group, [...groups.get(item.group) ?? [], item])
        if (!items.length) lines.push({ text: "  none", tone: "muted" })
        for (const [group, members] of groups) {
          const flat = members.every((item) => !item.detail)
          if (flat) lines.push({ text: `  ${group}: ${members.map((item) => item.name + (item.note ? ` ${item.note}` : "")).join(", ")}`, tone: "muted" })
          else {
            lines.push({ text: `  ${group}`, tone: "muted" })
            for (const item of members) lines.push({ text: `    ${item.name}${item.detail ? ` · ${item.detail}` : ""}`, tone: "muted" })
          }
        }
      } else if (items.length) {
        // An empty section says nothing worth a line at startup; `/resources` lists it as `none`.
        lines.push({ text: compactLine(section.title, items.map((item) => item.name + (item.note ? ` ${item.note}` : "")), width), tone: "muted", label: section.title })
      }
    }
    for (const problem of section.problems) lines.push({ text: `${options.quiet ? `${section.title} ` : "  "}${problem}`, tone: "warning" })
  }
  return lines
}
