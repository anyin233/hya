import { defineConfig, devices } from "@playwright/test"

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: true,
  // One retry on CI absorbs runner hiccups; a test that only passes on retry
  // is still listed as flaky in the report.
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    ...devices["Desktop Chrome"],
    viewport: { width: 1100, height: 640 },
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium" }],
})
