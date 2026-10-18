# 0.44.23-dev.5

## Terminal title

- Name the terminal tab after the open session and its Project, with a marker for what it needs: `●` a turn is running, `?` a permission or question ask is waiting, `✓`/`✗` a turn ended while the terminal was unfocused (cleared on focus). The terminal's own title is restored on exit.
- The WebUI's browser tab title follows the terminal title.

## Subagent viewer scrolling

- With the Subagents selector focused, PgUp/PgDn scroll the subagent viewer it previews in (the one Enter pins to), and Home/End jump to its top and newest line. The parent's transcript stays put; with the message editor focused, PgUp/PgDn still scroll the parent's transcript.

## Cursor

- The message editor and the command pane show a blinking bar cursor instead of a block. Vim mode still uses a block in normal mode.

## Subagent control

- In the Subagents selector, `x` interrupts the selected subagent's turn, `s` opens `/subagents steer <session> ` to send it a message, and `d` archives it after Enter confirms. The same actions are `/subagents interrupt|steer|archive <session>`. Interrupting the parent with Esc still leaves its subagents running.
- Requires backend 0.45.9 or later.

## Fixes

- A scrolled-up transcript no longer jumps to the bottom while a turn streams. Each session-list refresh (about every 1.5 s while a subagent or another session is busy) was treated as opening the session again.
