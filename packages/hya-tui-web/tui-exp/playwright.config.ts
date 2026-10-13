import { defineConfig } from "@playwright/test"

process.env.HYA_TUI_DRIVER = "pty"

export default defineConfig({
  testDir: "../e2e",
  testIgnore: ["host.spec.ts"],
  grepInvert: /@browser-only/,
  timeout: 30_000,
  fullyParallel: true,
  workers: Number(process.env.HYA_TUI_EXP_WORKERS ?? 4),
  retries: 0,
  outputDir: process.env.HYA_TUI_EXP_OUTPUT_DIR ?? `${process.env.HOME}/data/hya-rust/tmp/tui-exp-parity`,
  reporter: [["list"], ["./reporter.ts"]],
  projects: [{ name: "pty" }],
})
