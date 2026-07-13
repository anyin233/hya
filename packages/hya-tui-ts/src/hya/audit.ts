import { HyaPaths } from "./platform"
import {
  CLIPBOARD_TEMP_NAME,
  DEFAULT_SOUND_PACK,
  DEFAULT_THEME,
  PRODUCT_NAME,
  STATUS_COMMAND,
  terminalTitle,
} from "./product"
import { BUILTIN_IDS } from "../upstream/feature-plugins/builtins"

export const auditSurface = {
  product: PRODUCT_NAME,
  presentation: {
    logo: PRODUCT_NAME,
    home: PRODUCT_NAME,
    session: PRODUCT_NAME,
    permission: PRODUCT_NAME,
    question: PRODUCT_NAME,
    status: PRODUCT_NAME,
    help: PRODUCT_NAME,
    error: PRODUCT_NAME,
    config: PRODUCT_NAME,
    state: PRODUCT_NAME,
    temp: PRODUCT_NAME,
    theme: PRODUCT_NAME,
    sound: PRODUCT_NAME,
  },
  terminalTitle,
  defaultTheme: DEFAULT_THEME,
  defaultSoundPack: DEFAULT_SOUND_PACK,
  paths: [HyaPaths.data, HyaPaths.cache, HyaPaths.config, HyaPaths.state],
  tempName: CLIPBOARD_TEMP_NAME,
  staticBuiltins: BUILTIN_IDS,
  commands: [STATUS_COMMAND],
} as const
