import type { ToolInfo } from "./tool"

/** Maximum UTF-8 byte length of a Skill contribution id. */
export const MAX_SKILL_ID_BYTES = 256

/** Maximum UTF-8 byte length of a Skill contribution body. */
export const MAX_SKILL_CONTENT_BYTES = 256 * 1024

/** Maximum UTF-8 byte length of a Skill contribution digest. */
export const MAX_SKILL_DIGEST_BYTES = 64
/** One Skill declaration published by a Compat plugin initialize handshake. */
export type SkillContribution = {
  /** Stable Skill identifier in the plugin contribution namespace. */
  readonly id: string
  /** Complete Skill content supplied by the plugin. */
  readonly content: string
  /** Exact content digest supplied by the plugin. */
  readonly digest: string
}

/** One hook registration emitted by the Compat adapter. */
export type HookRegistration = {
  /** Wire hook name understood by the hya host. */
  readonly name: string
}

/** One workspace adapter metadata declaration emitted by the Compat adapter. */
export type WorkspaceAdapterContribution = {
  /** Adapter kind discriminator. */
  readonly type: string
  /** Human-readable adapter name. */
  readonly name: string
  /** Human-readable adapter description. */
  readonly description: string
}

/** The complete typed initialize contribution surface. */
export type PluginContributionSet = {
  /** Hook registrations produced by the loaded Compat plugins. */
  readonly hooks: readonly HookRegistration[]
  /** Tool declarations produced by the loaded Compat plugins. */
  readonly tools: readonly ToolInfo[]
  /** Skill declarations produced by the loaded Compat plugins. */
  readonly skills: readonly SkillContribution[]
  /** Workspace adapter registrations produced by the loaded Compat plugins. */
  readonly workspaceAdapters: readonly WorkspaceAdapterContribution[]
}

/** Return an empty contribution set for a newly created adapter context. */
export function emptyPluginContributionSet(): PluginContributionSet {
  return {
    hooks: [],
    tools: [],
    skills: [],
    workspaceAdapters: [],
  }
}
