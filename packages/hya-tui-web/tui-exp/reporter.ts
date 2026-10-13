import type { FullConfig, FullResult, Reporter, Suite } from "@playwright/test/reporter"
import { mkdir, writeFile } from "node:fs/promises"
import { join, relative } from "node:path"

/** Audit every shared scenario, with explicit browser-only exclusions. */
export default class ParityReporter implements Reporter {
  private suite?: Suite
  private config?: FullConfig
  onBegin(config: FullConfig, suite: Suite) { this.config = config; this.suite = suite }
  async onEnd(result: FullResult) {
    const config = this.config!
    const tests = this.suite!.allTests().map((test) => ({
      file: relative(config.rootDir, test.location.file),
      line: test.location.line,
      title: test.titlePath().slice(2).join(" > "),
      browserOnly: test.tags.includes("@browser-only"),
      outcome: test.outcome(),
      runs: test.results.map((run) => ({ status: run.status, durationMs: run.duration, retry: run.retry, errors: run.errors.map((error) => error.message) })),
    }))
    const directory = config.projects[0]!.outputDir
    await mkdir(directory, { recursive: true })
    await writeFile(join(directory, "parity.json"), JSON.stringify({
      driver: "real-pty-xterm-headless",
      status: result.status,
      durationMs: result.duration,
      workers: config.workers,
      selected: tests.length,
      terminalScenarios: tests.filter((test) => !test.browserOnly).length,
      browserOnlyExcludedTag: "@browser-only",
      browserOnlyFiles: [{ file: "host.spec.ts", reason: "Generic WebUI host exit notice and browser reconnection/process spawning. Direct child exit/cleanup and distinct processes have PTY protocol coverage." }],
      tests,
    }, null, 2))
  }
}
