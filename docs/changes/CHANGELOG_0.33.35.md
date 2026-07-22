# 0.33.35

- Fix session resume: owned `hya` backends now open a durable SQLite store at `$XDG_STATE_HOME/hya/sessions.db` (override with `HYA_DB`, empty for in-memory) so `hya --continue` / `hya -c` and `hya -s <id>` can restore prior sessions after restart.
- Interactive bare `hya-backend` and default `sessions`/`tail-session` paths use the same store when `--db` is omitted.
- Add short flags `-c` (`--continue`) and `-s` (`--session`) on the public `hya` launcher.
