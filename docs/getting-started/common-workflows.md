# Common Workflows

Once Hya is up and running, most daily work falls into a handful of simple patterns. Here are copy-paste recipes you can use right away.

---

## 1. Understand an unfamiliar codebase

Open `hya` at the root of a project you just cloned and ask broad questions first, then drill down:

```text
Give me a plain-English tour of this repo. Where does the app start, and where is the main business logic?
```

Follow up by pointing at specific files with `@`:

```text
Trace what happens step by step when a user logs in starting from @src/auth/login.ts
```

> **Tip:** Hya only reads files here, so it won't touch anything on disk. If you want a strictly read-only planner first, switch to the built-in plan agent with `/agent hya-plan`.

---

## 2. Fix a bug and make the tests pass

Paste the error message (or screenshot!) right into the chat. Long pastes fold neatly into a `[paste]` chip so your screen stays clean, and clipboard screenshots paste with `Ctrl+V` (`Cmd+V` in the WebUI):

```text
Users get a 500 error when submitting an empty cart. Find the bug, fix it, and run the cart tests to prove it's fixed.
```

While Hya is working:
- You can **type your next message while it's still running** and press **Enter**—Hya queues your message and sends it as soon as the current turn finishes (or steers the agent).
- When it finishes, run `/diff` to review every changed file before committing.
- Run `!git status` or `!npm test` with a leading `!` to run quick shell checks yourself without spending AI tokens.

---

## 3. Try two different approaches with `/fork` and `/undo`

Not sure whether to solve a problem one way or another?

1. Ask Hya to analyze the bug first.
2. Run `/fork` and press **Enter** on `Fork at the latest message`. Hya duplicates the chat into a new session so your original conversation stays untouched.
3. If Hya goes down a dead end, run `/undo` to roll back both the message and the file edits, tweak your prompt (it is already waiting back in your input box!), and hit **Enter** again.

---

## 4. Teach Hya your project's rules (`AGENTS.md`)

Tired of reminding Hya to use `pnpm` instead of `npm`, or to run a specific linter? Put an `AGENTS.md` file in your project root (or run `/init` inside Hya to generate a starter one):

```markdown
# Project Rules

- Package manager: `pnpm` (never run `npm` or `yarn`).
- Run `pnpm test` after changing anything under `src/`.
- Keep API handlers in `src/routes/` and database queries in `src/db/`.
```

Every chat inside that project reads `AGENTS.md` automatically. If you edit `AGENTS.md` while a chat is already open, run `/reload-context` so the open chat picks up your changes on the next turn.

---

## 5. Hand off a slow job to autopilot (`-p` and `loop`)

When a task takes several rounds of "edit → run tests → fix remaining errors," you don't have to babysit it in the chat window.

### Goal mode (`hya -p`)

Hya works on your goal, and after each round a separate **judge model** checks whether the goal is genuinely done:

```sh
hya -p "Migrate all files in src/utils/ from CommonJS to TypeScript and make the build pass" \
  --max-iterations 8
```

### Loop mode (`hya loop`)

Even stricter: Hya keeps iterating until an actual **shell command exits `0`**:

```sh
hya loop \
  --target "Fix all TypeScript type errors across the workspace" \
  --until "pnpm run typecheck" \
  --budget 6
```

Hya never takes the AI's word for it—if `pnpm run typecheck` still fails, the loop keeps going (up to `--budget` rounds).

---

## 6. Use Hya in shell scripts and CI (`hya exec`)

Need a quick answer in the terminal or inside a GitHub Actions workflow? `hya exec` runs one turn, prints the answer, and exits:

```sh
# Plain text answer on stdout
hya exec "Summarize the changes in the last 3 git commits"

# Machine-readable JSON Lines stream
hya exec --json "List any hardcoded secrets or TODOs in src/"
```

---

## 7. Control your desktop from your laptop

Leaving your desk while your workstation has your code, GPUs, and environment set up?

1. On a small cloud server or Tailscale node, start the blind relay once:
   ```sh
   hya proxy --port 8766
   ```
2. On your desktop machine, attach your Hya server to that relay:
   ```sh
   hya serve start --relay http://your-relay-host:8766
   ```
   Copy the `hya://...` secret link it prints.
3. On your laptop, connect from anywhere:
   ```sh
   hya --connect -
   ```
   Paste the link when asked. Your laptop opens the exact same TUI and WebUI, end-to-end encrypted—the relay server in the middle cannot read a single byte of your code or prompts.

See **[Daemon & Remote Relay](../guide/daemon-and-relay.md)** for full setup details.
