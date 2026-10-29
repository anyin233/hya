# What does the tui-race CI step check?

`bun run test:tui-race` repeats existing shared real-PTY cases tagged
`@tui-race`. CI reuses the `tui` job's build and runs four workers on at most
two permitted CPUs, with five independent executions per case and no retries.
The initial 11-case selection yields 55 executions. Failures stay failures;
the result is probabilistic scheduling coverage, not a proof of race freedom.

The normal and repeated gates use separate output directories and artifacts.
`parity.json` records `repeatEach`, and each test includes `repeatEachIndex`.
The gate runs even after a baseline terminal failure, unless cancelled.

The Markdown predicate itself previously awaited find() and cell() separately.
It now locates and copies all five required styles inside one synchronous
`Tui.inspect` callback. This closes the observation gap without adding runtime
instrumentation or claiming that every intermediate output frame was checked.

When deriving Playwright configs, spread the base config before replacing
projects. `defineConfig(base, override)` merges differently named projects;
that accidentally selected both pty and tui-race, doubling the gate to 110
executions. The configuration's --list check catches this; the intended
selection is one project and 55 executions.

The exact CI command passed all 55 executions locally on two CPUs in 92.1s.
The generated report was checked for eleven unique scenarios, indices 0–4
for each, one successful attempt per execution, and retry index zero.
WebUI typecheck, its 19 unit tests, and actionlint also pass; invalid repeat
counts fail configuration with a clear error. The prior Markdown-fix CI run
40894996747 completed successfully before adding this new gate.
