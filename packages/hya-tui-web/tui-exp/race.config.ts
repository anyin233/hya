import { defineConfig } from "@playwright/test"
import terminalConfig from "./playwright.config"

function positiveInteger(name: string, fallback: number): number {
  const value = Number(process.env[name] ?? fallback)
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be a positive integer`)
  return value
}

/** Repeat the shared regression scenarios without retrying failed attempts. */
export default defineConfig({
  ...terminalConfig,
  grep: /@tui-race/,
  repeatEach: positiveInteger("HYA_TUI_RACE_REPEAT", 5),
  workers: positiveInteger("HYA_TUI_RACE_WORKERS", 4),
  retries: 0,
  outputDir: process.env.HYA_TUI_RACE_OUTPUT_DIR ?? `${process.env.HOME}/data/hya-rust/tmp/tui-race-results`,
  projects: [{ name: "tui-race" }],
})
