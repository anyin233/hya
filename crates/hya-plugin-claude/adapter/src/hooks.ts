/**
 * Claude Code hook translation (design §5.2): `hooks/hooks.json` declarations
 * become hya hook registrations, and the adapter executes the underlying
 * shell commands with the Claude Code stdin/stdout JSON contract, translating
 * CC decisions into hya veto/allow replies.
 *
 * Event mapping (CC → hya):
 *
 * | Claude Code       | hya hook                |
 * | ----------------- | ----------------------- |
 * | `PreToolUse`      | `tool.execute.before`   |
 * | `PostToolUse`     | `tool.execute.after`    |
 * | `UserPromptSubmit`| `message.user.before`   |
 * | `SessionStart`, `SessionEnd`, `Stop`, `SubagentStop`,
 *   `PreCompact`, `Notification` | `event` (fire-and-forget)      |
 */

import { isRecord } from "./validate"

/** Wire hook names the adapter registers (hya plugin ABI v1). */
export type HookName =
  | "event"
  | "command.execute.before"
  | "message.user.before"
  | "tool.execute.before"
  | "tool.execute.after"

/** One hook registration on the initialize wire. */
export type HookRegistration = {
  /** Wire hook name understood by the hya host. */
  readonly name: HookName
}

/** One CC hook command: the `hooks: [{type: "command", command}]` entry. */
export type ClaudeHookCommand = {
  /** Shell command text executed via `sh -c`. */
  readonly command: string
  /** Optional per-hook timeout in seconds (CC default 60). */
  readonly timeoutSeconds: number
}

/** One CC matcher group: filter plus the commands it fans out to. */
export type ClaudeHookGroup = {
  /** CC matcher expression (`""`/`*` = all; otherwise `|`-separated names). */
  readonly matcher: string
  readonly commands: readonly ClaudeHookCommand[]
}

/** The parsed `hooks/hooks.json` surface. */
export type ClaudeHooks = {
  readonly groups: Readonly<Record<HookName, readonly ClaudeHookGroup[]>>
}

const CC_EVENT_TO_HYA: Readonly<Record<string, HookName>> = {
  PreToolUse: "tool.execute.before",
  PostToolUse: "tool.execute.after",
  UserPromptSubmit: "message.user.before",
  SessionStart: "event",
  SessionEnd: "event",
  Stop: "event",
  SubagentStop: "event",
  PreCompact: "event",
  Notification: "event",
}

/**
 * Parse a `hooks.json` document. Unknown CC event names are dropped; groups
 * without `type: "command"` entries are dropped (v1 has no prompt/device
 * hook transport).
 */
export function parseClaudeHooks(document: unknown): ClaudeHooks {
  const groups: Record<HookName, ClaudeHookGroup[]> = {
    event: [],
    "command.execute.before": [],
    "message.user.before": [],
    "tool.execute.before": [],
    "tool.execute.after": [],
  }
  if (!isRecord(document)) {
    return { groups }
  }
  for (const [ccEvent, rawValue] of Object.entries(document)) {
    const hyaName = CC_EVENT_TO_HYA[ccEvent]
    if (hyaName === undefined || !Array.isArray(rawValue)) {
      continue
    }
    for (const rawGroup of rawValue) {
      if (!isRecord(rawGroup)) {
        continue
      }
      const matcher = typeof rawGroup["matcher"] === "string" ? rawGroup["matcher"] : ""
      const commands: ClaudeHookCommand[] = []
      const rawCommands = Array.isArray(rawGroup["hooks"]) ? rawGroup["hooks"] : []
      for (const rawCommand of rawCommands) {
        if (
          !isRecord(rawCommand) ||
          rawCommand["type"] !== "command" ||
          typeof rawCommand["command"] !== "string" ||
          rawCommand["command"].length === 0
        ) {
          continue
        }
        const timeout =
          typeof rawCommand["timeout"] === "number" && rawCommand["timeout"] > 0
            ? rawCommand["timeout"]
            : 60
        commands.push({ command: rawCommand["command"], timeoutSeconds: timeout })
      }
      if (commands.length > 0) {
        groups[hyaName].push({ matcher, commands })
      }
    }
  }
  return { groups }
}

/** Distinct hook registrations declared by the parsed hooks document. */
export function hookRegistrationsFrom(hooks: ClaudeHooks): readonly HookRegistration[] {
  return (Object.keys(hooks.groups) as HookName[])
    .filter((name) => hooks.groups[name].length > 0)
    .sort()
    .map((name) => ({ name }))
}

/** CC matcher matching: `""`/`*` match everything, else `|`-separated names. */
export function matcherMatches(matcher: string, toolName: string): boolean {
  const trimmed = matcher.trim()
  if (trimmed.length === 0 || trimmed === "*") {
    return true
  }
  return trimmed
    .split("|")
    .map((part) => part.trim())
    .includes(toolName)
}

/** Build the CC stdin payload for one dispatched hya hook call. */
export function claudeHookPayload(
  hookName: HookName,
  params: {
    readonly session?: string
    readonly cwd?: string
    readonly tool?: string
    readonly input?: unknown
    readonly result?: unknown
  },
): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    session_id: params.session ?? "",
    transcript_path: "",
    cwd: params.cwd ?? process.cwd(),
    hook_event_name: reverseEventName(hookName),
  }
  if (hookName === "tool.execute.before" || hookName === "tool.execute.after") {
    payload["tool_name"] = params.tool ?? ""
    payload["tool_input"] = params.input ?? {}
  }
  if (hookName === "tool.execute.after") {
    payload["tool_response"] = params.result ?? null
  }
  return payload
}

/** Execute one CC hook command, returning its stdout (empty on failure). */
export async function runClaudeHookCommand(
  hook: ClaudeHookCommand,
  payload: unknown,
): Promise<{ readonly ok: boolean; readonly stdout: string }> {
  const process_ = Bun.spawn(["sh", "-c", hook.command], {
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const timeout = setTimeout(() => {
    process_.kill()
  }, hook.timeoutSeconds * 1000)
  try {
    process_.stdin.write(JSON.stringify(payload))
    await process_.stdin.flush()
    process_.stdin.end()
    const stdout = await new Response(process_.stdout).text()
    const exitCode = await process_.exited
    return { ok: exitCode === 0, stdout }
  } catch {
    return { ok: false, stdout: "" }
  } finally {
    clearTimeout(timeout)
  }
}

/**
 * Translate the first JSON object on a CC hook's stdout into a hya
 * `tool.execute.before` outcome. CC conventions understood:
 * `decision: "block"`, `permissionDecision: "deny"`, and
 * `hookSpecificOutput.permissionDecision: "deny"` veto the tool call with the
 * declared reason; anything else continues.
 */
export function toolDecisionFromClaudeStdout(
  stdout: string,
): { readonly outcome: "continue" } | { readonly outcome: "veto"; readonly reason: string } | undefined {
  const parsed = firstJsonObject(stdout)
  if (parsed === undefined) {
    return undefined
  }
  const direct = decisionFromObject(parsed)
  if (direct !== undefined) {
    return direct
  }
  const specific = parsed["hookSpecificOutput"]
  if (isRecord(specific)) {
    return decisionFromObject(specific)
  }
  return undefined
}

function decisionFromObject(
  object: Record<string, unknown>,
):
  | { readonly outcome: "continue" }
  | { readonly outcome: "veto"; readonly reason: string }
  | undefined {
  const fallbackReason = "blocked by Claude Code hook"
  const reason =
    typeof object["reason"] === "string"
      ? object["reason"]
      : typeof object["permissionDecisionReason"] === "string"
        ? (object["permissionDecisionReason"] as string)
        : fallbackReason
  const permission = object["permissionDecision"]
  if (object["decision"] === "block" || permission === "deny" || permission === "ask") {
    return { outcome: "veto", reason }
  }
  if (object["decision"] === "approve" || permission === "allow") {
    return { outcome: "continue" }
  }
  return undefined
}

/** Extract the first JSON object embedded in a CC hook stdout blob. */
export function firstJsonObject(stdout: string): Record<string, unknown> | undefined {
  const start = stdout.indexOf("{")
  if (start < 0) {
    return undefined
  }
  for (let end = stdout.lastIndexOf("}"); end > start; end = stdout.lastIndexOf("}", end - 1)) {
    try {
      const parsed: unknown = JSON.parse(stdout.slice(start, end + 1))
      if (isRecord(parsed)) {
        return parsed
      }
      return undefined
    } catch {
      continue
    }
  }
  return undefined
}

function reverseEventName(hookName: HookName): string {
  switch (hookName) {
    case "tool.execute.before":
      return "PreToolUse"
    case "tool.execute.after":
      return "PostToolUse"
    case "message.user.before":
      return "UserPromptSubmit"
    default:
      return "SessionStart"
  }
}
