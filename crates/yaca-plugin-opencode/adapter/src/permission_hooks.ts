import type { OpenCodeHooks } from "./loader/init"

export type WireResource = { readonly type: string; readonly value?: string }

export type PermissionAskParams = {
  readonly session?: string
  readonly action: string
  readonly resource: WireResource
}

export type PermissionOutcome =
  | { readonly outcome: "allow_once" }
  | { readonly outcome: "reject"; readonly feedback?: string }
  | { readonly outcome: "defer" }

type PermissionStatus = "ask" | "deny" | "allow"

type OpenCodePermission = {
  readonly id: string
  readonly type: string
  readonly pattern?: string | readonly string[]
  readonly sessionID: string
  readonly messageID: string
  readonly callID?: string
  readonly title: string
  readonly metadata: Readonly<Record<string, unknown>>
  readonly time: { readonly created: number }
}

type PermissionAskHook = (
  input: OpenCodePermission,
  output: { status: PermissionStatus },
) => unknown | Promise<unknown>

export async function runPermissionAskHooks(
  hooks: readonly OpenCodeHooks[],
  params: PermissionAskParams,
): Promise<PermissionOutcome> {
  const input = openCodePermission(params)
  const output: { status: PermissionStatus } = { status: "ask" }
  for (const hook of hooks) {
    const candidate = hook["permission.ask"]
    if (!isPermissionAskHook(candidate)) {
      continue
    }
    try {
      await candidate(input, output)
    } catch {
      continue
    }
  }
  switch (output.status) {
    case "allow":
      return { outcome: "allow_once" }
    case "deny":
      return { outcome: "reject" }
    case "ask":
      return { outcome: "defer" }
  }
}

function openCodePermission(params: PermissionAskParams): OpenCodePermission {
  const pattern = resourcePattern(params.resource)
  const sessionID = params.session ?? ""
  return {
    id: `perm_yaca_${params.action}_${pattern}`,
    type: params.action,
    pattern,
    sessionID,
    messageID: "",
    title: permissionTitle(params.action, pattern),
    metadata: {
      action: params.action,
      resource: params.resource.type,
    },
    time: { created: 0 },
  }
}

function resourcePattern(resource: WireResource): string {
  return resource.value ?? "*"
}

function permissionTitle(action: string, pattern: string): string {
  return `${action}: ${pattern}`
}

function isPermissionAskHook(value: unknown): value is PermissionAskHook {
  return typeof value === "function"
}
