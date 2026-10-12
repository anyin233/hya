# How many workers should terminal CI use?

Use eight workers in CI, four by default locally. In a 46-case sessions/tools/subagents/Vim sample pinned to four CPUs, four workers took 49.817s (clean repeat), eight took 38.663s; all passed. Median individual latency increased from about three to five seconds, so this is throughput improvement rather than faster individual tests.

After fixing readiness races exposed by load, all 307 shared scenarios passed at eight workers/four CPUs in 249.193s with no retries. The three additional race cases also passed 15/15 stress repetitions. A 90-second process sample during the first full run peaked at 3400 MiB aggregate RSS (shared pages may be counted repeatedly), 43 processes. This is local evidence, not a guarantee about GitHub hardware or future memory peaks.

Set HYA_TUI_EXP_WORKERS for direct parity runs; scripts/check-tui.sh uses HYA_TUI_TEST_WORKERS. Keep build/test artifacts under ~/data locally. Full reports: /home/zcm/data/hya-rust/tmp/workers-full-8-final/parity.json and workers-bench-{4-clean,8}/parity.json.
