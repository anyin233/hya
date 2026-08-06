import { Context } from "effect"
import os from "node:os"
import path from "node:path"

const home = os.homedir()
const data = process.env.XDG_DATA_HOME ?? path.join(home, ".local", "share")
const cache = process.env.XDG_CACHE_HOME ?? path.join(home, ".cache")
const config = process.env.XDG_CONFIG_HOME ?? path.join(home, ".config")
const state = process.env.XDG_STATE_HOME ?? path.join(home, ".local", "state")

/**
 * Resolved filesystem roots for hya under XDG (or home-relative) base directories.
 *
 * Each path is `…/hya` under the corresponding XDG_* base (or a default under `$HOME`).
 */
export const HyaPaths = {
  home,
  data: path.join(data, "hya"),
  cache: path.join(cache, "hya"),
  config: path.join(config, "hya"),
  state: path.join(state, "hya"),
}

/**
 * Effect service tag for injecting {@link HyaPaths} into the TUI runtime.
 */
export class HyaPlatform extends Context.Service<HyaPlatform, typeof HyaPaths>()("hya/Platform") {}

const truthy = (key: string) => ["1", "true"].includes(process.env[key]?.toLowerCase() ?? "")

/**
 * Process-level feature flags read once at module load from environment variables.
 *
 * Truthy values are the strings `1` or `true` (case-insensitive).
 */
export const HyaFlag = {
  /** When true, disable mouse input handling. Env: `HYA_DISABLE_MOUSE`. */
  disableMouse: truthy("HYA_DISABLE_MOUSE"),
  /** When true, do not update the terminal window title. Env: `HYA_DISABLE_TERMINAL_TITLE`. */
  disableTerminalTitle: truthy("HYA_DISABLE_TERMINAL_TITLE"),
  /** When true (or always on win32), disable copy-on-select. Env: `HYA_DISABLE_COPY_ON_SELECT`. */
  disableCopyOnSelect: process.platform === "win32" || truthy("HYA_DISABLE_COPY_ON_SELECT"),
  /** When true, show time-to-first-draw diagnostics. Env: `HYA_SHOW_TTFD`. */
  showTimeToFirstDraw: truthy("HYA_SHOW_TTFD"),
  /** Classic: await terminal theme mode (up to 1s) before first paint. Default is instant dark. */
  waitThemeMode: truthy("HYA_WAIT_THEME"),
  /** Classic: gate shell routes on sequential builtin plugin host start. Default paints shell immediately. */
  syncPluginStart: truthy("HYA_SYNC_PLUGIN_START"),
}

/** Product version string from `HYA_VERSION`, or `"local"` when unset. */
export const HyaVersion = process.env.HYA_VERSION ?? "local"

/** Release channel string from `HYA_CHANNEL`, or `"local"` when unset. */
export const HyaChannel = process.env.HYA_CHANNEL ?? "local"
