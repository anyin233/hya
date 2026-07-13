import { Context } from "effect"
import os from "node:os"
import path from "node:path"

const home = os.homedir()
const data = process.env.XDG_DATA_HOME ?? path.join(home, ".local", "share")
const cache = process.env.XDG_CACHE_HOME ?? path.join(home, ".cache")
const config = process.env.XDG_CONFIG_HOME ?? path.join(home, ".config")
const state = process.env.XDG_STATE_HOME ?? path.join(home, ".local", "state")

export const HyaPaths = {
  home,
  data: path.join(data, "hya"),
  cache: path.join(cache, "hya"),
  config: path.join(config, "hya"),
  state: path.join(state, "hya"),
}

export class HyaPlatform extends Context.Service<HyaPlatform, typeof HyaPaths>()("hya/Platform") {}

const truthy = (key: string) => ["1", "true"].includes(process.env[key]?.toLowerCase() ?? "")

export const HyaFlag = {
  disableMouse: truthy("HYA_DISABLE_MOUSE"),
  disableTerminalTitle: truthy("HYA_DISABLE_TERMINAL_TITLE"),
  disableCopyOnSelect: process.platform === "win32" || truthy("HYA_DISABLE_COPY_ON_SELECT"),
  showTimeToFirstDraw: truthy("HYA_SHOW_TTFD"),
}

export const HyaVersion = process.env.HYA_VERSION ?? "local"
export const HyaChannel = process.env.HYA_CHANNEL ?? "local"
