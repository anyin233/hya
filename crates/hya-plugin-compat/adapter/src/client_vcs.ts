export type CompatVcsInfo = {
  readonly branch: string
  readonly default_branch?: string
}

type VcsContext = {
  readonly directory: string
}

export async function vcsInfo(context: VcsContext): Promise<CompatVcsInfo> {
  const [branch, defaultBranch] = await Promise.all([
    gitText(context, ["branch", "--show-current"]),
    gitDefaultBranch(context),
  ])
  if (defaultBranch === "") {
    return { branch }
  }
  return { branch, default_branch: defaultBranch }
}

async function gitDefaultBranch(context: VcsContext): Promise<string> {
  const remote = await primaryRemote(context)
  if (remote !== "") {
    const branch = await gitText(context, ["symbolic-ref", "--short", `refs/remotes/${remote}/HEAD`])
    const prefix = `${remote}/`
    if (branch.startsWith(prefix)) {
      return branch.slice(prefix.length)
    }
    if (branch !== "") {
      return branch
    }
  }

  const refs = lines(await gitText(context, ["for-each-ref", "--format=%(refname:short)", "refs/heads"]))
  const configured = await gitText(context, ["config", "init.defaultBranch"])
  if (configured !== "" && refs.includes(configured)) {
    return configured
  }
  if (refs.includes("main")) {
    return "main"
  }
  if (refs.includes("master")) {
    return "master"
  }
  return ""
}

async function primaryRemote(context: VcsContext): Promise<string> {
  const remotes = lines(await gitText(context, ["remote"]))
  if (remotes.includes("origin")) {
    return "origin"
  }
  if (remotes.length === 1) {
    return remotes[0] ?? ""
  }
  if (remotes.includes("upstream")) {
    return "upstream"
  }
  return remotes[0] ?? ""
}

function lines(text: string): readonly string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
}

async function gitText(
  context: VcsContext,
  args: readonly string[],
): Promise<string> {
  const proc = Bun.spawn(["git", "-C", context.directory, ...args], {
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, , exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (exitCode !== 0) {
    return ""
  }
  return stdout.trim()
}
