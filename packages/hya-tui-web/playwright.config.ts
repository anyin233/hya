import { defineConfig, devices } from "@playwright/test"

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: true,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    ...devices["Desktop Chrome"],
    viewport: { width: 1100, height: 640 },
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium" }],
})
