# Headless & Automation

You don't need to open an interactive screen to use Hya. From shell scripts, Makefiles, cron jobs, or CI pipelines (like GitHub Actions), Hya has three non-interactive modes depending on how much supervision the job needs:

| Mode | Command | When it stops |
| --- | --- | --- |
| **Single run** | `hya exec "<prompt>"` | After finishing one turn (including any tool calls it makes) |
| **Goal mode** | `hya -p "<goal>"` | When a separate **judge model** agrees the goal is met (or `--max-iterations` runs out) |
| **Loop mode** | `hya loop --target "..." --until "<cmd>"` | When your **shell command exits `0`** and the verifier agrees (or `--budget` runs out) |

---

## 1. One-shot runs (`hya exec`)

`hya exec` sends one prompt, lets Hya read files or run tools until it has an answer, prints the final reply to `stdout`, and exits.

```sh
# Ask a question and print the answer
hya exec "What ports does docker-compose.yml expose?"

# Pipe file contents or logs in on stdin
git diff HEAD~1 | hya exec "Review this diff for obvious bugs"
```

### Saving history vs running in memory

By default, `hya exec` keeps its history in memory and throws it away when the command finishes, so parallel scripts never fight over a database lock. Pass `--db` if you want to keep the session on disk and inspect or resume it later:

```sh
hya --db ./ci-run.db exec "Fix formatting issues in src/"
```

### JSON output for scripts (`--json`)

Pass `--json` to stream one JSON event per line (JSONL) instead of plain text—handy if your script wants to watch tool calls, token counts, or errors as they happen:

```sh
hya exec --json "List all public endpoints" | jq -r 'select(.event.type == "message_finished")'
```

### Bare mode (`hya --bare`)

Want a lean runtime with **zero** built-in bundles (`bundles/builtin/`), the minimal `hya-bare` agent, and only the 8 standard bare tools (`read`, `write`, `edit`, `bash`, `grep`, `todo`, `rewrite`, `skill`) plus built-in `code-mode` (without `agent()`)? Pass the global `--bare` flag on `hya exec`, `hya serve`, or interactive `hya`:

```sh
# Headless bare run (keeps AGENTS.md, disk skills, MCP, and user/project plugins)
hya exec --bare "Fix the failing assertion in tests/math.test.ts"

# Pure kernel benchmark (also skips AGENTS.md, disk skills, MCP, and user/project plugins)
hya exec --bare --pure "Benchmark prompt"

# Bare server or interactive TUI
hya serve --bare --bind 127.0.0.1:8080
hya --bare
```

Because `--bare` uses the unified `SessionEngine`, it still enforces your configured permission rules (pass `--yolo` in disposable containers/sandboxes when unattended execution is desired).

---

## 2. Autopilot with a judge (`hya -p` Goal Mode)

In regular chat tools, an AI sometimes stops early and says *"All done!"* when two files are still broken.

**Goal mode (`hya -p`)** fixes that by separating the **worker** from the **judge**. Goal mode is provided by the built-in **`hya/goal`** native ABI v2 bundle (`GoalDriver` + `goal-contract`, `guided-goal`, and `evaluator-prompt` skills + `hya-goal-guide` and `hya-goal-verifier` agents). After Hya finishes a round of work, an independent evaluator (a registered `goal.evaluate` hook or the tool-free model verifier) checks the transcript before letting Hya stop. If anything is missing, Hya gets sent back to finish the job.

> **Bundle availability:** Goal mode requires `hya/goal` to be enabled (default in `profile: full`). Running `hya --bare -p "..."` or disabling `hya/goal` in `config.yaml` (`bundles.builtin.disable: [hya/goal]`) fails fast with an actionable error instead of falling back to an unverified single turn. Configure the default judge model per bundle in `~/.config/hya/bundles/hya%2Fgoal/config.yml` (`goal.evaluator_model`), or override it per run with `--evaluator-model`.

```sh
hya -p "Add unit tests for src/billing.rs and make sure cargo test passes" \
  --max-iterations 6
```

You can pick a fast or strict model specifically for judging:

```sh
hya -p "Clean up all compiler warnings" \
  --evaluator-model anthropic/claude-sonnet-4-6 \
  --max-iterations 5
```

### Writing rock-solid goals

Plain sentences work fine, or you can write a structured goal file with five clear headings (`## Objective`, `## Success criteria`, `## Verification`, `## Boundaries`, `## Stop conditions`) so the judge knows the exact command Hya must run to prove success:

```sh
hya -p "$(cat <<'EOF'
## Objective
Convert src/config.js to TypeScript.

## Success criteria
- src/config.ts exists with strict types and src/config.js is removed.

## Verification
`npm run typecheck && npm test`

## Boundaries
Do not change public function names.

## Stop conditions
Stop if a third-party dependency lacks type definitions.
EOF
)" --max-iterations 6
```

---

## 3. Autopilot gated by a shell check (`hya loop`)

When you already have a test suite, typechecker, or linter that tells you pass/fail via exit code, **`hya loop`** is the most reliable way to run unattended work.

- **`--until '<cmd>'`** keeps looping until `<cmd>` exits with code `0` (and exits `1` while work remains).
- **`--while '<cmd>'`** does the opposite—keeps looping while `<cmd>` exits `0`, and stops when it exits `1`.

```sh
hya loop \
  --target "Make all workspace clippy lints and unit tests pass" \
  --until "cargo clippy --workspace -- -D warnings && cargo test --workspace" \
  --budget 8
```

> **Safety guard:** In `--until` / `--while`, exit codes `0` and `1` mean "met" or "not met yet." Any other exit code (like `127` command not found or a syntax error in your check command) stops the loop immediately with an error instead of burning through your token budget on a broken check!

### Modular `hya/loop` bundle (`LoopDriver`)

Loop mode is supplied by the built-in native ABI v2 bundle **`hya/loop`** (`bundles/builtin/loop/`), which registers its `LoopDriver` (`id: "loop"`), lifecycle `LoopService`, and verifier/planner prompt skills (`loop-verifier-prompt`, `loop-planner-prompt`) through `HostRuntime`.

- **Full mode (default):** `hya/loop` is enabled automatically and `hya loop` dispatches through `HostRuntime::loop_driver()`.
- **`--bare` or disabled:** Running `hya loop --bare` or listing `hya/loop` in `bundles.builtin.disable` skips loading the bundle and returns an actionable error instructing you to remove `--bare` or enable `hya/loop` in `config.yaml`:

```yaml
schema_version: 2
bundles:
  builtin:
    profile: bare
    enable: [hya/loop]
```

---

## Permissions in headless runs

Remember that Hya checks permissions in headless commands (including `--bare` unless you pass `--yolo`). When running unattended inside a disposable CI container or VM where no human is sitting at the keyboard to press `1`, pass `--yolo` or configure explicit `allow` rules in `~/.config/hya/config.yaml` (see **[Projects & Permissions](projects-and-permissions.md)**) so Hya can run your build and test commands without waiting.
