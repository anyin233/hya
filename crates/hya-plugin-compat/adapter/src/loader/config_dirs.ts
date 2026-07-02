import path from "node:path"

import type { DiscoveryContext } from "./discovery"

export function compatConfigDirs(
  context: DiscoveryContext,
): readonly string[] {
  const dirs: string[] = []
  const global = globalConfigDir(context)
  if (global !== undefined) {
    dirs.push(global)
  }
  if (context.disableProjectConfig !== true) {
    dirs.push(...projectConfigDirs(context.directory, context.worktree))
  }
  const home = homeConfigDir(context)
  if (home !== undefined) {
    dirs.push(home)
  }
  if (context.customConfigDir !== undefined && context.customConfigDir.length > 0) {
    dirs.push(context.customConfigDir)
  }
  return [...new Set(dirs)]
}

export function projectConfigFiles(
  context: DiscoveryContext,
): readonly string[] {
  if (context.disableProjectConfig === true) {
    return []
  }
  return ancestorDirs(context.directory, context.worktree)
    .toReversed()
    .flatMap((dir) => [
      path.join(dir, "opencode.json"),
      path.join(dir, "opencode.jsonc"),
    ])
}

export function globalConfigDir(context: DiscoveryContext): string | undefined {
  if (context.xdgConfigHome !== undefined && context.xdgConfigHome.length > 0) {
    return path.join(context.xdgConfigHome, "compat")
  }
  const home = context.home ?? process.env.HOME
  if (home === undefined || home.length === 0) {
    return undefined
  }
  return path.join(home, ".config", "compat")
}

function projectConfigDirs(
  directory: string,
  worktree: string | undefined,
): readonly string[] {
  return ancestorDirs(directory, worktree).map((dir) => path.join(dir, ".opencode"))
}

function ancestorDirs(
  directory: string,
  worktree: string | undefined,
): readonly string[] {
  const dirs: string[] = []
  const stop = worktree === undefined ? undefined : path.resolve(worktree)
  let current = path.resolve(directory)
  while (true) {
    dirs.push(current)
    if (current === stop) {
      break
    }
    const parent = path.dirname(current)
    if (parent === current) {
      break
    }
    current = parent
  }
  return dirs
}

function homeConfigDir(context: DiscoveryContext): string | undefined {
  const home = context.home ?? process.env.HOME
  if (home === undefined || home.length === 0) {
    return undefined
  }
  return path.join(home, ".opencode")
}
