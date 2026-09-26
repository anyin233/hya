/**
 * The relay bridge child of `/connect-remote` (docs/tui.md "Remote backends";
 * docs/relay.md "Connecting from a client").
 *
 * `startBridge` runs `<hya> bridge - --json --exit-with-stdin [--transport T]
 * [--relay-ca PEM]`, writes the link and a newline to its stdin, and keeps
 * that pipe open: closing it (or this process dying) makes the bridge exit 0.
 * The link is never put in argv (process listings), never kept here after
 * it is written, and never part of an error or status text.
 *
 * Readiness is the first stdout line, JSON `{url, room, proxy, label}`.
 * stderr lines are the bridge's status (prefixed `hya bridge:`); a failure is
 * exit status 1 with a one-line reason on stderr.
 */

/** `/connect-remote` flags passed through to `hya bridge`. */
export interface BridgeFlags {
  transport?: "auto" | "grpc" | "ws"
  /** PEM file of extra trusted CA certificates for the relay's TLS. */
  relayCa?: string
}

/** The bridge's readiness line. */
export interface BridgeReady {
  /** Loopback URL the TUI uses as its server. */
  url: string
  room: string
  /** The redacted relay (no room, no secret). */
  proxy: string
  /** What the TUI shows for the server: `remote: <relay>/<room>`. */
  label: string
}

/** The child process as `startBridge` needs it (Bun.spawn in production, a fake in tests). */
export interface BridgeProcess {
  pid?: number
  write(text: string): void
  /** Close stdin: `--exit-with-stdin` makes the bridge exit 0. */
  end(): void
  stdout: ReadableStream<Uint8Array>
  stderr: ReadableStream<Uint8Array>
  exited: Promise<number>
  kill(signal?: "SIGTERM" | "SIGKILL"): void
}

export type BridgeSpawner = (argv: string[]) => BridgeProcess

/** A running bridge child. */
export interface Bridge extends BridgeReady {
  /** Resolves with the exit status; `stop()` sets `stopping` first, so an unexpected exit is one without it. */
  exited: Promise<number>
  readonly stopping: boolean
  /** The latest stderr line (prefix kept), for the status line after an unexpected exit. */
  lastLine(): string | undefined
  /** Close stdin; SIGTERM after `graceMs` when it is still alive. */
  stop(graceMs?: number): Promise<void>
}

export class BridgeError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "BridgeError"
  }
}

/** A relay link with its secret fragment (`hya[+insecure]://…#<key>.<psk>`), anywhere in `text`. */
const linkPattern = /\bhya(?:\+insecure)?:\/\/[^\s#]*#\S*/gi

/** Whether `text` holds a relay link with its secret part. */
export function containsRelayLink(text: string): boolean {
  linkPattern.lastIndex = 0
  return linkPattern.test(text)
}

/** `text` with every relay link cut to its redacted form (`hya://host/room#…`). */
export function redactRelayLinks(text: string): string {
  return text.replace(linkPattern, (link) => `${link.slice(0, link.indexOf("#"))}#…`)
}

/**
 * What the input history keeps of a submitted input: relay links removed
 * (`/connect-remote --transport ws <link>` → `/connect-remote --transport
 * ws`), everything else as typed.
 */
export function historyEntry(text: string): string {
  if (!containsRelayLink(text)) return text
  return text.replace(linkPattern, "").replace(/[ \t]+/g, " ").trim()
}

const transports = ["auto", "grpc", "ws"] as const

/** `/connect-remote [<link>] [--transport auto|grpc|ws] [--relay-ca <pem>]`. Errors never echo a positional argument (it may be the link). */
export function parseConnectRemote(args: readonly string[]): { link?: string; flags: BridgeFlags } {
  const flags: BridgeFlags = {}
  let link: string | undefined
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!
    if (!arg) continue
    if (arg === "--transport") {
      const value = args[++index]
      if (!value || !(transports as readonly string[]).includes(value)) throw new Error("--transport takes auto, grpc, or ws")
      flags.transport = value as BridgeFlags["transport"]
    } else if (arg === "--relay-ca") {
      const value = args[++index]
      if (!value) throw new Error("--relay-ca needs a PEM file")
      flags.relayCa = value
    } else if (arg.startsWith("--") && !containsRelayLink(arg)) {
      throw new Error(`Unknown option ${arg} · usage: /connect-remote [<link>] [--transport auto|grpc|ws] [--relay-ca <pem>]`)
    } else if (link !== undefined) {
      throw new Error("Usage: /connect-remote [<link>] [--transport auto|grpc|ws] [--relay-ca <pem>] (one link)")
    } else link = arg
  }
  return link === undefined ? { flags } : { link, flags }
}

/** Whether `link` has the shape of a relay link (`hya://` or `hya+insecure://`, with the `#` secret). */
export function looksLikeRelayLink(link: string): boolean {
  return /^hya(?:\+insecure)?:\/\/[^\s#]+#\S+$/i.test(link.trim())
}

/** `hya bridge` argv for `bin` and `flags`; the link goes to stdin, never here. */
export function bridgeArgv(bin: string, flags: BridgeFlags = {}): string[] {
  return [
    bin, "bridge", "-", "--json", "--exit-with-stdin",
    ...(flags.transport ? ["--transport", flags.transport] : []),
    ...(flags.relayCa ? ["--relay-ca", flags.relayCa] : []),
  ]
}

/** Parse the readiness line; `undefined` when it is not one. */
export function parseBridgeReady(line: string): BridgeReady | undefined {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch {
    return undefined
  }
  if (!value || typeof value !== "object") return undefined
  const { url, room, proxy, label } = value as Record<string, unknown>
  if (typeof url !== "string" || !/^https?:\/\//.test(url)) return undefined
  return {
    url,
    room: typeof room === "string" ? room : "",
    proxy: typeof proxy === "string" ? proxy : "",
    label: typeof label === "string" && label.trim() ? label.trim() : `remote: ${typeof room === "string" ? room : url}`,
  }
}

/** Call `onLine` for every line of `stream` until it ends. */
async function readLines(stream: ReadableStream<Uint8Array>, onLine: (line: string) => void): Promise<void> {
  const decoder = new TextDecoder()
  let buffered = ""
  try {
    for await (const chunk of stream as unknown as AsyncIterable<Uint8Array>) {
      buffered += decoder.decode(chunk, { stream: true })
      let newline = buffered.indexOf("\n")
      while (newline >= 0) {
        const line = buffered.slice(0, newline).replace(/\r$/, "")
        buffered = buffered.slice(newline + 1)
        if (line.trim()) onLine(line)
        newline = buffered.indexOf("\n")
      }
    }
  } catch {
    // The child went away; `exited` says how.
  }
  if (buffered.trim()) onLine(buffered.trim())
}

/** Spawn with Bun: stdin piped (the link, then held open), stdout/stderr piped. */
export const spawnBridge: BridgeSpawner = (argv) => {
  const child = Bun.spawn(argv, { stdin: "pipe", stdout: "pipe", stderr: "pipe", env: process.env })
  return {
    pid: child.pid,
    write: (text) => { child.stdin.write(text); void child.stdin.flush() },
    end: () => { try { void child.stdin.end() } catch { /* already closed */ } },
    stdout: child.stdout,
    stderr: child.stderr,
    exited: child.exited,
    kill: (signal = "SIGTERM") => { try { child.kill(signal) } catch { /* gone */ } },
  }
}

export interface StartBridgeOptions {
  /** The hya binary (the TUI's `--hya`, `HYA_BIN`, or `hya` on PATH). */
  bin: string
  link: string
  flags?: BridgeFlags
  spawn?: BridgeSpawner
  /** Longest wait for the readiness line (default 20 s). */
  timeoutMs?: number
  /** Every stderr line (redacted), as it arrives. */
  onLine?: (line: string) => void
}

const bridgePrefix = /^(?:error:\s*)?(?:hya bridge:\s*)?/i

/**
 * Start the bridge child and wait for its readiness line. Rejects with a
 * `BridgeError` (the child's one-line reason, a timeout, or a bad readiness
 * line); the child is then stopped. The rejection never contains the link.
 */
export async function startBridge({ bin, link, flags = {}, spawn = spawnBridge, timeoutMs = 20_000, onLine }: StartBridgeOptions): Promise<Bridge> {
  if (!looksLikeRelayLink(link)) throw new BridgeError("that is not a relay link (hya://…#… or hya+insecure://…#…)")
  let child: BridgeProcess
  try {
    child = spawn(bridgeArgv(bin, flags))
  } catch (error) {
    throw new BridgeError(`could not run ${bin} bridge: ${error instanceof Error ? error.message : String(error)}`)
  }
  let last: string | undefined
  let stopping = false
  const stderrDone = readLines(child.stderr, (raw) => {
    const line = redactRelayLinks(raw)
    last = line
    onLine?.(line)
  })
  let resolveReady!: (ready: BridgeReady | string) => void
  const ready = new Promise<BridgeReady | string>((resolve) => (resolveReady = resolve))
  void readLines(child.stdout, (line) => {
    const parsed = parseBridgeReady(line)
    resolveReady(parsed ?? "hya bridge printed an unexpected readiness line (is --hya an older hya?)")
  })
  child.write(`${link}\n`)

  const stop = async (graceMs = 2_000): Promise<void> => {
    stopping = true
    child.end()
    const timer = setTimeout(() => child.kill("SIGTERM"), graceMs)
    const killer = setTimeout(() => child.kill("SIGKILL"), graceMs + 3_000)
    await child.exited.catch(() => undefined)
    clearTimeout(timer)
    clearTimeout(killer)
  }

  let timer: ReturnType<typeof setTimeout> | undefined
  const outcome = await Promise.race([
    ready,
    child.exited.then(async (code) => {
      // The reason is the child's last stderr line; wait for it to be read.
      await Promise.race([stderrDone, Bun.sleep(500)])
      return `${last ? last.replace(bridgePrefix, "") : `hya bridge exited with code ${code}`}`
    }),
    new Promise<string>((resolve) => { timer = setTimeout(() => resolve(`no answer from the relay within ${Math.round(timeoutMs / 1000)} s${last ? ` (${last.replace(bridgePrefix, "")})` : ""}`), timeoutMs) }),
  ])
  if (timer) clearTimeout(timer)
  if (typeof outcome === "string") {
    await stop(0)
    throw new BridgeError(outcome)
  }
  return {
    ...outcome,
    exited: child.exited,
    get stopping() { return stopping },
    lastLine: () => last,
    stop,
  }
}
