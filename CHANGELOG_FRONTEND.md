# 0.44.23-dev.6

## Semantic colors

- Reserve the blue focus color for focused borders, input cursors, and highlighted choices. Window and Markdown titles and assistant identities now use neutral heading text; window titles do not inherit a focused border color.
- Simplify code highlighting to purple keywords, warm-gold literals, neutral names/operators, and gray comments; make diff location headers neutral.
- Use green for successful results, amber for attention/waiting, red for errors, and neutral activity spinners. Links use an underline; current-value markers and shortcuts no longer paint unrelated rows blue.
- Expose semantic theme tokens to TUI extensions, retaining `accent` as a compatibility alias for `focus`. First-party panes follow the same roles. Requires backend 0.45.10 or later.

## Terminal title

- Name the terminal tab after the open session and its Project, with a marker for what it needs: `●` a turn is running, `?` a permission or question ask is waiting, `✓`/`✗` a turn ended while the terminal was unfocused (cleared on focus). The terminal's own title is restored on exit.
- The WebUI's browser tab title follows the terminal title.

## Subagent viewer scrolling

- With the Subagents selector focused, PgUp/PgDn scroll the subagent viewer it previews in (the one Enter pins to), and Home/End jump to its top and newest line. The parent's transcript stays put; with the message editor focused, PgUp/PgDn still scroll the parent's transcript.

## Cursor

- The message editor and the command pane show a blinking bar cursor instead of a block. Vim mode still uses a block in normal mode.

## Subagent control

- In the Subagents selector, `x` interrupts the selected subagent's turn, `s` opens `/subagents steer <session> ` to send it a message, and `d` archives it after Enter confirms. The same actions are `/subagents interrupt|steer|archive <session>`. Interrupting a subagent also stops its own subagents, and they stay stopped until you steer them.
- Esc and `/cancel` now stop every subagent along with the parent, and nothing restarts them until you send a message or steer one. With the parent's turn already over, Esc in an empty editor stops the subagents that are still working (with a draft typed, the first Esc clears it).
- Requires backend 0.45.9 or later.

## Fixes

- The Subagents selector no longer shows a working subagent as idle when its turn started just after the TUI last polled it. It now uses the session list's live busy flag.
- A scrolled-up transcript no longer jumps to the bottom while a turn streams. Each session-list refresh (about every 1.5 s while a subagent or another session is busy) was treated as opening the session again.
