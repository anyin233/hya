# Context, Memory & Compaction

AI models have a limited **context window**—think of it as the size of the desk Hya works on. Every file Hya reads, every command output, and every message takes up space on that desk (shown in the bottom status bar as `ctx ━━─── 42%`).

This guide explains how to give Hya permanent project knowledge (`AGENTS.md`) and how Hya automatically cleans off its desk during long sessions so it never crashes with a "context limit exceeded" error.

---

## 1. Project instructions (`AGENTS.md`)

Instead of pasting the same coding style, build commands, and folder conventions into every new chat, put an **`AGENTS.md`** file in your repository.

When a chat starts, Hya walks from your home config down to your project folder and loads every instructions file it finds (parent folders first, project folder last):

1. Global user instructions (`~/.config/hya/AGENTS.md`)
2. Any `AGENTS.md` in parent folders above your project
3. `<project-root>/AGENTS.md` (across all roots of a multi-folder Project)

### Tips for a great `AGENTS.md`

Keep it short and actionable—50 to 150 lines is ideal. Focus on things Hya would otherwise have to guess by trial and error:

```markdown
# Build & Test
- Build: `cargo build`
- Test a single package: `cargo test -p my-crate`

# Code Style
- Never commit `.env` files or change database migrations that already shipped.
- Put shared API types in `packages/shared/src/types.ts`.
```

- **Generate one automatically:** Run **`/init`** inside Hya and let Hya inspect your repo and draft an `AGENTS.md` for you.
- **See what loaded:** Check the `[Context]` line at the top of a new chat (or run `/resources`).
- **Reload after editing:** Hya snapshots `AGENTS.md` on the first turn of a chat so provider prompt caches stay fast and cheap. If you edit `AGENTS.md` mid-session, run **`/reload-context`** to pick up your new text on the next turn.

### Sharing instructions across multiple folders

Working across a monorepo or a separate shared docs repo? Add that second folder as an extra root on your Project in **`/project`** (press `e` to edit roots)—Hya reads `AGENTS.md` from every root in the Project and lets file tools read across them without extra prompts.

---

## 2. Keeping Hya on track with the `todo` list

During multi-step tasks, Hya keeps a structured **session todo list** (`todo` tool). You can see open items in the sidebar, above the dock, or anytime by running **`/todos`**.

Why does the todo list matter? Because even when a huge conversation gets trimmed down to free up space, **Hya always preserves your original messages and the active todo list**, so it never forgets what is finished and what is still left to do.

---

## 3. Automatic compaction (when `ctx` hits 80%)

You never have to start a new chat just because the context bar gets full. Before each round, if the conversation uses **more than 80% of the model's context window**, Hya automatically shrinks it in three steps, starting with the gentlest one:

1. **Step 1 — Shelve old tool outputs (`shake`):** Big file reads and build logs from earlier steps are moved out of the active prompt into local storage (`artifact://...`) and replaced with a one-line note. You still see the full tool outputs in your chat window, and if Hya ever needs one again, it can `read` that `artifact://` link! Usually this drops usage right back down with zero loss of reasoning.
2. **Step 2 — Provider-native compaction (`remote`):** If the chat is still over 80% and your provider supports native window folding (such as OpenAI Responses / Codex / Grok Build), Hya asks the provider to fold the older history cleanly.
3. **Step 3 — Clean slate with memory (`Rewrite`):** On providers without native folding (or in `--bare` mode), Hya clears the old back-and-forth steps before the current turn while **keeping all of your user prompts verbatim plus the current `todo` list** at the top of the window. Everything stays saved in your local session log on disk.

---

## 4. Shrinking the chat manually (`/compact` & `/summarize`)

Finishing one task and moving on to a related one in the same session? You can free up context space anytime without waiting for the 80% mark:

- **`/compact`** or **`/summarize`** folds earlier turns right away (using provider-native compaction if available, or Hya's clean `Rewrite` marker otherwise).
- **`/new`** starts a completely fresh session when you are switching to an unrelated topic.

### Optional tuning

Most people never need to touch these settings. How many recent messages/steps `shake` keeps untouched before shelving older tool outputs lives in the compaction bundle's own file, `~/.config/hya/bundles/hya%2Fcompaction/config.yml` (env `HYA_COMPACTION_KEEP_RECENT` wins):

```yaml
compaction:
  keep_recent: 6           # Keep the last 6 recent messages/tool steps intact (default: 6)
```

How Hya measures how full the window is belongs to the core, in `~/.config/hya/config.yaml` (env `HYA_TOKEN_ACCOUNTING` wins):

```yaml
context:
  token_accounting: auto   # auto | provider | estimate
```
