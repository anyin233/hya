/**
 * Read-only file access for extensions (`api.fs`, permission `fs.read`;
 * docs/tui-extensions.md "File access"): the shared extension host relays an
 * extension's `fs/read`, `fs/list`, `fs/stat`, `fs/watch`, `fs/unwatch` here.
 * Every path must resolve, symlinks followed, inside a root of the active
 * Project; reads are bounded, listings capped, watches limited per extension
 * and batched. All I/O is asynchronous: the TUI thread never blocks on it.
 */
import { watch as watchPath, type FSWatcher } from "node:fs"
import { readdir, readFile, realpath, stat } from "node:fs/promises"
import { isAbsolute, join, relative, resolve, sep } from "node:path"

export const maxReadBytes = 1024 * 1024
export const maxListEntries = 2_000
export const maxWatches = 16
export const watchBatchMs = 100
export const maxBatchEvents = 100

export interface FsEvent { readonly path: string; readonly kind: "change" | "rename" | "closed" }
type EntryKind = "file" | "dir" | "other"

interface Watch {
  readonly ext: string
  readonly id: number
  /** The watched path as the extension named it, for `closed` events. */
  readonly path: string
  readonly real: string
  readonly watcher: FSWatcher
  batch: FsEvent[]
  timer?: Timer
}

export class FsAccessError extends Error {
  override name = "FsAccessError"
}

const kindOf = (entry: { isFile(): boolean; isDirectory(): boolean }): EntryKind => entry.isFile() ? "file" : entry.isDirectory() ? "dir" : "other"

/** `child` is `root` or inside it (both real paths). */
const inside = (root: string, child: string): boolean => {
  const rest = relative(root, child)
  return rest === "" || (!rest.startsWith("..") && !isAbsolute(rest) && !rest.startsWith(`..${sep}`))
}

/**
 * The real path of `path`, symlinks followed; for a path that does not exist
 * (yet), the real path of its nearest existing ancestor joined with the rest,
 * so a missing file under a symlinked directory still resolves where it would be.
 */
async function realOrAncestor(path: string): Promise<string> {
  const rest: string[] = []
  let current = path
  for (;;) {
    try {
      return join(await realpath(current), ...rest.reverse())
    } catch (error) {
      const parent = resolve(current, "..")
      if (parent === current || (error instanceof Error && "code" in error && error.code !== "ENOENT")) throw new FsAccessError(`${path}: ${error instanceof Error ? error.message : String(error)}`)
      rest.push(relative(parent, current))
      current = parent
    }
  }
}

export class ExtensionFiles {
  /** The active Project's roots as given, the first being primary, and their real paths. */
  private roots: readonly string[] = []
  private realRoots: Promise<readonly string[]> = Promise.resolve([])
  private readonly watches = new Map<string, Watch>()

  /** `deliver` hands a batch of watch events to the extension (`tui/fs_event`). */
  constructor(private readonly deliver: (ext: string, watch: number, events: readonly FsEvent[]) => void) {}

  /** The active Project changed: later calls resolve against these roots; watches outside them close. */
  setRoots(roots: readonly string[]): void {
    if (roots.length === this.roots.length && roots.every((root, index) => root === this.roots[index])) return
    this.roots = roots
    this.realRoots = Promise.all(roots.map((root) => realpath(root).catch(() => resolve(root))))
    void this.realRoots.then((real) => {
      for (const watch of [...this.watches.values()]) if (!real.some((root) => inside(root, watch.real))) this.close(watch, true)
    })
  }

  /** The real path of `path` (relative to the primary root), refused unless it lies inside a root. */
  private async resolvePath(path: string): Promise<string> {
    const primary = this.roots[0]
    if (!primary) throw new FsAccessError("no Project is open")
    if (typeof path !== "string" || !path || path.includes("\0")) throw new FsAccessError("path required")
    const real = await realOrAncestor(isAbsolute(path) ? resolve(path) : resolve(primary, path))
    if (!(await this.realRoots).some((root) => inside(root, real))) throw new FsAccessError("outside the Project roots")
    return real
  }

  async read(path: string): Promise<{ text: string }> {
    const real = await this.resolvePath(path)
    const info = await stat(real).catch(() => { throw new FsAccessError(`${path}: not found`) })
    if (!info.isFile()) throw new FsAccessError(`${path}: not a file`)
    if (info.size > maxReadBytes) throw new FsAccessError("file larger than 1 MiB")
    return { text: await readFile(real, "utf8") }
  }

  async list(path: string): Promise<{ entries: { name: string; kind: EntryKind }[] }> {
    const real = await this.resolvePath(path)
    const entries = await readdir(real, { withFileTypes: true }).catch(() => { throw new FsAccessError(`${path}: not a directory`) })
    return {
      entries: entries.map((entry) => ({ name: entry.name, kind: kindOf(entry) }))
        .sort((left, right) => left.name.localeCompare(right.name))
        .slice(0, maxListEntries),
    }
  }

  async stat(path: string): Promise<{ kind: EntryKind; size: number; mtime: number } | null> {
    const real = await this.resolvePath(path)
    const info = await stat(real).catch(() => undefined)
    return info ? { kind: kindOf(info), size: info.size, mtime: info.mtimeMs } : null
  }

  async watch(ext: string, id: number, path: string, recursive: boolean): Promise<null> {
    if (!Number.isSafeInteger(id)) throw new FsAccessError("watch id required")
    const real = await this.resolvePath(path)
    // Counted after the await: concurrent watch requests must not all pass the check.
    if ([...this.watches.values()].filter((watch) => watch.ext === ext).length >= maxWatches) throw new FsAccessError(`at most ${maxWatches} watches`)
    const key = `${ext}\0${id}`
    if (this.watches.has(key)) throw new FsAccessError(`watch ${id} exists`)
    let watcher: FSWatcher
    try {
      watcher = watchPath(real, { recursive, persistent: false })
    } catch (error) {
      throw new FsAccessError(`${path}: ${error instanceof Error ? error.message : String(error)}`)
    }
    const entry: Watch = { ext, id, path, real, watcher, batch: [] }
    watcher.on("change", (eventType, filename) => {
      if (entry.batch.length >= maxBatchEvents) return
      const name = typeof filename === "string" ? filename : filename?.toString() ?? ""
      entry.batch.push({ path: name ? join(path, name) : path, kind: eventType === "rename" ? "rename" : "change" })
      entry.timer ??= setTimeout(() => this.flush(entry), watchBatchMs)
    })
    // A watched path that disappears or errors ends the watch.
    watcher.on("error", () => this.close(entry, true))
    this.watches.set(key, entry)
    return null
  }

  unwatch(ext: string, id: number): null {
    const watch = this.watches.get(`${ext}\0${id}`)
    if (watch) this.close(watch, false)
    return null
  }

  /** The extension is gone: its watches close without telling it. */
  release(ext: string): void {
    for (const watch of [...this.watches.values()]) if (watch.ext === ext) this.close(watch, false)
  }

  private flush(watch: Watch): void {
    watch.timer = undefined
    const events = watch.batch
    watch.batch = []
    if (events.length) this.deliver(watch.ext, watch.id, events)
  }

  private close(watch: Watch, tell: boolean): void {
    clearTimeout(watch.timer)
    watch.watcher.close()
    this.watches.delete(`${watch.ext}\0${watch.id}`)
    if (tell) this.deliver(watch.ext, watch.id, [{ path: watch.path, kind: "closed" }])
  }
}
