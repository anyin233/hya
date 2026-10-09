/**
 * The TUI process a WebUI host or direct `bun src/main.ts` starts (bare
 * `hya` implements the same supervision itself): it runs the app as a child Bun process on the same
 * terminal and starts it again when it asks to reload (src/reload.ts), so a
 * host keeps one process for the TUI's whole life while the app code is
 * replaced (docs/tui.md "Hot update after `hya serve restart`").
 *
 * The supervisor never touches the terminal: the app inherits stdin, stdout,
 * and stderr. SIGINT, SIGTERM, and SIGHUP go to the running app, and once one
 * arrived no reload starts another app. The supervisor exits with the app's
 * code (128 + signal number when the app died of a signal).
 */
import { constants } from "node:os"
import { readFileSync, rmSync } from "node:fs"
import { parseReloadRequest, reloadExitCode, reloadFileEnv, reloadStateEnv, supervisorEnv, type ReloadState } from "./reload"

type Env = Record<string, string | undefined>

export interface SupervisedChild {
  /** The app's exit code (128 + signal number after a signal). */
  exited: Promise<number>
  kill(signal: NodeJS.Signals): void
}

export interface SuperviseOptions {
  /** The Bun executable. */
  bun: string
  /** The app entry script (src/main.ts: read from disk again at every start). */
  entry: string
  argv: string[]
  env: Env
  /** The reload request file (the supervisor removes it). */
  file: string
  spawn?: (command: string[], env: Env) => SupervisedChild
  /** Register the forwarding of SIGINT, SIGTERM, and SIGHUP. */
  onSignal?: (handler: (signal: NodeJS.Signals) => void) => void
}

const forwarded: NodeJS.Signals[] = ["SIGINT", "SIGTERM", "SIGHUP"]

const signalCode = (signal: NodeJS.Signals): number => 128 + (constants.signals[signal] ?? 0)

function spawnApp(command: string[], env: Env): SupervisedChild {
  const child = Bun.spawn(command, { env, stdin: "inherit", stdout: "inherit", stderr: "inherit" })
  return {
    exited: child.exited.then(() => child.signalCode ? signalCode(child.signalCode as NodeJS.Signals) : child.exitCode ?? 1),
    kill: (signal) => { child.kill(signal) },
  }
}

function listen(handler: (signal: NodeJS.Signals) => void): void {
  for (const signal of forwarded) process.on(signal, () => handler(signal))
}

/** Run the app until it exits without asking to reload; resolves to the exit code for the supervisor. */
export function supervise({ bun, entry, argv, env, file, spawn = spawnApp, onSignal = listen }: SuperviseOptions): Promise<number> {
  const base: Env = { ...env, [reloadFileEnv]: file, [supervisorEnv]: String(process.pid) }
  delete base[reloadStateEnv]
  let signalled: NodeJS.Signals | undefined
  let current = spawn([bun, entry, ...argv], base)
  onSignal((signal) => {
    signalled ??= signal
    current.kill(signal)
  })
  const loop = async (): Promise<number> => {
    for (;;) {
      const code = await current.exited
      const request = code === reloadExitCode ? takeRequest(file) : undefined
      if (!request || signalled) {
        rmSync(file, { force: true })
        return signalled ? signalCode(signalled) : code
      }
      const state: ReloadState = request.draft ? { draft: request.draft } : {}
      current = spawn([bun, entry, ...request.argv], { ...base, [reloadStateEnv]: JSON.stringify(state) })
    }
  }
  return loop()
}

function takeRequest(file: string): ReturnType<typeof parseReloadRequest> {
  let text: string
  try {
    text = readFileSync(file, "utf8")
  } catch {
    return undefined
  }
  rmSync(file, { force: true })
  return parseReloadRequest(text)
}
