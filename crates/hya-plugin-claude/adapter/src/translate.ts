/**
 * Translation of Claude Code plugin sources into hya concepts.
 *
 * Mapping (design §5.2, P7):
 *
 * | Claude Code            | hya                                             |
 * | ---------------------- | ----------------------------------------------- |
 * | `plugin.json`          | bundle identity (`claude/<name>`, publisher     |
 * |                        | `claude`), namespace = sanitized name           |
 * | `agents/*.md`          | skill resources + the bundle Agent prompt       |
 * |                        | (first entry)                                   |
 * | per-skill `SKILL.md`   | skill resources                                 |
 * | `commands/*.md`        | skill resources (prompt-template bodies)        |
 * | `.mcp.json`            | `resources.mcp` JSON files                      |
 * | `hooks/hooks.json`     | runtime hook declarations (see `hooks.ts`)      |
 *
 * The same translation feeds two surfaces: the runtime initialize
 * declaration (skill contributions with digests) and the offline
 * `--emit-bundle-manifest` envelope consumed by
 * `hya-backend bundle install --claude`.
 */

import fs from "node:fs"
import path from "node:path"

import { agentMetaFrom, parseFrontmatter } from "./frontmatter"
import { parseClaudeHooks, type ClaudeHookGroup, type HookName } from "./hooks"
import {
  CLAUDE_PLUGIN_METADATA_DIR,
  CLAUDE_MCP_FILE,
  PLUGIN_MANIFEST_FILE,
} from "./manifest_paths"
import { readPluginJson, type PluginSource } from "./discovery"

/** Hygiene limits shared with the runtime contribution validation. */
export const MAX_SKILL_CONTENT_BYTES = 256 * 1024

/** One translated skill resource (also a runtime skill contribution). */
export type TranslatedSkill = {
  /** Sanitized, deduplicated resource id. */
  readonly id: string
  /** Bundle-source-relative file path (`skills/<id>.md`). */
  readonly path: string
  /** Full skill content. */
  readonly content: string
  /** Lowercase SHA-256 hex digest of the UTF-8 content. */
  readonly digest: string
}

/** One translated MCP server declaration file. */
export type TranslatedMcp = {
  /** Sanitized MCP server id (the `mcpServers` key). */
  readonly id: string
  /** Bundle-source-relative file path (`mcp/<id>.json`). */
  readonly path: string
  /** File content (the hya `McpServerConfig` JSON shape). */
  readonly content: string
}

/** The complete translation of one plugin source. */
export type Translation = {
  /** Discovered source directory and manifest. */
  readonly source: PluginSource
  /** Sanitized namespace token (the plugin name). */
  readonly namespace: string
  /** Bundle identity: `claude/<namespace>`, plugin version, publisher `claude`. */
  readonly identity: { readonly id: string; readonly version: string; readonly publisher: string }
  /** Skill resources derived from `agents/`, `skills/`, and `commands/`. */
  readonly skills: readonly TranslatedSkill[]
  /** MCP declaration files derived from `.mcp.json`. */
  readonly mcp: readonly TranslatedMcp[]
  /** The single bundle Agent (first `agents/*.md`, else synthesized). */
  readonly agent: {
    readonly id: string
    readonly description: string
    readonly promptPath: string
    readonly prompt: string
  }
  /** All translated files referenced by the emitted manifest. */
  readonly files: readonly { readonly path: string; readonly content: string }[]
  /** The complete `bundle.yaml` manifest text. */
  readonly manifestYaml: string
  /** Parsed `hooks/hooks.json` matcher groups keyed by hya hook name. */
  readonly hookGroups: Readonly<Record<HookName, readonly ClaudeHookGroup[]>>
}

/** Why a plugin directory could not be translated. */
export class TranslateError extends Error {}

/**
 * Translate one Claude Code plugin directory.
 *
 * Throws `TranslateError` for a missing/invalid `plugin.json` or resource
 * files that exceed the contribution limits.
 */
export function translatePlugin(dir: string): Translation {
  const source = readPluginJson(dir)
  if (source === undefined) {
    throw new TranslateError(
      `${dir} is not a Claude Code plugin: missing ${PLUGIN_MANIFEST_FILE}`,
    )
  }
  const namespace = sanitizeToken(source.manifest.name)
  if (namespace.length === 0) {
    throw new TranslateError(`plugin name ${JSON.stringify(source.manifest.name)} sanitizes to an empty namespace`)
  }

  const skills: TranslatedSkill[] = []
  const usedIds = new Set<string>(["mcp", "harness", "builtin", "plugin"])
  collectAgentSkills(dir, skills, usedIds)
  collectSkills(dir, skills, usedIds)
  collectCommandSkills(dir, skills, usedIds)

  const mcp = collectMcpFiles(dir)
  const agent = buildAgent(source, dir)
  const hookGroups = parseClaudeHooks(readHooksDocument(dir)).groups
  const files = [
    ...skills.map((skill) => ({ path: skill.path, content: skill.content })),
    ...mcp.map((entry) => ({ path: entry.path, content: entry.content })),
    { path: agent.promptPath, content: agent.prompt },
  ]
  const manifestYaml = renderManifest(source, namespace, skills, mcp, agent)
  return {
    source,
    namespace,
    identity: {
      id: `claude/${namespace}`,
      version: source.manifest.version,
      publisher: "claude",
    },
    skills,
    mcp,
    agent,
    files,
    manifestYaml,
    hookGroups,
  }
}

/**
 * Sanitize a Claude Code name into a hya namespace/resource token:
 * lowercase, `__` and non-`[a-z0-9_]` characters map to `-`, repeats
 * collapse, edges trim. Mirrors `hya-plugin-claude::emit::sanitize_namespace`.
 */
export function sanitizeToken(name: string): string {
  return name
    .replace(/__/g, "-")
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "")
}

/** Lowercase SHA-256 hex digest of a UTF-8 string (synchronous). */
export function sha256Hex(content: string): string {
  const hasher = new Bun.CryptoHasher("sha256")
  hasher.update(content)
  return hasher.digest("hex")
}

function collectAgentSkills(
  dir: string,
  skills: TranslatedSkill[],
  usedIds: Set<string>,
): void {
  const agentsDir = path.join(dir, "agents")
  for (const filePath of listMarkdown(agentsDir)) {
    const content = readBounded(filePath)
    const stem = path.basename(filePath, ".md")
    const meta = agentMetaFrom(content, stem)
    const id = uniqueId(sanitizeToken(meta.name) || sanitizeToken(stem), usedIds)
    skills.push({
      id,
      path: `skills/${id}.md`,
      content,
      digest: sha256Hex(content),
    })
  }
}

function collectSkills(
  dir: string,
  skills: TranslatedSkill[],
  usedIds: Set<string>,
): void {
  const skillsDir = path.join(dir, "skills")
  if (!fs.existsSync(skillsDir)) {
    return
  }
  for (const entry of sortedEntries(skillsDir)) {
    const skillDir = path.join(skillsDir, entry)
    if (!fs.statSync(skillDir).isDirectory()) {
      continue
    }
    const skillFile = path.join(skillDir, "SKILL.md")
    if (!fs.existsSync(skillFile)) {
      continue
    }
    const content = readBounded(skillFile)
    const id = uniqueId(sanitizeToken(entry), usedIds)
    skills.push({ id, path: `skills/${id}.md`, content, digest: sha256Hex(content) })
  }
}

function collectCommandSkills(
  dir: string,
  skills: TranslatedSkill[],
  usedIds: Set<string>,
): void {
  for (const filePath of listMarkdown(path.join(dir, "commands"))) {
    const content = readBounded(filePath)
    const stem = path.basename(filePath, ".md")
    const id = uniqueId(sanitizeToken(stem), usedIds)
    skills.push({ id, path: `skills/${id}.md`, content, digest: sha256Hex(content) })
  }
}

function readHooksDocument(dir: string): unknown {
  const hooksFile = path.join(dir, "hooks", "hooks.json")
  if (!fs.existsSync(hooksFile)) {
    return undefined
  }
  try {
    return JSON.parse(fs.readFileSync(hooksFile, "utf8"))
  } catch {
    return undefined
  }
}

function collectMcpFiles(dir: string): readonly TranslatedMcp[] {
  const candidates = [
    path.join(dir, CLAUDE_MCP_FILE),
    path.join(dir, CLAUDE_PLUGIN_METADATA_DIR, CLAUDE_MCP_FILE),
  ]
  for (const mcpFile of candidates) {
    if (!fs.existsSync(mcpFile)) {
      continue
    }
    let parsed: unknown
    try {
      parsed = JSON.parse(fs.readFileSync(mcpFile, "utf8"))
    } catch (error) {
      throw new TranslateError(`${path.relative(dir, mcpFile)} is not valid JSON: ${String(error)}`)
    }
    if (!isRecord(parsed) || !isRecord(parsed["mcpServers"])) {
      continue
    }
    const servers = parsed["mcpServers"] as Record<string, unknown>
    const used = new Set<string>()
    const translated: TranslatedMcp[] = []
    for (const key of Object.keys(servers).sort()) {
      const id = uniqueId(sanitizeToken(key), used)
      const content = renderMcpConfig(servers[key])
      translated.push({ id, path: `mcp/${id}.json`, content })
    }
    return translated
  }
  return []
}

function renderMcpConfig(value: unknown): string {
  if (!isRecord(value)) {
    throw new TranslateError(".mcp.json server entries must be objects")
  }
  const config: Record<string, unknown> = {}
  for (const key of ["command", "env", "url", "transport", "enabled", "timeout_ms"]) {
    if (value[key] !== undefined) {
      config[key] = value[key]
    }
  }
  return `${JSON.stringify(config, null, 2)}\n`
}

function buildAgent(
  source: PluginSource,
  dir: string,
): Translation["agent"] {
  const agentsDir = path.join(dir, "agents")
  const markdown = listMarkdown(agentsDir)
  if (markdown.length > 0) {
    const first = markdown[0]
    if (first === undefined) {
      return synthesizedAgent(source)
    }
    const content = readBounded(first)
    const stem = path.basename(first, ".md")
    const meta = agentMetaFrom(content, stem)
    const body = parseFrontmatter(content).body.trim()
    const id = sanitizeToken(meta.name) || sanitizeToken(stem) || sanitizeToken(source.manifest.name)
    const prompt = body.length > 0 ? `${body}\n` : `You operate the ${source.manifest.name} plugin.\n`
    return {
      id,
      description: meta.description || source.manifest.description,
      promptPath: `prompts/${id}.md`,
      prompt,
    }
  }
  return synthesizedAgent(source)
}

function synthesizedAgent(source: PluginSource): Translation["agent"] {
  const id = sanitizeToken(source.manifest.name)
  return {
    id: id.length > 0 ? id : "claude-plugin",
    description: source.manifest.description,
    promptPath: "prompts/claude-plugin.md",
    prompt: `You operate the ${source.manifest.name} Claude Code plugin inside hya. Use its skills to answer and act.\n`,
  }
}

function renderManifest(
  source: PluginSource,
  namespace: string,
  skills: readonly TranslatedSkill[],
  mcp: readonly TranslatedMcp[],
  agent: Translation["agent"],
): string {
  const lines: string[] = []
  lines.push("kind: AgentBundle")
  lines.push("identity:")
  lines.push(`  id: ${yamlString(`claude/${namespace}`)}`)
  lines.push(`  version: ${yamlString(source.manifest.version)}`)
  lines.push(`  publisher: ${yamlString("claude")}`)
  lines.push(`namespace: ${yamlString(namespace)}`)
  lines.push("resources:")
  lines.push("  skills:")
  if (skills.length === 0) {
    lines.push("    []")
  } else {
    for (const skill of [...skills].sort((left, right) => left.id.localeCompare(right.id))) {
      lines.push(`    - id: ${yamlString(skill.id)}`)
      lines.push(`      path: ${yamlString(skill.path)}`)
    }
  }
  lines.push("  mcp:")
  if (mcp.length === 0) {
    lines.push("    []")
  } else {
    for (const entry of mcp) {
      lines.push(`    - id: ${yamlString(entry.id)}`)
      lines.push(`      path: ${yamlString(entry.path)}`)
    }
  }
  lines.push("agent:")
  lines.push(`  id: ${yamlString(agent.id)}`)
  if (agent.description.length > 0) {
    lines.push(`  description: ${yamlString(agent.description)}`)
  }
  lines.push("  role: main")
  lines.push("  spawn_lifecycle: transient")
  lines.push(`  prompt: ${yamlString(agent.promptPath)}`)
  return `${lines.join("\n")}\n`
}

/** YAML double-quoted scalar; JSON escaping is valid YAML 1.2. */
function yamlString(value: string): string {
  return JSON.stringify(value)
}

function listMarkdown(dir: string): readonly string[] {
  if (!fs.existsSync(dir)) {
    return []
  }
  return sortedEntries(dir)
    .filter((entry) => entry.endsWith(".md"))
    .map((entry) => path.join(dir, entry))
}

function sortedEntries(dir: string): readonly string[] {
  return fs.readdirSync(dir).sort()
}

function readBounded(filePath: string): string {
  const stat = fs.statSync(filePath)
  if (stat.size > MAX_SKILL_CONTENT_BYTES) {
    throw new TranslateError(`${filePath} exceeds the ${MAX_SKILL_CONTENT_BYTES}-byte skill limit`)
  }
  return fs.readFileSync(filePath, "utf8")
}

function uniqueId(candidate: string, used: Set<string>): string {
  let id = candidate.length > 0 ? candidate : "resource"
  let suffix = 2
  while (used.has(id)) {
    id = `${candidate}-${suffix}`
    suffix += 1
  }
  used.add(id)
  return id
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/** Re-export for callers that want the frontmatter parser alongside. */
export { parseFrontmatter }
